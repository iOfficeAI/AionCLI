use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use aionui_api_types::TeamMcpStdioConfig;
use aionui_common::{AcpBackend, AgentType, ProviderWithModel};

/// Data payload for sending a user message to an Agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageData {
    /// User message content.
    pub content: String,
    /// Client-generated message ID for correlation.
    pub msg_id: String,
    /// File paths attached to the message.
    #[serde(default)]
    pub files: Vec<String>,
    /// Skills to inject into this message turn.
    #[serde(default)]
    pub inject_skills: Vec<String>,
}

/// Options for building (creating or resuming) an Agent task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildTaskOptions {
    /// Type of agent to create.
    pub agent_type: AgentType,
    /// Working directory for the agent.
    pub workspace: String,
    /// Model selection config.
    pub model: ProviderWithModel,
    /// Conversation ID this task belongs to.
    pub conversation_id: String,
    /// Type-specific extra parameters (JSON object).
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// ACP-specific fields extracted from `extra` in [`BuildTaskOptions`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpBuildExtra {
    /// Agent registry ID. When provided, `backend`/`cli_path` are resolved
    /// from the registry and need not be supplied by the caller.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// ACP sub-backend identifier.
    #[serde(default)]
    pub backend: Option<AcpBackend>,
    /// Path to the CLI executable (resolved from registry when `agent_id` is set).
    #[serde(default)]
    pub cli_path: Option<String>,
    /// Agent name within the ACP backend.
    #[serde(default)]
    pub agent_name: Option<String>,
    /// Custom agent ID (for user-defined agents).
    #[serde(default)]
    pub custom_agent_id: Option<String>,
    /// Preset context to inject.
    #[serde(default)]
    pub preset_context: Option<String>,
    /// Resolved skill snapshot for the session. Populated from
    /// `conversation.extra.skills` by the factory. No runtime filtering —
    /// what the caller sends is what the agent sees.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Preset assistant ID.
    #[serde(default)]
    pub preset_assistant_id: Option<String>,
    /// Session mode override.
    #[serde(default)]
    pub session_mode: Option<String>,
    /// Associated cron job ID.
    #[serde(default)]
    pub cron_job_id: Option<String>,
    /// Team session MCP stdio config. When present, the factory injects it as
    /// a stdio MCP server on `session/new`. Written by
    /// `TeamSessionService::ensure_session` into `conversation.extra`; solo
    /// conversations leave this unset.
    #[serde(default)]
    pub team_mcp_stdio_config: Option<TeamMcpStdioConfig>,
}

/// OpenClaw gateway configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenClawGatewayConfig {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub token: Option<String>,
    pub password: Option<String>,
    #[serde(default)]
    pub use_external_gateway: bool,
    pub cli_path: Option<String>,
}

/// OpenClaw-specific fields extracted from `extra` in [`BuildTaskOptions`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenClawBuildExtra {
    /// Optional downstream AI backend (informational only).
    #[serde(default)]
    pub backend: Option<AcpBackend>,
    /// Agent name.
    #[serde(default)]
    pub agent_name: Option<String>,
    /// OpenClaw gateway configuration.
    #[serde(default)]
    pub gateway: OpenClawGatewayConfig,
    /// Resolved skill snapshot for the session. Populated from
    /// `conversation.extra.skills` by the factory.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Preset assistant ID.
    #[serde(default)]
    pub preset_assistant_id: Option<String>,
    /// Associated cron job ID.
    #[serde(default)]
    pub cron_job_id: Option<String>,
    /// Persisted session key for resume (from conversation.extra.sessionKey).
    #[serde(default, rename = "sessionKey")]
    pub session_key: Option<String>,
}

/// Remote agent-specific fields extracted from `extra` in [`BuildTaskOptions`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteBuildExtra {
    /// Remote agent configuration ID.
    pub remote_agent_id: String,
}

/// Aionrs-specific fields extracted from `extra` in [`BuildTaskOptions`].
///
/// Provider credentials (provider name, api_key, model) are resolved from
/// the providers table in the factory — they are NOT expected in `extra`.
/// This struct only carries optional overrides the caller may supply.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AionrsBuildExtra {
    /// System prompt override.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Max tokens per response.
    #[serde(default = "default_aionrs_max_tokens")]
    pub max_tokens: u32,
    /// Max agentic turns.
    #[serde(default)]
    pub max_turns: Option<usize>,
    /// Session mode (default, auto_edit, yolo).
    #[serde(default)]
    pub session_mode: Option<String>,
}

/// Provider-specific compat overrides resolved in the factory.
///
/// These are merged on top of the provider defaults in the agent manager.
#[derive(Debug, Clone, Default)]
pub struct AionrsCompatOverrides {
    pub max_tokens_field: Option<String>,
    pub api_path: Option<String>,
}

