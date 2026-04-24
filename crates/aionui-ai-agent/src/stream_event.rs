use agent_client_protocol::schema::{
    ContentBlock, SessionNotification, SessionUpdate, ToolCallStatus as SdkToolCallStatus,
};
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Events emitted by an Agent during a message processing turn.
///
/// These are parsed from Agent stdout (line-delimited JSON) and forwarded
/// to the WebSocket layer as `message.stream` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentStreamEvent {
    /// Start of a new response turn.
    Start(StartEventData),
    /// Incremental text content.
    Text(TextEventData),
    /// Tip / notification (error, success, warning).
    Tips(TipsEventData),
    /// Single tool call status update.
    ToolCall(ToolCallEventData),
    /// Group of tool calls.
    ToolGroup(Vec<ToolGroupEntry>),
    /// Agent status change (backend, status, session info).
    AgentStatus(AgentStatusEventData),
    /// Thinking / reasoning trace.
    Thinking(ThinkingEventData),
    /// Execution plan.
    Plan(PlanEventData),
    /// ACP permission request (tool approval).
    AcpPermission(serde_json::Value),
    /// ACP tool call progress.
    AcpToolCall(serde_json::Value),
    /// Codex permission request.
    CodexPermission(serde_json::Value),
    /// Codex tool call progress.
    CodexToolCall(serde_json::Value),
    /// Available slash commands update.
    AvailableCommands(AvailableCommandsEventData),
    /// Skill suggestion from cron job.
    SkillSuggest(SkillSuggestEventData),
    /// Cron trigger notification.
    CronTrigger(CronTriggerEventData),
    /// ACP model info update.
    AcpModelInfo(serde_json::Value),
    /// ACP context usage info.
    AcpContextUsage(serde_json::Value),
    /// Response finished.
    Finish(FinishEventData),
    /// Error during processing.
    Error(ErrorEventData),
    /// System-level message from ACP.
    System(serde_json::Value),
    /// Raw request trace (ACP debug info).
    RequestTrace(serde_json::Value),
    /// Slash commands updated notification.
    SlashCommandsUpdated(serde_json::Value),
}

/// Data for the `Start` event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StartEventData {
    /// Session ID for this turn (if available).
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Data for the `Text` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextEventData {
    /// Incremental text content.
    pub content: String,
}

/// Data for the `Tips` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TipsEventData {
    /// Tip message content.
    pub content: String,
    /// Severity level.
    #[serde(rename = "type")]
    pub tip_type: TipType,
}

/// Severity level for a tip event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TipType {
    Error,
    Success,
    Warning,
}

/// Data for the `ToolCall` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallEventData {
    pub call_id: String,
    pub name: String,
    #[serde(default)]
    pub args: serde_json::Value,
    pub status: ToolCallStatus,
}

/// Status of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Running,
    Completed,
    Error,
}

/// A single entry in a `ToolGroup` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolGroupEntry {
    pub call_id: String,
    pub name: String,
    pub status: ToolCallStatus,
    #[serde(default)]
    pub description: Option<String>,
}

/// Data for the `AgentStatus` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatusEventData {
    pub backend: String,
    pub status: String,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Data for the `Thinking` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingEventData {
    pub content: String,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub duration: Option<u64>,
    #[serde(default)]
    pub status: Option<String>,
}

/// Data for the `Plan` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEventData {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub entries: Vec<serde_json::Value>,
}

/// Data for the `AvailableCommands` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableCommandsEventData {
    pub commands: Vec<serde_json::Value>,
}

/// Data for the `SkillSuggest` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSuggestEventData {
    #[serde(default)]
    pub cron_job_id: Option<String>,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub skill_content: Option<String>,
}

/// Data for the `CronTrigger` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronTriggerEventData {
    pub cron_job_id: String,
    pub cron_job_name: String,
    pub triggered_at: i64,
}

/// Data for the `Finish` event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FinishEventData {
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Data for the `Error` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEventData {
    pub message: String,
    #[serde(default)]
    pub code: Option<String>,
}

// ── SDK SessionNotification → AgentStreamEvent conversion ────────────────────

