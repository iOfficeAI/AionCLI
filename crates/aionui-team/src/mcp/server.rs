use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tracing::{debug, error, warn};

use crate::error::TeamError;
use crate::scheduler::TeammateManager;
use crate::types::TeammateRole;

use super::protocol::{
    INVALID_PARAMS, INVALID_REQUEST, JsonRpcResponse, METHOD_NOT_FOUND, PROTOCOL_VERSION,
    SERVER_NAME, SERVER_VERSION, read_request, write_response,
};
use super::tools::{
    RenameAgentInput, SendMessageInput, ShutdownAgentInput, SpawnAgentInput, TaskCreateInput,
    TaskUpdateInput, all_tool_descriptors, handle_team_describe_assistant, handle_team_list_models,
    is_whitelisted_backend,
};

// ---------------------------------------------------------------------------
// TeamMcpServer
// ---------------------------------------------------------------------------

pub struct TeamMcpServer {
    addr: SocketAddr,
    auth_token: String,
    shutdown_tx: watch::Sender<bool>,
}

impl TeamMcpServer {
    pub async fn start(
        auth_token: String,
        scheduler: Arc<TeammateManager>,
    ) -> Result<Self, TeamError> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| TeamError::InvalidRequest(format!("Failed to bind TCP: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| TeamError::InvalidRequest(format!("Failed to get local addr: {e}")))?;

        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let token = auth_token.clone();
        tokio::spawn(accept_loop(listener, token, scheduler, shutdown_rx));

        debug!(port = addr.port(), "Team MCP Server started");

        Ok(Self {
            addr,
            auth_token,
            shutdown_tx,
        })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn auth_token(&self) -> &str {
        &self.auth_token
    }

    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
        debug!(port = self.addr.port(), "Team MCP Server stop requested");
    }
}

impl Drop for TeamMcpServer {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
    }
}

// ---------------------------------------------------------------------------
// Accept loop
// ---------------------------------------------------------------------------

async fn accept_loop(
    listener: TcpListener,
    auth_token: String,
    scheduler: Arc<TeammateManager>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, peer)) => {
                        debug!(?peer, "New MCP connection");
                        let token = auth_token.clone();
                        let sched = Arc::clone(&scheduler);
                        tokio::spawn(handle_connection(stream, token, sched));
                    }
                    Err(e) => {
                        error!("Accept error: {e}");
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    debug!("MCP server shutting down");
                    break;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Connection handler
// ---------------------------------------------------------------------------

async fn handle_connection(stream: TcpStream, auth_token: String, scheduler: Arc<TeammateManager>) {
    let (mut reader, mut writer) = tokio::io::split(stream);

    let mut authenticated = false;
    let mut caller_slot_id: Option<String> = None;

    loop {
        let request = match read_request(&mut reader).await {
            Ok(req) => req,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => {
                warn!("Read error: {e}");
                break;
            }
        };

        if request.id.is_none() {
            continue;
        }

        let response = if !authenticated {
            match handle_initialize(&request, &auth_token) {
                InitResult::Authenticated(slot_id, resp) => {
                    authenticated = true;
                    caller_slot_id = Some(slot_id);
                    resp
                }
                InitResult::Response(resp) => resp,
            }
        } else {
            handle_method(
                &request,
                &scheduler,
                caller_slot_id.as_deref().unwrap_or("unknown"),
            )
            .await
        };

        if write_response(&mut writer, &response).await.is_err() {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Initialize / handshake
// ---------------------------------------------------------------------------

enum InitResult {
    Authenticated(String, JsonRpcResponse),
    Response(JsonRpcResponse),
}

fn handle_initialize(request: &super::protocol::JsonRpcRequest, auth_token: &str) -> InitResult {
    if request.method != "initialize" {
        return InitResult::Response(JsonRpcResponse::error(
            request.id,
            INVALID_REQUEST,
            "Expected 'initialize' as first request",
        ));
    }

    let params = request.params.as_ref();

    let token = params
        .and_then(|p| p.get("auth_token"))
        .or_else(|| params.and_then(|p| p.get("authToken")))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if token != auth_token {
        return InitResult::Response(JsonRpcResponse::error(
            request.id,
            INVALID_REQUEST,
            "Authentication failed: invalid auth_token",
        ));
    }

    let slot_id = params
        .and_then(|p| p.get("slot_id"))
        .or_else(|| params.and_then(|p| p.get("slotId")))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();

    let resp = JsonRpcResponse::success(
        request.id,
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION
            },
            "capabilities": {
                "tools": {}
            }
        }),
    );

    InitResult::Authenticated(slot_id, resp)
}

// ---------------------------------------------------------------------------
// Method router
// ---------------------------------------------------------------------------

async fn handle_method(
    request: &super::protocol::JsonRpcRequest,
    scheduler: &TeammateManager,
    caller_slot_id: &str,
) -> JsonRpcResponse {
    match request.method.as_str() {
        "notifications/initialized" => JsonRpcResponse::success(request.id, json!({})),
        "tools/list" => handle_tools_list(request.id),
        "tools/call" => handle_tools_call(request, scheduler, caller_slot_id).await,
        _ => JsonRpcResponse::error(
            request.id,
            METHOD_NOT_FOUND,
            format!("Unknown method: {}", request.method),
        ),
    }
}

fn handle_tools_list(id: Option<u64>) -> JsonRpcResponse {
    let tools = all_tool_descriptors();
    JsonRpcResponse::success(id, json!({ "tools": tools }))
}

// ---------------------------------------------------------------------------
// tools/call dispatcher
// ---------------------------------------------------------------------------

async fn handle_tools_call(
    request: &super::protocol::JsonRpcRequest,
    scheduler: &TeammateManager,
    caller_slot_id: &str,
) -> JsonRpcResponse {
    let params = match request.params.as_ref() {
        Some(p) => p,
        None => {
            return JsonRpcResponse::error(
                request.id,
                INVALID_PARAMS,
                "Missing params for tools/call",
            );
        }
    };

    let tool_name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return JsonRpcResponse::error(
                request.id,
                INVALID_PARAMS,
                "Missing 'name' in tools/call params",
            );
        }
    };

    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    let caller_role = match scheduler.get_agent(caller_slot_id).await {
        Ok(agent) => agent.role,
        Err(_) => TeammateRole::Teammate,
    };

    let result = dispatch_tool(
        tool_name,
        &arguments,
        scheduler,
        caller_slot_id,
        caller_role,
    )
    .await;

    match result {
        Ok(content) => JsonRpcResponse::success(
            request.id,
            json!({
                "content": [{ "type": "text", "text": content }]
            }),
        ),
        Err(err_msg) => JsonRpcResponse::success(
            request.id,
            json!({
                "content": [{ "type": "text", "text": err_msg }],
                "isError": true
            }),
        ),
    }
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