/// Fully resolved Aionrs configuration passed to the agent manager.
///
/// Constructed in the factory by combining provider DB data with
/// optional overrides from [`AionrsBuildExtra`].
#[derive(Debug, Clone)]
pub struct AionrsResolvedConfig {
    /// LLM provider name (anthropic, openai, bedrock, vertex).
    pub provider: String,
    /// Decrypted API key.
    pub api_key: String,
    /// Model identifier.
    pub model: String,
    /// Provider base URL.
    pub base_url: Option<String>,
    /// System prompt override.
    pub system_prompt: Option<String>,
    /// Max tokens per response.
    pub max_tokens: u32,
    /// Max agentic turns.
    pub max_turns: Option<usize>,
    /// Provider-specific compat overrides.
    pub compat_overrides: AionrsCompatOverrides,
    /// Directory for aionrs session persistence files.
    pub session_directory: PathBuf,
    /// Session mode (default, auto_edit, yolo).
    pub session_mode: Option<String>,
}

fn default_aionrs_max_tokens() -> u32 {
    8192
}

/// ACP model information returned by the ACP backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpModelInfo {
    pub model_id: String,
    pub model_name: Option<String>,
    pub provider: Option<String>,
}

/// ACP session configuration option.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpSessionConfigOption {
    pub config_id: String,
    pub label: String,
    pub value: String,
    /// Possible values; `None` means free-form input.
    pub options: Option<Vec<String>>,
}

/// A slash command item available in a conversation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashCommandItem {
    pub command: String,
    pub description: String,
}

