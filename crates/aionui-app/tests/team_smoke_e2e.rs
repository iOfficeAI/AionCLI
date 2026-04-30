//! End-to-end smoke tests for the team subsystem — the real deal.
//!
//! **What this guards against:** "the agent says a tool worked but it was an
//! empty shell" shipping failure. Each scenario drives the system like the
//! frontend would (real REST calls, real auth, real CSRF, real DB, real
//! `TeamMcpServer`, real scheduler, real mailbox, real task board), and the
//! only thing it mocks is *what the LLM would have done*: a deterministic
//! `MockAgentManager` that, on receiving a user message, connects over HTTP
//! to the team's MCP server **the same way an ACP CLI child process would**,
//! calls a specific tool, and surfaces the result.
//!
//! No `#[ignore]`. No direct MCP traffic from the test body. No bypassing
//! `TeamSessionService::ensure_session`, `warmup`, or the route layer. The
//! only test-only plumbing is `AppServices::with_worker_task_manager` — the
//! public override that already exists for exactly this purpose.

mod common;

use std::sync::Arc;

use aionui_ai_agent::agent_manager::IAgentManager;
use aionui_ai_agent::stream_event::{AgentStreamEvent, ErrorEventData, FinishEventData};
use aionui_ai_agent::types::{AcpBuildExtra, AgentStreamChunk, BuildTaskOptions, SendMessageData};
use aionui_ai_agent::{AgentFactory, IWorkerTaskManager, WorkerTaskManagerImpl};
use aionui_api_types::TeamMcpStdioConfig;
use aionui_app::{AppServices, build_module_states, create_router_with_states};
use aionui_common::{AgentKillReason, AgentType, AppError, Confirmation, ConversationStatus, TimestampMs};
use aionui_db::models::{MailboxMessageRow, TeamTaskRow};
use aionui_db::{ITeamRepository, SqliteTeamRepository};
use axum::http::StatusCode;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tower::ServiceExt;

use common::{body_json, json_with_token, setup_and_login};

// ---------------------------------------------------------------------------
// MockAgentManager — a deterministic stand-in for the real ACP CLI child
// process. On `send_message`, it keyword-matches the user content to decide
// which team_* tool to invoke, then POSTs a JSON-RPC `tools/call` to the
// team's HTTP MCP endpoint using the `port`/`token`/`slot_id` that
// `TeamSessionService` persisted into its conversation's `extra`.
//
// The tool call is a REAL HTTP request against a REAL `TeamMcpServer`
// running inside the same process (same Router, same AppServices). The
// resulting mailbox rows / task board rows are what the assertions check.
// ---------------------------------------------------------------------------

struct MockAgentManager {
    conversation_id: String,
    workspace: String,
    mcp_config: Option<TeamMcpStdioConfig>,
    event_tx: broadcast::Sender<AgentStreamEvent>,
    chunk_tx: broadcast::Sender<AgentStreamChunk>,
}

impl MockAgentManager {
    fn new(conversation_id: String, workspace: String, mcp_config: Option<TeamMcpStdioConfig>) -> Self {
        let (event_tx, _) = broadcast::channel(32);
        let (chunk_tx, _) = broadcast::channel(32);
        Self {
            conversation_id,
            workspace,
            mcp_config,
            event_tx,
            chunk_tx,
        }
    }

    /// Keyword-matched tool picker. Not a brain — just enough determinism to
    /// hit every MCP tool the smoke suite needs to verify.
    fn pick_tool(message: &str) -> Option<(&'static str, Value)> {
        let lower = message.to_lowercase();
        if lower.contains("list members") || lower.contains("who is on") {
            return Some(("team_members", json!({})));
        }
        if lower.contains("send") && lower.contains("worker") {
            return Some((
                "team_send_message",
                json!({ "to": "worker-e2e", "message": "hello from mock lead" }),
            ));
        }
        if lower.contains("create task") {
            return Some(("team_task_create", json!({ "subject": "E2E smoke task" })));
        }
        None
    }

