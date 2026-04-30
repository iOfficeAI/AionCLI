//! `aionui-backend mcp-team-stdio` subcommand: MCP stdio server for team tools.
//!
//! Uses the `rmcp` crate (Rust MCP SDK) for protocol handling. Tool calls are
//! forwarded to the TeamMcpServer TCP listener via 4-byte big-endian
//! length-prefixed JSON frames — the same wire protocol used by `mcp-bridge`,
//! but with proper tool registration via rmcp instead of transparent proxying.
//!
//! Each tool call opens a fresh TCP connection, sends an `initialize` frame
//! (injecting auth_token + slot_id), then sends the `tools/call` frame, reads
//! the response, and closes the connection (one-shot mode).

use std::io;
use std::process::ExitCode;

use aionui_api_types::TeamMcpStdioConfig;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{schemars, service::ServiceExt, tool, tool_router, transport};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const CONNECT_HOST: &str = "127.0.0.1";

pub async fn run_team_stdio() -> ExitCode {
    let _ = std::fs::write(
        "/tmp/mcp-team-stdio-spawned.txt",
        format!(
            "spawned at {:?}\nargs: {:?}\nenv PORT={}\n",
            std::time::SystemTime::now(),
            std::env::args().collect::<Vec<_>>(),
            std::env::var(TeamMcpStdioConfig::ENV_PORT).unwrap_or_default(),
        ),
    );

    let port: u16 = match std::env::var(TeamMcpStdioConfig::ENV_PORT) {
        Ok(p) => match p.parse() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[mcp-team-stdio] ERROR: invalid {}: {e}", TeamMcpStdioConfig::ENV_PORT);
                return ExitCode::from(1);
            }
        },
        Err(_) => {
            eprintln!("[mcp-team-stdio] ERROR: missing {}", TeamMcpStdioConfig::ENV_PORT);
            return ExitCode::from(1);
        }
    };
    let token = match std::env::var(TeamMcpStdioConfig::ENV_TOKEN) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("[mcp-team-stdio] ERROR: missing {}", TeamMcpStdioConfig::ENV_TOKEN);
            return ExitCode::from(1);
        }
    };
    let slot_id = match std::env::var(TeamMcpStdioConfig::ENV_SLOT_ID) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[mcp-team-stdio] ERROR: missing {}", TeamMcpStdioConfig::ENV_SLOT_ID);
            return ExitCode::from(1);
        }
    };

    eprintln!("[mcp-team-stdio] Started. PORT={port}, SLOT_ID={slot_id}");

    let server = TeamStdioServer { port, token, slot_id };

    let transport = transport::io::stdio();
    match server.serve(transport).await {
        Ok(peer) => {
            eprintln!("[mcp-team-stdio] MCP session started, waiting for completion...");
            if let Err(e) = peer.waiting().await {
                eprintln!("[mcp-team-stdio] Session ended with error: {e}");
            } else {
                eprintln!("[mcp-team-stdio] Session ended normally");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("[mcp-team-stdio] Failed to start MCP server: {e}");
            ExitCode::from(1)
        }
    }
}

// ---------------------------------------------------------------------------
// Server struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct TeamStdioServer {
    port: u16,
    token: String,
    slot_id: String,
}

// ---------------------------------------------------------------------------
// Parameter types
// ---------------------------------------------------------------------------

