//! `aionui-backend mcp-guide-stdio` subcommand: MCP stdio server for team-guide tools.
//!
//! Uses the `rmcp` crate (Rust MCP SDK) for protocol handling, ensuring full
//! compatibility with Claude CLI's MCP client implementation.
//!

// Pre-existing layout: `forward_tool` impl block lives after the test module.
#![allow(clippy::items_after_test_module)]
//! Tool calls are forwarded as HTTP POST to the Guide server running in the main
//! process at `http://127.0.0.1:{AION_MCP_PORT}/tool`.

use std::process::ExitCode;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{schemars, service::ServiceExt, tool, tool_router, transport};
use serde::Deserialize;

pub async fn run_guide_stdio() -> ExitCode {
    // Debug breadcrumb
    let _ = std::fs::write(
        "/tmp/mcp-guide-stdio-spawned.txt",
        format!(
            "spawned at {:?}\nargs: {:?}\nenv PORT={}\n",
            std::time::SystemTime::now(),
            std::env::args().collect::<Vec<_>>(),
            std::env::var("AION_MCP_PORT").unwrap_or_default(),
        ),
    );

    let port = match std::env::var("AION_MCP_PORT") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("[mcp-guide-stdio] ERROR: missing AION_MCP_PORT");
            return ExitCode::from(1);
        }
    };
    let token = match std::env::var("AION_MCP_TOKEN") {
        Ok(t) => t,
        Err(_) => {
            eprintln!("[mcp-guide-stdio] ERROR: missing AION_MCP_TOKEN");
            return ExitCode::from(1);
        }
    };
    let backend = std::env::var("AION_MCP_BACKEND").unwrap_or_default();
    let conversation_id = std::env::var("AION_MCP_CONVERSATION_ID").unwrap_or_default();
    let user_id = std::env::var("AION_MCP_USER_ID").unwrap_or_default();

    eprintln!(
        "[mcp-guide-stdio] Started OK. PORT={port}, BACKEND={backend}, CONV_ID={conversation_id}, USER={user_id}"
    );

    let http_client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let server = GuideServer {
        port: port.parse().unwrap_or(0),
        token,
        backend,
        conversation_id,
        user_id,
        http_client,
    };

    let transport = transport::io::stdio();
    match server.serve(transport).await {
        Ok(peer) => {
            eprintln!("[mcp-guide-stdio] MCP session started, waiting for completion...");
            if let Err(e) = peer.waiting().await {
                eprintln!("[mcp-guide-stdio] Session ended with error: {e}");
            } else {
                eprintln!("[mcp-guide-stdio] Session ended normally");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("[mcp-guide-stdio] Failed to start MCP server: {e}");
            ExitCode::from(1)
        }
    }
}