/// Convert an SDK [`SessionNotification`] into zero or more [`AgentStreamEvent`]s.
///
/// Each `SessionUpdate` variant is mapped to the closest existing event type.
/// Unknown or unmappable variants produce a debug log and are skipped (not
/// silently swallowed, not panicked).
pub fn session_notification_to_events(notif: &SessionNotification) -> Vec<AgentStreamEvent> {
    let session_id = notif.session_id.to_string();
    let mut events = Vec::new();

    match &notif.update {
        SessionUpdate::AgentMessageChunk(chunk) => {
            if let ContentBlock::Text(text) = &chunk.content {
                events.push(AgentStreamEvent::Text(TextEventData {
                    content: text.text.clone(),
                }));
            }
        }

        SessionUpdate::AgentThoughtChunk(chunk) => {
            if let ContentBlock::Text(text) = &chunk.content {
                events.push(AgentStreamEvent::Thinking(ThinkingEventData {
                    content: text.text.clone(),
                    subject: None,
                    duration: None,
                    status: Some("in_progress".into()),
                }));
            }
        }

        SessionUpdate::UserMessageChunk(_chunk) => {
            // User message echoes are not forwarded to the event stream.
            // The frontend already has the user's message.
        }

        SessionUpdate::ToolCall(tc) => {
            let status = map_sdk_tool_status(&tc.status);
            events.push(AgentStreamEvent::ToolCall(ToolCallEventData {
                call_id: tc.tool_call_id.to_string(),
                name: tc.title.clone(),
                args: tc.raw_input.clone().unwrap_or_default(),
                status,
            }));
        }

        SessionUpdate::ToolCallUpdate(tcu) => {
            let status = tcu
                .fields
                .status
                .as_ref()
                .map(map_sdk_tool_status)
                .unwrap_or(ToolCallStatus::Running);

            events.push(AgentStreamEvent::ToolCall(ToolCallEventData {
                call_id: tcu.tool_call_id.to_string(),
                name: tcu.fields.title.clone().unwrap_or_default(),
                args: tcu.fields.raw_input.clone().unwrap_or_default(),
                status,
            }));
        }

        SessionUpdate::Plan(plan) => {
            let entries: Vec<serde_json::Value> = plan
                .entries
                .iter()
                .map(|e| serde_json::to_value(e).unwrap_or_default())
                .collect();

            events.push(AgentStreamEvent::Plan(PlanEventData {
                session_id: Some(session_id),
                entries,
            }));
        }

        SessionUpdate::AvailableCommandsUpdate(update) => {
            let commands: Vec<serde_json::Value> = update
                .available_commands
                .iter()
                .map(|c| serde_json::to_value(c).unwrap_or_default())
                .collect();

            events.push(AgentStreamEvent::AvailableCommands(
                AvailableCommandsEventData { commands },
            ));
        }

        SessionUpdate::CurrentModeUpdate(_update) => {
            // Mode changes are tracked internally by acp_agent.rs.
            // The frontend receives mode info through the mode API endpoint.
            debug!("SessionUpdate::CurrentModeUpdate received, not forwarded to event stream");
        }

        SessionUpdate::ConfigOptionUpdate(_update) => {
            // Config changes are tracked internally.
            debug!("SessionUpdate::ConfigOptionUpdate received, not forwarded to event stream");
        }

        SessionUpdate::SessionInfoUpdate(_update) => {
            // Session info updates (title changes etc.) are not forwarded.
            debug!("SessionUpdate::SessionInfoUpdate received, not forwarded to event stream");
        }

        // Future SDK variants or feature-gated variants — log and skip.
        _ => {
            debug!("Unknown SessionUpdate variant received, skipping");
        }
    }

    events
}

/// Map SDK [`ToolCallStatus`](SdkToolCallStatus) to our [`ToolCallStatus`].
fn map_sdk_tool_status(sdk: &SdkToolCallStatus) -> ToolCallStatus {
    match sdk {
        SdkToolCallStatus::Pending | SdkToolCallStatus::InProgress => ToolCallStatus::Running,
        SdkToolCallStatus::Completed => ToolCallStatus::Completed,
        SdkToolCallStatus::Failed => ToolCallStatus::Error,
        // Future SDK variants — default to Running
        _ => ToolCallStatus::Running,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_event_roundtrip() {
        let event = AgentStreamEvent::Text(TextEventData {
            content: "Hello world".into(),
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["data"]["content"], "Hello world");

        let parsed: AgentStreamEvent = serde_json::from_value(json).unwrap();
        if let AgentStreamEvent::Text(data) = parsed {
            assert_eq!(data.content, "Hello world");
        } else {
            panic!("Expected Text event");
        }
    }

    #[test]
    fn tips_event_roundtrip() {
        let event = AgentStreamEvent::Tips(TipsEventData {
            content: "Something went wrong".into(),
            tip_type: TipType::Error,
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "tips");
        assert_eq!(json["data"]["type"], "error");
    }

    #[test]
    fn tool_call_event_roundtrip() {
        let event = AgentStreamEvent::ToolCall(ToolCallEventData {
            call_id: "call-1".into(),
            name: "read_file".into(),
            args: json!({ "path": "/tmp/a.txt" }),
            status: ToolCallStatus::Running,
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "tool_call");
        assert_eq!(json["data"]["call_id"], "call-1");
        assert_eq!(json["data"]["status"], "running");
    }

    #[test]
    fn finish_event_roundtrip() {
        let event = AgentStreamEvent::Finish(FinishEventData {
            session_id: Some("sess-abc".into()),
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "finish");
        assert_eq!(json["data"]["session_id"], "sess-abc");
    }

    #[test]
    fn error_event_roundtrip() {
        let event = AgentStreamEvent::Error(ErrorEventData {
            message: "timeout".into(),
            code: Some("E001".into()),
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["data"]["message"], "timeout");
    }

    #[test]
    fn start_event_default_session_id() {
        let event = AgentStreamEvent::Start(StartEventData::default());
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "start");
        assert_eq!(json["data"]["session_id"], serde_json::Value::Null);
    }

    #[test]
    fn tool_group_event_roundtrip() {
        let entries = vec![
            ToolGroupEntry {
                call_id: "c1".into(),
                name: "read".into(),
                status: ToolCallStatus::Completed,
                description: Some("Read file".into()),
            },
            ToolGroupEntry {
                call_id: "c2".into(),
                name: "write".into(),
                status: ToolCallStatus::Running,
                description: None,
            },
        ];
        let event = AgentStreamEvent::ToolGroup(entries);
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "tool_group");
        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["call_id"], "c1");
    }

    #[test]
    fn agent_status_event_roundtrip() {
        let event = AgentStreamEvent::AgentStatus(AgentStatusEventData {
            backend: "claude".into(),
            status: "running".into(),
            agent_name: Some("default".into()),
            session_id: None,
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "agent_status");
        assert_eq!(json["data"]["backend"], "claude");
    }

    #[test]
    fn thinking_event_roundtrip() {
        let event = AgentStreamEvent::Thinking(ThinkingEventData {
            content: "Analyzing...".into(),
            subject: Some("code review".into()),
            duration: Some(1500),
            status: Some("in_progress".into()),
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "thinking");
        assert_eq!(json["data"]["duration"], 1500);
    }
}