async fn dispatch_tool(
    tool_name: &str,
    arguments: &Value,
    scheduler: &TeammateManager,
    caller_slot_id: &str,
    caller_role: TeammateRole,
) -> Result<String, String> {
    match tool_name {
        "team_send_message" => exec_send_message(arguments, scheduler, caller_slot_id).await,
        "team_spawn_agent" => exec_spawn_agent(arguments, scheduler, caller_role).await,
        "team_task_create" => exec_task_create(arguments, scheduler).await,
        "team_task_update" => exec_task_update(arguments, scheduler).await,
        "team_task_list" => exec_task_list(scheduler).await,
        "team_members" => exec_members(scheduler).await,
        "team_rename_agent" => exec_rename_agent(arguments, scheduler).await,
        "team_shutdown_agent" => {
            exec_shutdown_agent(arguments, scheduler, caller_slot_id, caller_role).await
        }
        "team_list_models" => exec_list_models(arguments).await,
        "team_describe_assistant" => exec_describe_assistant(arguments).await,
        _ => Err(format!("Unknown tool: {tool_name}")),
    }
}

async fn exec_list_models(args: &Value) -> Result<String, String> {
    let value = handle_team_list_models(args);
    serde_json::to_string_pretty(&value).map_err(|e| format!("Serialization error: {e}"))
}

async fn exec_describe_assistant(args: &Value) -> Result<String, String> {
    Ok(handle_team_describe_assistant(args))
}

// ---------------------------------------------------------------------------
// Individual tool handlers
// ---------------------------------------------------------------------------

async fn exec_send_message(
    args: &Value,
    scheduler: &TeammateManager,
    caller_slot_id: &str,
) -> Result<String, String> {
    let input: SendMessageInput =
        serde_json::from_value(args.clone()).map_err(|e| format!("Invalid params: {e}"))?;

    let action = crate::scheduler::SchedulerAction::SendMessage {
        to: input.to.clone(),
        message: input.message,
    };
    scheduler
        .execute_action(caller_slot_id, &action)
        .await
        .map_err(|e| e.to_string())?;

    Ok(format!("Message sent to {}", input.to))
}

