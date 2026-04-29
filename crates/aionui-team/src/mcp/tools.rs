use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::scheduler::SchedulerAction;
use crate::types::TeammateRole;

// ---------------------------------------------------------------------------
// Tool description constants (原样复用 AionUi `toolDescriptions.ts`)
// ---------------------------------------------------------------------------

/// `team_spawn_agent` 工具描述 — 原样复制自 AionUi `toolDescriptions.ts`
/// 对应 team-prompts.md §5.2 `team_spawn_agent` Description 原文。
/// 禁止翻译、改写；aionui-audit §8 #5 硬约束。
pub const TEAM_SPAWN_AGENT_DESCRIPTION: &str = r#"Create a new teammate agent to join the team.

Use this only when one of the following is true:
- The user explicitly approved the proposed teammate lineup in a previous message
- The user explicitly instructed you to create a specific teammate immediately

Before calling this tool in the normal planning flow:
- Start with one short sentence explaining why additional teammates would help
- Tell the user which teammate(s) you recommend
- Present the proposal as a table with: name, responsibility, recommended agent type/backend, and recommended model
- Include each teammate's responsibility, recommended agent type/backend, and model
- Ask whether to create them as proposed or change any names, responsibilities, or agent types
- In that approval question, remind the user that they can later ask you to replace or adjust any teammate if the lineup is not working well
- Do NOT call this tool in that same turn; wait for explicit approval in a later user message

When calling this tool, provide the model parameter if a specific model was recommended and approved.

The new agent will be created and added to the team. You can then assign tasks and send messages to it."#;

// ---------------------------------------------------------------------------
// Tool descriptors (returned by tools/list)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub fn all_tool_descriptors() -> Vec<ToolDescriptor> {
    vec![
        ToolDescriptor {
            name: "team_send_message".into(),
            description: "Send a message to a teammate or broadcast to all (to=\"*\").".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "Target agent slot_id or \"*\" for broadcast" },
                    "message": { "type": "string", "description": "Message content" }
                },
                "required": ["to", "message"]
            }),
        },
        ToolDescriptor {
            name: "team_spawn_agent".into(),
            description: TEAM_SPAWN_AGENT_DESCRIPTION.into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Agent display name" },
                    "role": { "type": "string", "description": "Agent role: 'teammate'" },
                    "backend": { "type": "string", "description": "AI backend (whitelist: claude, codex)" }
                },
                "required": ["name", "backend"]
            }),
        },
        ToolDescriptor {
            name: "team_task_create".into(),
            description: "Create a new task on the team task board.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "subject": { "type": "string", "description": "Task subject" },
                    "description": { "type": "string", "description": "Task description" },
                    "owner": { "type": "string", "description": "Owning agent slotId" },
                    "blocked_by": { "type": "array", "items": { "type": "string" }, "description": "Task IDs this task depends on" }
                },
                "required": ["subject"]
            }),
        },
        ToolDescriptor {
            name: "team_task_update".into(),
            description: "Update an existing task on the team task board.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "Task ID to update" },
                    "status": { "type": "string", "description": "New status: pending, in_progress, completed, deleted" },
                    "description": { "type": "string", "description": "New description" },
                    "owner": { "type": "string", "description": "New owning agent slotId" },
                    "blocked_by": { "type": "array", "items": { "type": "string" }, "description": "New dependency list" }
                },
                "required": ["task_id"]
            }),
        },
        ToolDescriptor {
            name: "team_task_list".into(),
            description: "List all tasks on the team task board.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDescriptor {
            name: "team_members".into(),
            description: "List all team members with their roles and current status.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDescriptor {
            name: "team_rename_agent".into(),
            description: "Rename a team member.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "slot_id": { "type": "string", "description": "Agent slot_id to rename" },
                    "new_name": { "type": "string", "description": "New display name" }
                },
                "required": ["slot_id", "new_name"]
            }),
        },
        ToolDescriptor {
            name: "team_shutdown_agent".into(),
            description: "Initiate shutdown of a teammate (Lead only). Sends a shutdown_request to the target agent.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "slot_id": { "type": "string", "description": "Agent slot_id to shut down" },
                    "reason": { "type": "string", "description": "Reason for shutdown" }
                },
                "required": ["slot_id"]
            }),
        },
    ]
}

// ---------------------------------------------------------------------------
// Tool call input types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SendMessageInput {
    pub to: String,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct SpawnAgentInput {
    pub name: String,
    pub role: Option<String>,
    pub backend: String,
}