#[derive(Deserialize, schemars::JsonSchema)]
struct SendMessageParams {
    /// Target agent slot_id or "*" for broadcast.
    to: String,
    /// Message content.
    message: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SpawnAgentParams {
    /// Agent display name.
    name: String,
    /// AI backend (whitelist: claude, codex).
    backend: String,
    /// Agent role (default: "teammate").
    #[serde(default)]
    role: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TaskCreateParams {
    /// Task subject.
    subject: String,
    /// Task description.
    #[serde(default)]
    description: Option<String>,
    /// Owning agent slot_id.
    #[serde(default)]
    owner: Option<String>,
    /// Task IDs this task depends on.
    #[serde(default)]
    blocked_by: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TaskUpdateParams {
    /// Task ID to update.
    task_id: String,
    /// New status: pending, in_progress, completed, deleted.
    #[serde(default)]
    status: Option<String>,
    /// New description.
    #[serde(default)]
    description: Option<String>,
    /// New owning agent slot_id.
    #[serde(default)]
    owner: Option<String>,
    /// New dependency list.
    #[serde(default)]
    blocked_by: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RenameAgentParams {
    /// Agent slot_id to rename.
    slot_id: String,
    /// New display name.
    new_name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ShutdownAgentParams {
    /// Agent slot_id to shut down.
    slot_id: String,
    /// Reason for shutdown.
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ListModelsParams {
    /// Agent type/backend to query (e.g. "gemini", "claude", "codex"). Shows all when omitted.
    #[serde(default)]
    agent_type: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct DescribeAssistantParams {
    /// The preset assistant ID from the "Available Preset Assistants" catalog.
    custom_agent_id: String,
}

// ---------------------------------------------------------------------------
// Tool router
// ---------------------------------------------------------------------------

#[tool_router(server_handler)]
impl TeamStdioServer {
    #[tool(
        name = "team_send_message",
        description = "Send a message to a teammate or broadcast to all (to=\"*\")."
    )]
    async fn send_message(&self, Parameters(params): Parameters<SendMessageParams>) -> String {
        eprintln!("[mcp-team-stdio] tools/call: team_send_message to={}", params.to);
        self.forward_to_tcp(
            "team_send_message",
            &serde_json::json!({ "to": params.to, "message": params.message }),
        )
        .await
    }

    #[tool(
        name = "team_spawn_agent",
        description = "Create a new teammate agent to join the team.\n\nUse this only when one of the following is true:\n- The user explicitly approved the proposed teammate lineup in a previous message\n- The user explicitly instructed you to create a specific teammate immediately\n\nBefore calling this tool in the normal planning flow:\n- Start with one short sentence explaining why additional teammates would help\n- Tell the user which teammate(s) you recommend\n- Present the proposal as a table with: name, responsibility, recommended agent type/backend, and recommended model\n- Include each teammate's responsibility, recommended agent type/backend, and model\n- Ask whether to create them as proposed or change any names, responsibilities, or agent types\n- In that approval question, remind the user that they can later ask you to replace or adjust any teammate if the lineup is not working well\n- Do NOT call this tool in that same turn; wait for explicit approval in a later user message\n\nWhen calling this tool, provide the model parameter if a specific model was recommended and approved.\n\nThe new agent will be created and added to the team. You can then assign tasks and send messages to it."
    )]
    async fn spawn_agent(&self, Parameters(params): Parameters<SpawnAgentParams>) -> String {
        eprintln!("[mcp-team-stdio] tools/call: team_spawn_agent name={}", params.name);
        self.forward_to_tcp(
            "team_spawn_agent",
            &serde_json::json!({
                "name": params.name,
                "backend": params.backend,
                "role": params.role,
            }),
        )
        .await
    }

    #[tool(name = "team_task_create", description = "Create a new task on the team task board.")]
    async fn task_create(&self, Parameters(params): Parameters<TaskCreateParams>) -> String {
        eprintln!(
            "[mcp-team-stdio] tools/call: team_task_create subject={}",
            params.subject
        );
        self.forward_to_tcp(
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
    async fn task_update(&self, Parameters(params): Parameters<TaskUpdateParams>) -> String {
        eprintln!(
            "[mcp-team-stdio] tools/call: team_task_update task_id={}",
            params.task_id
        );
        self.forward_to_tcp(
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
    async fn task_list(&self) -> String {
        eprintln!("[mcp-team-stdio] tools/call: team_task_list");
        self.forward_to_tcp("team_task_list", &serde_json::json!({})).await
    }

    #[tool(
        name = "team_members",
        description = "List all team members with their roles and current status."
    )]
    async fn members(&self) -> String {
        eprintln!("[mcp-team-stdio] tools/call: team_members");
        self.forward_to_tcp("team_members", &serde_json::json!({})).await
    }

    #[tool(name = "team_rename_agent", description = "Rename a team member.")]
    async fn rename_agent(&self, Parameters(params): Parameters<RenameAgentParams>) -> String {
        eprintln!(
            "[mcp-team-stdio] tools/call: team_rename_agent slot_id={}",
            params.slot_id
        );
        self.forward_to_tcp(
            "team_rename_agent",
            &serde_json::json!({ "slot_id": params.slot_id, "new_name": params.new_name }),
        )
        .await
    }

    #[tool(
        name = "team_shutdown_agent",
        description = "Initiate shutdown of a teammate (Lead only). Sends a shutdown_request to the target agent."
    )]
    async fn shutdown_agent(&self, Parameters(params): Parameters<ShutdownAgentParams>) -> String {
        eprintln!(
            "[mcp-team-stdio] tools/call: team_shutdown_agent slot_id={}",
            params.slot_id
        );
        self.forward_to_tcp(
            "team_shutdown_agent",
            &serde_json::json!({ "slot_id": params.slot_id, "reason": params.reason }),
        )
        .await
    }

    #[tool(
        name = "team_list_models",
        description = "Query available models for team agent types. Returns the real-time model list that matches the frontend model selector.\n\nUse this to:\n- Check what models are available before spawning an agent with a specific model\n- See all available agent types and their models at once\n- Verify a model ID is valid for a given agent type\n\nPass agent_type to query a specific backend, or omit it to see all."
    )]
    async fn list_models(&self, Parameters(params): Parameters<ListModelsParams>) -> String {
        eprintln!("[mcp-team-stdio] tools/call: team_list_models");
        self.forward_to_tcp(
            "team_list_models",
            &serde_json::json!({ "agent_type": params.agent_type }),
        )
        .await
    }

    #[tool(
        name = "team_describe_assistant",
        description = "Get detailed information about a preset assistant before spawning it as a teammate.\n\nReturns the preset's full description, enabled skills, and example tasks so you can\njudge whether it fits the user's request. Use this when two or more presets look\nrelevant from the one-line catalog in your system prompt.\n\nOnly works on preset assistants listed in \"Available Preset Assistants for Spawning\".\nAfter confirming a match, call team_spawn_agent with the same custom_agent_id."
    )]
    async fn describe_assistant(&self, Parameters(params): Parameters<DescribeAssistantParams>) -> String {
        eprintln!(
            "[mcp-team-stdio] tools/call: team_describe_assistant id={}",
            params.custom_agent_id
        );
        self.forward_to_tcp(
            "team_describe_assistant",
            &serde_json::json!({ "custom_agent_id": params.custom_agent_id }),
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// TCP forwarding
// ---------------------------------------------------------------------------

impl TeamStdioServer {
    /// One-shot TCP forward: connect → initialize (with auth) → tools/call → read response → close.
    async fn forward_to_tcp(&self, tool_name: &str, args: &serde_json::Value) -> String {
        match self.do_forward(tool_name, args).await {
            Ok(result) => result,
            Err(e) => {
                eprintln!("[mcp-team-stdio] TCP forward error for {tool_name}: {e}");
                format!("Error: {e}")
            }
        }
    }

    async fn do_forward(&self, tool_name: &str, args: &serde_json::Value) -> io::Result<String> {
        let mut stream = TcpStream::connect((CONNECT_HOST, self.port)).await?;
        stream.set_nodelay(true).ok();

        // initialize with auth
        let init_frame = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {
                "auth_token": self.token,
                "slot_id": self.slot_id,
            }
        });
        write_frame(&mut stream, &serde_json::to_vec(&init_frame).unwrap()).await?;
        let init_resp = read_frame(&mut stream).await?;
        eprintln!(
            "[mcp-team-stdio] init response: {}",
            String::from_utf8_lossy(&init_resp[..init_resp.len().min(200)])
        );

        // tools/call
        let call_frame = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": args,
            }
        });
        write_frame(&mut stream, &serde_json::to_vec(&call_frame).unwrap()).await?;
        let resp_bytes = read_frame(&mut stream).await?;

        let text = String::from_utf8_lossy(&resp_bytes).into_owned();
        eprintln!(
            "[mcp-team-stdio] tool response preview: {}",
            &text[..text.len().min(200)]
        );

        // Extract result string if JSON, otherwise return raw text
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(result) = v.get("result").and_then(|r| r.as_str()) {
                return Ok(result.to_owned());
            }
            if let Some(error) = v.get("error") {
                return Ok(format!("Error: {error}"));
            }
        }
        Ok(text)
    }
}

// ---------------------------------------------------------------------------
// Frame helpers
// ---------------------------------------------------------------------------

async fn write_frame(stream: &mut TcpStream, data: &[u8]) -> io::Result<()> {
    let len = (data.len() as u32).to_be_bytes();
    stream.write_all(&len).await?;
    stream.write_all(data).await?;
    stream.flush().await
}

async fn read_frame(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}