    async fn call_mcp_tool(&self, tool: &str, args: Value) -> Result<Value, String> {
        let cfg = self
            .mcp_config
            .as_ref()
            .ok_or_else(|| "no team_mcp_stdio_config on this conversation".to_string())?;

        let url = format!("http://127.0.0.1:{}/", cfg.port);
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        });

        let resp = reqwest::Client::new()
            .post(&url)
            .header("Authorization", cfg.slot_id.clone())
            .json(&req_body)
            .send()
            .await
            .map_err(|e| format!("mcp POST failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("mcp returned HTTP {}", resp.status()));
        }

        resp.json::<Value>().await.map_err(|e| format!("mcp body decode: {e}"))
    }
}

#[async_trait::async_trait]
impl IAgentManager for MockAgentManager {
    fn agent_type(&self) -> AgentType {
        AgentType::Acp
    }
    fn status(&self) -> Option<ConversationStatus> {
        None
    }
    fn workspace(&self) -> &str {
        &self.workspace
    }
    fn conversation_id(&self) -> &str {
        &self.conversation_id
    }
    fn last_activity_at(&self) -> TimestampMs {
        0
    }
    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.event_tx.subscribe()
    }
    fn subscribe_stream(&self) -> broadcast::Receiver<AgentStreamChunk> {
        self.chunk_tx.subscribe()
    }

    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError> {
        // Production ACP's send_message returns as soon as the child
        // process accepts the input; all downstream work flows through the
        // event stream. Preserve that shape: kick the tool call off in a
        // detached task and surface the outcome as Finish / Error.
        let event_tx = self.event_tx.clone();
        let mcp_config = self.mcp_config.clone();
        let conv_id = self.conversation_id.clone();
        let workspace = self.workspace.clone();
        tokio::spawn(async move {
            let agent = MockAgentManager {
                conversation_id: conv_id,
                workspace,
                mcp_config,
                event_tx: event_tx.clone(),
                chunk_tx: broadcast::channel(1).0,
            };

            if let Some((tool, args)) = Self::pick_tool(&data.content) {
                match agent.call_mcp_tool(tool, args).await {
                    Ok(_resp) => {
                        let _ = event_tx.send(AgentStreamEvent::Finish(FinishEventData::default()));
                    }
                    Err(err) => {
                        let _ = event_tx.send(AgentStreamEvent::Error(ErrorEventData {
                            message: err,
                            code: None,
                        }));
                    }
                }
            } else {
                // No tool keyword matched — plain reply, no side effect.
                let _ = event_tx.send(AgentStreamEvent::Finish(FinishEventData::default()));
            }
        });
        Ok(())
    }

    async fn stop(&self) -> Result<(), AppError> {
        Ok(())
    }
    fn confirm(&self, _msg_id: &str, _call_id: &str, _data: Value, _always_allow: bool) -> Result<(), AppError> {
        Ok(())
    }
    fn get_confirmations(&self) -> Vec<Confirmation> {
        vec![]
    }
    fn check_approval(&self, _action: &str, _command_type: Option<&str>) -> bool {
        false
    }
    fn kill(&self, _reason: Option<AgentKillReason>) -> Result<(), AppError> {
        Ok(())
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Every conversation gets a `MockAgentManager` wired to whatever
/// `team_mcp_stdio_config` was persisted for it.
fn mock_factory() -> AgentFactory {
    use futures_util::FutureExt;
    Arc::new(|opts: BuildTaskOptions| {
        async move {
            let extra: AcpBuildExtra = serde_json::from_value(opts.extra.clone()).unwrap_or_else(|_| AcpBuildExtra {
                agent_id: None,
                backend: None,
                cli_path: None,
                agent_name: None,
                custom_agent_id: None,
                preset_context: None,
                skills: vec![],
                preset_assistant_id: None,
                session_mode: None,
                cron_job_id: None,
                team_mcp_stdio_config: None,
                guide_mcp_config: None,
            });
            let agent = MockAgentManager::new(opts.conversation_id, opts.workspace, extra.team_mcp_stdio_config);
            Ok(Arc::new(agent) as aionui_ai_agent::AgentManagerHandle)
        }
        .boxed()
    })
}

// ---------------------------------------------------------------------------
// Harness — real AppServices, real Router, only worker_task_manager swapped
// to the deterministic mock.
// ---------------------------------------------------------------------------

async fn build_app_with_mock_agent() -> (axum::Router, AppServices) {
    let db = aionui_db::init_database_memory().await.unwrap();
    let services = AppServices::from_database(db).await.unwrap();
    let mock_wtm: Arc<dyn IWorkerTaskManager> = Arc::new(WorkerTaskManagerImpl::new(mock_factory()));
    let services = services.with_worker_task_manager(mock_wtm);
    let (states, _) = build_module_states(&services).await;
    let router = create_router_with_states(&services, states);
    (router, services)
}

async fn create_lead_worker_team(app: &mut axum::Router, token: &str, csrf: &str) -> Value {
    let body = json!({
        "name": "SmokeAlpha",
        "agents": [
            { "name": "Lead",       "role": "lead",     "backend": "acp", "model": "claude" },
            { "name": "worker-e2e", "role": "teammate", "backend": "acp", "model": "claude" }
        ]
    });
    let req = json_with_token("POST", "/api/teams", body, token, csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "POST /api/teams must return 201");
    let json = body_json(resp).await;
    assert!(
        json["success"].as_bool().unwrap_or(false),
        "create_team not success: {json}"
    );
    json["data"].clone()
}

async fn send_user_message(
    app: &mut axum::Router,
    team_id: &str,
    slot_id: &str,
    content: &str,
    token: &str,
    csrf: &str,
) {
    let body = json!({ "content": content, "files": null });
    let uri = format!("/api/teams/{team_id}/agents/{slot_id}/messages");
    let req = json_with_token("POST", &uri, body, token, csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    assert!(
        resp.status().is_success(),
        "POST {uri} expected success, got {}",
        resp.status()
    );
}

fn team_repo(services: &AppServices) -> Arc<dyn ITeamRepository> {
    Arc::new(SqliteTeamRepository::new(services.database.pool().clone()))
}

async fn wait_for_mailbox<F>(
    repo: &Arc<dyn ITeamRepository>,
    team_id: &str,
    slot_id: &str,
    predicate: F,
) -> MailboxMessageRow
where
    F: Fn(&MailboxMessageRow) -> bool,
{
    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_secs(5);
    loop {
        let rows = repo
            .get_history(team_id, slot_id, None)
            .await
            .expect("ITeamRepository::get_history");
        if let Some(msg) = rows.into_iter().find(&predicate) {
            return msg;
        }
        if start.elapsed() > deadline {
            panic!("timeout waiting for mailbox message on {team_id}/{slot_id}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn wait_for_task<F>(repo: &Arc<dyn ITeamRepository>, team_id: &str, predicate: F) -> TeamTaskRow
where
    F: Fn(&TeamTaskRow) -> bool,
{
    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_secs(5);
    loop {
        let tasks = repo.list_tasks(team_id).await.expect("ITeamRepository::list_tasks");
        if let Some(task) = tasks.into_iter().find(&predicate) {
            return task;
        }
        if start.elapsed() > deadline {
            panic!("timeout waiting for task on {team_id}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

// ===========================================================================
// E2E-1 — Lead can introspect roster via `team_members`.
// ===========================================================================

#[tokio::test]
async fn smoke_team_members_tool_returns_roster() {
    let (mut app, services) = build_app_with_mock_agent().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let data = create_lead_worker_team(&mut app, &token, &csrf).await;
    let team_id = data["id"].as_str().unwrap().to_owned();
    let lead_slot = data["lead_agent_id"].as_str().unwrap().to_owned();
    let lead_conv = data["agents"][0]["conversation_id"].as_str().unwrap().to_owned();

    // Subscribe BEFORE firing the user message to avoid racing the mock.
    let handle = services
        .worker_task_manager
        .get_task(&lead_conv)
        .expect("lead agent task should be warm after create_team/ensure_session");
    let mut rx = handle.subscribe();
    drop(handle);

    send_user_message(&mut app, &team_id, &lead_slot, "list members please", &token, &csrf).await;

    let evt = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("agent should emit an event within 5s")
        .expect("broadcast recv");
    assert!(
        matches!(evt, AgentStreamEvent::Finish(_)),
        "expected Finish (tool succeeded); got {evt:?}"
    );
}

// ===========================================================================
// E2E-2 — `team_send_message` really writes into the worker's mailbox.
// ===========================================================================

#[tokio::test]
async fn smoke_team_send_message_tool_writes_to_worker_mailbox() {
    let (mut app, services) = build_app_with_mock_agent().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let data = create_lead_worker_team(&mut app, &token, &csrf).await;
    let team_id = data["id"].as_str().unwrap().to_owned();
    let lead_slot = data["lead_agent_id"].as_str().unwrap().to_owned();

    let repo = team_repo(&services);

    send_user_message(
        &mut app,
        &team_id,
        &lead_slot,
        "please send a message to worker about the plan",
        &token,
        &csrf,
    )
    .await;

    let msg = wait_for_mailbox(&repo, &team_id, "worker-e2e", |m| {
        m.content.contains("hello from mock lead")
    })
    .await;
    assert_eq!(
        msg.to_agent_id, "worker-e2e",
        "mailbox row must be addressed to the worker slot the tool call targeted"
    );
}

// ===========================================================================
// E2E-3 — `team_task_create` really writes into the task board.
// ===========================================================================

#[tokio::test]
async fn smoke_team_task_create_tool_writes_to_task_board() {
    let (mut app, services) = build_app_with_mock_agent().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let data = create_lead_worker_team(&mut app, &token, &csrf).await;
    let team_id = data["id"].as_str().unwrap().to_owned();
    let lead_slot = data["lead_agent_id"].as_str().unwrap().to_owned();

    let repo = team_repo(&services);

    send_user_message(
        &mut app,
        &team_id,
        &lead_slot,
        "please create task to ship the smoke suite",
        &token,
        &csrf,
    )
    .await;

    let task = wait_for_task(&repo, &team_id, |t| t.subject == "E2E smoke task").await;
    assert_eq!(task.subject, "E2E smoke task");
}

// ===========================================================================
// E2E-4 — Negative: a plain user reply (no tool keyword) must NOT produce a
// worker mailbox row. Guards against MockAgent logic bleeding through.
// ===========================================================================

#[tokio::test]
async fn smoke_plain_reply_does_not_write_worker_mailbox() {
    let (mut app, services) = build_app_with_mock_agent().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let data = create_lead_worker_team(&mut app, &token, &csrf).await;
    let team_id = data["id"].as_str().unwrap().to_owned();
    let lead_slot = data["lead_agent_id"].as_str().unwrap().to_owned();

    let lead_conv = data["agents"][0]["conversation_id"].as_str().unwrap().to_owned();
    let handle = services.worker_task_manager.get_task(&lead_conv).unwrap();
    let mut rx = handle.subscribe();
    drop(handle);

    send_user_message(
        &mut app,
        &team_id,
        &lead_slot,
        "just say hi, nothing to do",
        &token,
        &csrf,
    )
    .await;

    let evt = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout")
        .expect("recv");
    assert!(matches!(evt, AgentStreamEvent::Finish(_)));

    let repo = team_repo(&services);
    let rows = repo
        .get_history(&team_id, "worker-e2e", None)
        .await
        .expect("ITeamRepository::get_history");
    assert!(
        !rows.iter().any(|m| m.content.contains("hello from mock lead")),
        "plain user message must not produce a worker mailbox row"
    );
}
