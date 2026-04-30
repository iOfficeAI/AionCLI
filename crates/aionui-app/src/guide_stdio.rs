//! `aionui-backend mcp-guide-stdio` subcommand: MCP stdio server for team-guide tools.
//!
//! Uses the `rmcp` crate (Rust MCP SDK) for protocol handling, ensuring full
//! compatibility with Claude CLI's MCP client implementation.
//!
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

    eprintln!("[mcp-guide-stdio] Started OK. PORT={port}, BACKEND={backend}, CONV_ID={conversation_id}");

    let server = GuideServer {
        port: port.parse().unwrap_or(0),
        token,
        backend,
        conversation_id,
        http_client: reqwest::Client::new(),
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
}

impl GuideServer {
    async fn forward_tool(&self, tool_name: &str, args: &serde_json::Value) -> String {
        let url = format!("http://127.0.0.1:{}/tool", self.port);
        let body = serde_json::json!({
            "tool": tool_name,
            "args": args,
            "backend": self.backend,
            "conversation_id": self.conversation_id,
        });

        eprintln!("[mcp-guide-stdio] HTTP POST {url}");
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
                        text
                    }
                    Err(e) => {
                        eprintln!("[mcp-guide-stdio] HTTP FAILED: read body: {e}");
                        format!("Error: failed to read response: {e}")
                    }
                }
            }
            Err(e) => {
                eprintln!("[mcp-guide-stdio] HTTP FAILED: {e}");
                format!("Error: {e}")
            }
        }
    }
}