#[derive(Clone)]
struct GuideServer {
    port: u16,
    token: String,
    backend: String,
    conversation_id: String,
    user_id: String,
    http_client: reqwest::Client,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct CreateTeamParams {
    /// Task summary or initial instruction to send to the team leader agent.
    summary: String,
    /// Optional team name. When omitted the first few words of summary are used.
    #[serde(default)]
    name: Option<String>,
    /// Absolute path to the project workspace directory.
    #[serde(default)]
    workspace: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ListModelsParams {
    /// Agent type/backend to query (e.g. "gemini", "claude", "codex"). Shows all when omitted.
    #[serde(default)]
    agent_type: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SendMessageParams {
    /// Target teammate name, or "*" to broadcast to all.
    to: String,
    /// Message content to send.
    message: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SpawnAgentParams {
    /// Name for the new teammate agent.
    name: String,
    /// AI backend type: "claude" or "codex". Default when omitted.
    #[serde(default)]
    agent_type: Option<String>,
    /// Preset assistant identifier.
    #[serde(default)]
    custom_agent_id: Option<String>,
    /// Model override for the new agent.
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TaskCreateParams {
    /// Short task title.
    subject: String,
    /// Detailed task description.
    #[serde(default)]
    description: Option<String>,
    /// Teammate name assigned as owner.
    #[serde(default)]
    owner: Option<String>,
    /// Task IDs that must complete before this task can start.
    #[serde(default)]
    blocked_by: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TaskUpdateParams {
    /// ID of the task to update.
    task_id: String,
    /// New status: pending, in_progress, completed, or deleted.
    #[serde(default)]
    status: Option<String>,
    /// Updated task description.
    #[serde(default)]
    description: Option<String>,
    /// New owner teammate name.
    #[serde(default)]
    owner: Option<String>,
    /// Updated list of blocking task IDs.
    #[serde(default)]
    blocked_by: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RenameAgentParams {
    /// Slot ID of the team member to rename.
    slot_id: String,
    /// New display name.
    new_name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ShutdownAgentParams {
    /// Slot ID of the teammate to shut down.
    slot_id: String,
    /// Optional reason for shutdown.
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TeamListModelsParams {
    /// Agent type to filter models (e.g. "claude", "codex"). Shows all when omitted.
    #[serde(default)]
    agent_type: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct DescribeAssistantParams {
    /// Preset assistant identifier to look up.
    custom_agent_id: String,
    /// Locale for the description (e.g. "en", "zh"). Default when omitted.
    #[serde(default)]
    locale: Option<String>,
}

#[tool_router(server_handler)]
impl GuideServer {
    #[tool(
        name = "aion_create_team",
        description = "Create a multi-agent Team. Only call after user explicitly confirms team configuration."
    )]
    async fn create_team(&self, Parameters(params): Parameters<CreateTeamParams>) -> String {
        eprintln!("[mcp-guide-stdio] tools/call: aion_create_team");
        self.forward_tool(
            "aion_create_team",
            &serde_json::json!({
                "summary": params.summary,
                "name": params.name,
                "workspace": params.workspace,
            }),
        )
        .await
    }

    #[tool(
        name = "aion_list_models",
        description = "Query available models for team agent types. Pass agent_type to filter, or omit to see all."
    )]
    async fn list_models(&self, Parameters(params): Parameters<ListModelsParams>) -> String {
        eprintln!("[mcp-guide-stdio] tools/call: aion_list_models");
        self.forward_tool(
            "aion_list_models",
            &serde_json::json!({
                "agent_type": params.agent_type,
            }),
        )
        .await
    }

    #[tool(
        name = "team_send_message",
        description = "Send a message to a teammate or broadcast to all (to=\"*\")."
    )]
    async fn team_send_message(&self, Parameters(params): Parameters<SendMessageParams>) -> String {
        eprintln!("[mcp-guide-stdio] tools/call: team_send_message");
        self.forward_tool(
            "team_send_message",
            &serde_json::json!({
                "to": params.to,
                "message": params.message,
            }),
        )
        .await
    }

    #[tool(
        name = "team_spawn_agent",
        description = "Create a new teammate agent to join the team. Leader only."
    )]
    async fn team_spawn_agent(&self, Parameters(params): Parameters<SpawnAgentParams>) -> String {
        eprintln!("[mcp-guide-stdio] tools/call: team_spawn_agent");
        self.forward_tool(
            "team_spawn_agent",
            &serde_json::json!({
                "name": params.name,
                "agent_type": params.agent_type,
                "custom_agent_id": params.custom_agent_id,
                "model": params.model,
            }),
        )
        .await
    }

    #[tool(name = "team_task_create", description = "Create a new task on the team task board.")]
    async fn team_task_create(&self, Parameters(params): Parameters<TaskCreateParams>) -> String {
        eprintln!("[mcp-guide-stdio] tools/call: team_task_create");
        self.forward_tool(
            "team_task_create",
            &serde_json::json!({
                "subject": params.subject,
                "description": params.description,
                "owner": params.owner,
                "blocked_by": params.blocked_by,
            }),
        )
        .await
    }

    #[tool(
        name = "team_task_update",
        description = "Update an existing task on the team task board."
    )]
    async fn team_task_update(&self, Parameters(params): Parameters<TaskUpdateParams>) -> String {
        eprintln!("[mcp-guide-stdio] tools/call: team_task_update");
        self.forward_tool(
            "team_task_update",
            &serde_json::json!({
                "task_id": params.task_id,
                "status": params.status,
                "description": params.description,
                "owner": params.owner,
                "blocked_by": params.blocked_by,
            }),
        )
        .await
    }

    #[tool(name = "team_task_list", description = "List all tasks on the team task board.")]
    async fn team_task_list(&self) -> String {
        eprintln!("[mcp-guide-stdio] tools/call: team_task_list");
        self.forward_tool("team_task_list", &serde_json::json!({})).await
    }