#[derive(Debug, Deserialize)]
pub struct TaskCreateInput {
    pub subject: String,
    pub description: Option<String>,
    pub owner: Option<String>,
    pub blocked_by: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct TaskUpdateInput {
    pub task_id: String,
    pub status: Option<String>,
    pub description: Option<String>,
    pub owner: Option<String>,
    pub blocked_by: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct RenameAgentInput {
    pub slot_id: String,
    pub new_name: String,
}

#[derive(Debug, Deserialize)]
pub struct ShutdownAgentInput {
    pub slot_id: String,
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Backend whitelist for spawn_agent
// ---------------------------------------------------------------------------

const SPAWN_BACKEND_WHITELIST: &[&str] = &["claude", "codex"];

pub fn is_whitelisted_backend(backend: &str) -> bool {
    SPAWN_BACKEND_WHITELIST.contains(&backend)
}

// ---------------------------------------------------------------------------
// Parse tool call into SchedulerAction
// ---------------------------------------------------------------------------

pub fn parse_tool_call(
    tool_name: &str,
    arguments: &Value,
    caller_role: TeammateRole,
) -> Result<SchedulerAction, String> {
    match tool_name {
        "team_send_message" => {
            let input: SendMessageInput = serde_json::from_value(arguments.clone())
                .map_err(|e| format!("Invalid arguments for team_send_message: {e}"))?;
            Ok(SchedulerAction::SendMessage {
                to: input.to,
                message: input.message,
            })
        }
        "team_spawn_agent" => {
            if caller_role != TeammateRole::Lead {
                return Err("Only Lead can spawn agents".into());
            }
            let input: SpawnAgentInput = serde_json::from_value(arguments.clone())
                .map_err(|e| format!("Invalid arguments for team_spawn_agent: {e}"))?;
            if !is_whitelisted_backend(&input.backend) {
                return Err(format!(
                    "Backend '{}' not allowed. Whitelist: {}",
                    input.backend,
                    SPAWN_BACKEND_WHITELIST.join(", ")
                ));
            }
            Ok(SchedulerAction::SpawnAgent {
                name: input.name,
                role: input.role.unwrap_or_else(|| "teammate".into()),
                backend: input.backend,
            })
        }
        "team_task_create" => {
            let input: TaskCreateInput = serde_json::from_value(arguments.clone())
                .map_err(|e| format!("Invalid arguments for team_task_create: {e}"))?;
            Ok(SchedulerAction::TaskCreate {
                subject: input.subject,
                description: input.description,
                owner: input.owner,
                blocked_by: input.blocked_by.unwrap_or_default(),
            })
        }
        "team_task_update" => {
            let input: TaskUpdateInput = serde_json::from_value(arguments.clone())
                .map_err(|e| format!("Invalid arguments for team_task_update: {e}"))?;
            Ok(SchedulerAction::TaskUpdate {
                task_id: input.task_id,
                status: input.status,
                description: input.description,
                owner: input.owner,
                blocked_by: input.blocked_by,
            })
        }
        "team_task_list" | "team_members" | "team_rename_agent" | "team_shutdown_agent" => {
            Err("handled directly by server".into())
        }
        _ => Err(format!("Unknown tool: {tool_name}")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_descriptors_count() {
        assert_eq!(all_tool_descriptors().len(), 8);
    }

    #[test]
    fn descriptor_names_are_unique() {
        let descs = all_tool_descriptors();
        let mut names: Vec<&str> = descs.iter().map(|d| d.name.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 8);
    }

    #[test]
    fn descriptors_have_required_fields() {
        for d in all_tool_descriptors() {
            assert!(!d.name.is_empty());
            assert!(!d.description.is_empty());
            assert_eq!(d.input_schema["type"], "object");
        }
    }

    #[test]
    fn team_spawn_agent_description_is_aionui_original() {
        let desc = all_tool_descriptors()
            .into_iter()
            .find(|d| d.name == "team_spawn_agent")
            .expect("team_spawn_agent descriptor must exist")
            .description;
        assert_eq!(desc, TEAM_SPAWN_AGENT_DESCRIPTION);
        assert!(
            desc.contains("Before calling this tool"),
            "description must be the full AionUi original, not the legacy one-liner"
        );
        assert!(
            desc.contains("explicitly approved"),
            "description must retain the explicit-approval precondition clause"
        );
    }

    #[test]
    fn parse_send_message() {
        let args = json!({"to": "slot-1", "message": "hello"});
        let action = parse_tool_call("team_send_message", &args, TeammateRole::Teammate).unwrap();
        assert!(matches!(
            action,
            SchedulerAction::SendMessage { to, message }
            if to == "slot-1" && message == "hello"
        ));
    }

    #[test]
    fn parse_spawn_agent_lead_ok() {
        let args = json!({"name": "Helper", "backend": "claude"});
        let action = parse_tool_call("team_spawn_agent", &args, TeammateRole::Lead).unwrap();
        assert!(matches!(
            action,
            SchedulerAction::SpawnAgent { name, backend, role }
            if name == "Helper" && backend == "claude" && role == "teammate"
        ));
    }

    #[test]
    fn parse_spawn_agent_teammate_rejected() {
        let args = json!({"name": "X", "backend": "claude"});
        let result = parse_tool_call("team_spawn_agent", &args, TeammateRole::Teammate);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Only Lead"));
    }

    #[test]
    fn parse_spawn_agent_bad_backend() {
        let args = json!({"name": "X", "backend": "malicious"});
        let result = parse_tool_call("team_spawn_agent", &args, TeammateRole::Lead);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not allowed"));
    }

    #[test]
    fn parse_task_create() {
        let args = json!({"subject": "Implement X", "owner": "slot-a"});
        let action = parse_tool_call("team_task_create", &args, TeammateRole::Teammate).unwrap();
        assert!(matches!(
            action,
            SchedulerAction::TaskCreate { subject, owner, .. }
            if subject == "Implement X" && owner == Some("slot-a".into())
        ));
    }

    #[test]
    fn parse_task_update() {
        let args = json!({"task_id": "tk-1", "status": "completed"});
        let action = parse_tool_call("team_task_update", &args, TeammateRole::Teammate).unwrap();
        assert!(matches!(
            action,
            SchedulerAction::TaskUpdate { task_id, status, .. }
            if task_id == "tk-1" && status == Some("completed".into())
        ));
    }

    #[test]
    fn unknown_tool_errors() {
        let result = parse_tool_call("unknown_tool", &json!({}), TeammateRole::Lead);
        assert!(result.is_err());
    }

    #[test]
    fn whitelist_check() {
        assert!(is_whitelisted_backend("claude"));
        assert!(is_whitelisted_backend("codex"));
        assert!(!is_whitelisted_backend("gpt"));
        assert!(!is_whitelisted_backend(""));
    }

    #[test]
    fn parse_send_message_missing_field() {
        let args = json!({"to": "slot-1"});
        let result = parse_tool_call("team_send_message", &args, TeammateRole::Teammate);
        assert!(result.is_err());
    }

    #[test]
    fn parse_spawn_with_explicit_role() {
        let args = json!({"name": "W", "role": "worker", "backend": "codex"});
        let action = parse_tool_call("team_spawn_agent", &args, TeammateRole::Lead).unwrap();
        assert!(matches!(
            action,
            SchedulerAction::SpawnAgent { role, .. }
            if role == "worker"
        ));
    }

    #[test]
    fn task_create_with_blocked_by() {
        let args = json!({"subject": "Test", "blocked_by": ["tk-a", "tk-b"]});
        let action = parse_tool_call("team_task_create", &args, TeammateRole::Lead).unwrap();
        assert!(matches!(
            action,
            SchedulerAction::TaskCreate { blocked_by, .. }
            if blocked_by == vec!["tk-a", "tk-b"]
        ));
    }

    #[test]
    fn parse_task_list_handled_by_server() {
        let result = parse_tool_call("team_task_list", &json!({}), TeammateRole::Teammate);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("handled directly by server"));
    }

    #[test]
    fn parse_members_handled_by_server() {
        let result = parse_tool_call("team_members", &json!({}), TeammateRole::Lead);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("handled directly by server"));
    }

    #[test]
    fn parse_rename_agent_handled_by_server() {
        let args = json!({"slot_id": "s1", "new_name": "X"});
        let result = parse_tool_call("team_rename_agent", &args, TeammateRole::Lead);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("handled directly by server"));
    }

    #[test]
    fn parse_shutdown_agent_handled_by_server() {
        let args = json!({"slot_id": "s1"});
        let result = parse_tool_call("team_shutdown_agent", &args, TeammateRole::Lead);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("handled directly by server"));
    }
}