async fn exec_spawn_agent(
    args: &Value,
    scheduler: &TeammateManager,
    caller_role: TeammateRole,
) -> Result<String, String> {
    if caller_role != TeammateRole::Lead {
        return Err("Only Lead can spawn agents".into());
    }
    let input: SpawnAgentInput =
        serde_json::from_value(args.clone()).map_err(|e| format!("Invalid params: {e}"))?;

    if !is_whitelisted_backend(&input.backend) {
        return Err(format!(
            "Backend '{}' not allowed. Whitelist: claude, codex",
            input.backend
        ));
    }

    let action = crate::scheduler::SchedulerAction::SpawnAgent {
        name: input.name.clone(),
        role: input.role.unwrap_or_else(|| "teammate".into()),
        backend: input.backend,
    };
    scheduler
        .execute_action("lead", &action)
        .await
        .map_err(|e| e.to_string())?;

    Ok(format!("Agent '{}' spawn requested", input.name))
}

async fn exec_task_create(args: &Value, scheduler: &TeammateManager) -> Result<String, String> {
    let input: TaskCreateInput =
        serde_json::from_value(args.clone()).map_err(|e| format!("Invalid params: {e}"))?;

    let action = crate::scheduler::SchedulerAction::TaskCreate {
        subject: input.subject.clone(),
        description: input.description,
        owner: input.owner,
        blocked_by: input.blocked_by.unwrap_or_default(),
    };
    scheduler
        .execute_action("system", &action)
        .await
        .map_err(|e| e.to_string())?;

    Ok(format!("Task '{}' created", input.subject))
}

async fn exec_task_update(args: &Value, scheduler: &TeammateManager) -> Result<String, String> {
    let input: TaskUpdateInput =
        serde_json::from_value(args.clone()).map_err(|e| format!("Invalid params: {e}"))?;

    let action = crate::scheduler::SchedulerAction::TaskUpdate {
        task_id: input.task_id.clone(),
        status: input.status,
        description: input.description,
        owner: input.owner,
        blocked_by: input.blocked_by,
    };
    scheduler
        .execute_action("system", &action)
        .await
        .map_err(|e| e.to_string())?;

    Ok(format!("Task '{}' updated", input.task_id))
}

async fn exec_task_list(scheduler: &TeammateManager) -> Result<String, String> {
    let tasks = scheduler.list_tasks().await.map_err(|e| e.to_string())?;
    let output: Vec<Value> = tasks
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "subject": t.subject,
                "description": t.description,
                "status": t.status,
                "owner": t.owner,
                "blocked_by": t.blocked_by,
                "blocks": t.blocks,
            })
        })
        .collect();
    serde_json::to_string_pretty(&output).map_err(|e| format!("Serialization error: {e}"))
}

async fn exec_members(scheduler: &TeammateManager) -> Result<String, String> {
    let agents = scheduler.list_agents().await;
    let output: Vec<Value> = agents
        .iter()
        .map(|a| {
            json!({
                "slot_id": a.slot_id,
                "name": a.name,
                "role": a.role,
                "status": a.status,
                "backend": a.backend,
                "model": a.model,
            })
        })
        .collect();
    serde_json::to_string_pretty(&output).map_err(|e| format!("Serialization error: {e}"))
}

async fn exec_rename_agent(args: &Value, scheduler: &TeammateManager) -> Result<String, String> {
    let input: RenameAgentInput =
        serde_json::from_value(args.clone()).map_err(|e| format!("Invalid params: {e}"))?;

    scheduler
        .rename_agent(&input.slot_id, &input.new_name)
        .await
        .map_err(|e| e.to_string())?;

    Ok(format!(
        "Agent '{}' renamed to '{}'",
        input.slot_id, input.new_name
    ))
}

async fn exec_shutdown_agent(
    args: &Value,
    scheduler: &TeammateManager,
    caller_slot_id: &str,
    caller_role: TeammateRole,
) -> Result<String, String> {
    if caller_role != TeammateRole::Lead {
        return Err("Only Lead can shut down agents".into());
    }
    let input: ShutdownAgentInput =
        serde_json::from_value(args.clone()).map_err(|e| format!("Invalid params: {e}"))?;

    let action = crate::scheduler::SchedulerAction::ShutdownAgent {
        slot_id: input.slot_id.clone(),
        reason: input.reason,
    };
    scheduler
        .execute_action(caller_slot_id, &action)
        .await
        .map_err(|e| e.to_string())?;

    Ok(format!(
        "Shutdown request sent to agent '{}'",
        input.slot_id
    ))
}