    #[tool(
        name = "team_members",
        description = "List all team members with their roles and current status."
    )]
    async fn team_members(&self) -> String {
        eprintln!("[mcp-guide-stdio] tools/call: team_members");
        self.forward_tool("team_members", &serde_json::json!({})).await
    }

    #[tool(name = "team_rename_agent", description = "Rename a team member.")]
    async fn team_rename_agent(&self, Parameters(params): Parameters<RenameAgentParams>) -> String {
        eprintln!("[mcp-guide-stdio] tools/call: team_rename_agent");
        self.forward_tool(
            "team_rename_agent",
            &serde_json::json!({
                "slot_id": params.slot_id,
                "new_name": params.new_name,
            }),
        )
        .await
    }

    #[tool(
        name = "team_shutdown_agent",
        description = "Initiate graceful shutdown of a teammate. Leader only."
    )]
    async fn team_shutdown_agent(&self, Parameters(params): Parameters<ShutdownAgentParams>) -> String {
        eprintln!("[mcp-guide-stdio] tools/call: team_shutdown_agent");
        self.forward_tool(
            "team_shutdown_agent",
            &serde_json::json!({
                "slot_id": params.slot_id,
                "reason": params.reason,
            }),
        )
        .await
    }

    #[tool(
        name = "team_list_models",
        description = "Query available models for team agent types."
    )]
    async fn team_list_models(&self, Parameters(params): Parameters<TeamListModelsParams>) -> String {
        eprintln!("[mcp-guide-stdio] tools/call: team_list_models");
        self.forward_tool(
            "team_list_models",
            &serde_json::json!({
                "agent_type": params.agent_type,
            }),
        )
        .await
    }

    #[tool(
        name = "team_describe_assistant",
        description = "Get detailed information about a preset assistant before spawning."
    )]
    async fn team_describe_assistant(&self, Parameters(params): Parameters<DescribeAssistantParams>) -> String {
        eprintln!("[mcp-guide-stdio] tools/call: team_describe_assistant");
        self.forward_tool(
            "team_describe_assistant",
            &serde_json::json!({
                "custom_agent_id": params.custom_agent_id,
                "locale": params.locale,
            }),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tool_schemas_have_properties_field() {
        let router = GuideServer::tool_router();
        let tools = router.list_all();
        assert!(!tools.is_empty());
        for tool in &tools {
            assert!(
                tool.input_schema.contains_key("properties"),
                "Tool '{}' schema missing 'properties' field: {:?}. OpenAI API rejects schemas without it.",
                tool.name,
                tool.input_schema,
            );
        }
    }
}

impl GuideServer {
    async fn forward_tool(&self, tool_name: &str, args: &serde_json::Value) -> String {
        let url = format!("http://127.0.0.1:{}/tool", self.port);
        let body = serde_json::json!({
            "tool": tool_name,
            "args": args,
            "backend": self.backend,
            "conversation_id": self.conversation_id,
            "user_id": self.user_id,
        });

        // Retry up to 3 times with backoff — the Guide HTTP server may not be
        // fully ready immediately after a session resume spawns this process.
        let delays_ms: &[u64] = &[0, 1000, 2000, 3000];
        let mut last_error = String::new();
        for (attempt, &delay_ms) in delays_ms.iter().enumerate() {
            if delay_ms > 0 {
                let delay = std::time::Duration::from_millis(delay_ms);
                eprintln!("[mcp-guide-stdio] retrying in {delay:?}...");
                tokio::time::sleep(delay).await;
            }
            eprintln!("[mcp-guide-stdio] HTTP POST {url} (attempt {})", attempt + 1);
            match self
                .http_client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.token))
                .json(&body)
                .send()
                .await
            {
                Ok(resp) => {
                    let status = resp.status();
                    match resp.text().await {
                        Ok(text) => {
                            eprintln!(
                                "[mcp-guide-stdio] HTTP POST /tool → status={status}, body_preview={}",
                                &text[..text.len().min(100)]
                            );
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                                if let Some(result) = v.get("result").and_then(|r| r.as_str()) {
                                    return result.to_owned();
                                }
                                if let Some(error) = v.get("error") {
                                    return format!("Error: {error}");
                                }
                            }
                            return text;
                        }
                        Err(e) => {
                            last_error = format!("failed to read response: {e}");
                            eprintln!("[mcp-guide-stdio] HTTP FAILED: {last_error}");
                        }
                    }
                }
                Err(e) => {
                    last_error = format!("{e:#}");
                    eprintln!("[mcp-guide-stdio] HTTP FAILED: {last_error}");
                }
            }
        }
        format!("Error: {last_error}")
    }
}