/// Unified stream chunk published on the per-session broadcast channel.
///
/// Emitted by `AgentManagerHandle` implementations at every stream event so
/// downstream subscribers (wake timeout watchdog, crash detector) can observe
/// agent progress without intercepting the existing frontend event pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentStreamChunk {
    Text {
        text: String,
    },
    ToolUse {
        tool_name: String,
        input: serde_json::Value,
    },
    Thought {
        content: String,
    },
    Finish {
        agent_crash: bool,
        stop_reason: Option<String>,
    },
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn acp_build_extra_accepts_payload_without_skills() {
        // Legacy payloads without skills should still deserialize.
        let legacy = r#"{"backend":"claude"}"#;
        let parsed: AcpBuildExtra = serde_json::from_str(legacy).unwrap();
        assert!(parsed.skills.is_empty());
    }

    #[test]
    fn acp_build_extra_accepts_skills() {
        let with_field = r#"{"backend":"claude","skills":["cron","pdf"]}"#;
        let parsed: AcpBuildExtra = serde_json::from_str(with_field).unwrap();
        assert_eq!(parsed.skills, vec!["cron".to_owned(), "pdf".to_owned()]);
    }

    #[test]
    fn acp_build_extra_missing_team_mcp_stdio_config_is_none() {
        // Legacy extra (pre-team-wave1) must deserialize cleanly with the new
        // optional `team_mcp_stdio_config` absent — this keeps solo chat
        // conversations working without any migration.
        let legacy = r#"{"backend":"claude","skills":["cron"]}"#;
        let parsed: AcpBuildExtra = serde_json::from_str(legacy).unwrap();
        assert!(parsed.team_mcp_stdio_config.is_none());
    }

    #[test]
    fn acp_build_extra_parses_team_mcp_stdio_config() {
        let with_cfg = r#"{
            "backend":"claude",
            "team_mcp_stdio_config":{
                "team_id":"team-42",
                "port":54321,
                "token":"tok-abc",
                "slot_id":"slot-lead"
            }
        }"#;
        let parsed: AcpBuildExtra = serde_json::from_str(with_cfg).unwrap();
        let cfg = parsed.team_mcp_stdio_config.expect("config present");
        assert_eq!(cfg.team_id, "team-42");
        assert_eq!(cfg.port, 54321);
        assert_eq!(cfg.token, "tok-abc");
        assert_eq!(cfg.slot_id, "slot-lead");
    }

    #[test]
    fn send_message_data_serde_roundtrip() {
        let data = SendMessageData {
            content: "Hello".into(),
            msg_id: "msg-001".into(),
            files: vec!["/tmp/a.txt".into()],
            inject_skills: vec!["review".into()],
        };
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(json["content"], "Hello");
        assert_eq!(json["msg_id"], "msg-001");
        assert_eq!(json["files"], json!(["/tmp/a.txt"]));
        assert_eq!(json["inject_skills"], json!(["review"]));

        let parsed: SendMessageData = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.content, "Hello");
        assert_eq!(parsed.msg_id, "msg-001");
    }

    #[test]
    fn send_message_data_defaults_optional_fields() {
        let json = json!({ "content": "Hi", "msg_id": "m1" });
        let data: SendMessageData = serde_json::from_value(json).unwrap();
        assert!(data.files.is_empty());
        assert!(data.inject_skills.is_empty());
    }

    #[test]
    fn build_task_options_serde() {
        let opts = BuildTaskOptions {
            agent_type: AgentType::Acp,
            workspace: "/project".into(),
            model: ProviderWithModel {
                provider_id: "p1".into(),
                model: "claude-sonnet".into(),
                use_model: None,
            },
            conversation_id: "conv-1".into(),
            extra: json!({ "backend": "claude" }),
        };
        let json = serde_json::to_value(&opts).unwrap();
        assert_eq!(json["agent_type"], "acp");
        assert_eq!(json["workspace"], "/project");
        assert_eq!(json["conversation_id"], "conv-1");
    }

    #[test]
    fn acp_model_info_serde() {
        let info = AcpModelInfo {
            model_id: "claude-sonnet-4".into(),
            model_name: Some("Claude Sonnet 4".into()),
            provider: Some("anthropic".into()),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["model_id"], "claude-sonnet-4");
        assert_eq!(json["model_name"], "Claude Sonnet 4");
    }

    #[test]
    fn acp_session_config_option_serde() {
        let opt = AcpSessionConfigOption {
            config_id: "theme".into(),
            label: "Theme".into(),
            value: "dark".into(),
            options: Some(vec!["light".into(), "dark".into()]),
        };
        let json = serde_json::to_value(&opt).unwrap();
        assert_eq!(json["config_id"], "theme");
        assert_eq!(json["options"], json!(["light", "dark"]));
    }

    #[test]
    fn slash_command_item_serde() {
        let cmd = SlashCommandItem {
            command: "/review".into(),
            description: "Code review".into(),
        };
        let json = serde_json::to_value(&cmd).unwrap();
        assert_eq!(json["command"], "/review");
    }

    #[test]
    fn openclaw_gateway_config_defaults() {
        let json = json!({});
        let config: OpenClawGatewayConfig = serde_json::from_value(json).unwrap();
        assert!(!config.use_external_gateway);
        assert!(config.host.is_none());
        assert!(config.port.is_none());
    }

    #[test]
    fn aionrs_build_extra_serde_defaults() {
        let json = json!({});
        let extra: AionrsBuildExtra = serde_json::from_value(json).unwrap();
        assert!(extra.system_prompt.is_none());
        assert_eq!(extra.max_tokens, 8192);
        assert!(extra.max_turns.is_none());
    }

    #[test]
    fn agent_stream_chunk_all_variants_serde_roundtrip() {
        let cases = vec![
            AgentStreamChunk::Text {
                text: "hello".into(),
            },
            AgentStreamChunk::ToolUse {
                tool_name: "read_file".into(),
                input: json!({ "path": "/tmp/a.txt" }),
            },
            AgentStreamChunk::Thought {
                content: "analyzing".into(),
            },
            AgentStreamChunk::Finish {
                agent_crash: false,
                stop_reason: Some("end_turn".into()),
            },
            AgentStreamChunk::Error {
                message: "timeout".into(),
            },
        ];
        for chunk in cases {
            let json = serde_json::to_value(&chunk).unwrap();
            let parsed: AgentStreamChunk = serde_json::from_value(json).unwrap();
            match (chunk, parsed) {
                (AgentStreamChunk::Text { text: a }, AgentStreamChunk::Text { text: b }) => {
                    assert_eq!(a, b);
                }
                (
                    AgentStreamChunk::ToolUse {
                        tool_name: a1,
                        input: a2,
                    },
                    AgentStreamChunk::ToolUse {
                        tool_name: b1,
                        input: b2,
                    },
                ) => {
                    assert_eq!(a1, b1);
                    assert_eq!(a2, b2);
                }
                (
                    AgentStreamChunk::Thought { content: a },
                    AgentStreamChunk::Thought { content: b },
                ) => {
                    assert_eq!(a, b);
                }
                (
                    AgentStreamChunk::Finish {
                        agent_crash: a1,
                        stop_reason: a2,
                    },
                    AgentStreamChunk::Finish {
                        agent_crash: b1,
                        stop_reason: b2,
                    },
                ) => {
                    assert_eq!(a1, b1);
                    assert_eq!(a2, b2);
                }
                (
                    AgentStreamChunk::Error { message: a },
                    AgentStreamChunk::Error { message: b },
                ) => {
                    assert_eq!(a, b);
                }
                _ => panic!("variant mismatch after roundtrip"),
            }
        }
    }

    #[test]
    fn aionrs_build_extra_serde_with_overrides() {
        let json = json!({
            "system_prompt": "You are a helpful assistant.",
            "max_tokens": 4096,
            "max_turns": 10
        });
        let extra: AionrsBuildExtra = serde_json::from_value(json).unwrap();
        assert_eq!(extra.system_prompt.unwrap(), "You are a helpful assistant.");
        assert_eq!(extra.max_tokens, 4096);
        assert_eq!(extra.max_turns.unwrap(), 10);
    }
}
