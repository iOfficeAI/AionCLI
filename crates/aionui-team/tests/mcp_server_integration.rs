mod common;

use std::sync::Arc;

use aionui_api_types::WebSocketMessage;
use aionui_realtime::EventBroadcaster;
use aionui_team::mcp::protocol::{read_frame, write_frame};
use aionui_team::{Mailbox, TaskBoard, TeamAgent, TeamMcpServer, TeammateManager, TeammateRole};
use common::MockTeamRepo;
use serde_json::{Value, json};
use tokio::net::TcpStream;

// ---------------------------------------------------------------------------
// Test infrastructure
// ---------------------------------------------------------------------------

struct RecordingBroadcaster {
    events: std::sync::Mutex<Vec<WebSocketMessage<Value>>>,
}

impl RecordingBroadcaster {
    fn new() -> Self {
        Self {
            events: std::sync::Mutex::new(vec![]),
        }
    }
}

impl EventBroadcaster for RecordingBroadcaster {
    fn broadcast(&self, event: WebSocketMessage<Value>) {
        self.events.lock().unwrap().push(event);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_agents() -> Vec<TeamAgent> {
    vec![
        TeamAgent {
            slot_id: "lead-1".into(),
            name: "Leader".into(),
            role: TeammateRole::Lead,
            conversation_id: "conv-lead".into(),
            backend: "acp".into(),
            model: "claude".into(),
            custom_agent_id: None,
            status: None,
            conversation_type: None,
            cli_path: None,
        },
        TeamAgent {
            slot_id: "worker-1".into(),
            name: "Worker".into(),
            role: TeammateRole::Teammate,
            conversation_id: "conv-worker".into(),
            backend: "acp".into(),
            model: "claude".into(),
            custom_agent_id: None,
            status: None,
            conversation_type: None,
            cli_path: None,
        },
    ]
}

struct TestEnv {
    server: TeamMcpServer,
    _repo: Arc<MockTeamRepo>,
}

async fn setup() -> TestEnv {
    let repo = Arc::new(MockTeamRepo::new());
    let mailbox = Arc::new(Mailbox::new(repo.clone()));
    let task_board = Arc::new(TaskBoard::new(repo.clone()));
    let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
    let agents = make_agents();
    let scheduler = Arc::new(TeammateManager::new(
        "team-1".into(),
        &agents,
        mailbox,
        task_board,
        broadcaster,
    ));

    let server = TeamMcpServer::start("test-token-123".into(), scheduler)
        .await
        .unwrap();

    TestEnv {
        server,
        _repo: repo,
    }
}

async fn connect_and_init(port: u16, token: &str, slot_id: &str) -> TcpStream {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();

    let init_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "auth_token": token,
            "slot_id": slot_id,
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "1.0" }
        }
    });
    send_request(&mut stream, &init_req).await;
    let resp = read_response(&mut stream).await;
    assert!(resp["result"]["serverInfo"]["name"].is_string());

    stream
}

async fn send_request(stream: &mut TcpStream, request: &Value) {
    let data = serde_json::to_vec(request).unwrap();
    write_frame(stream, &data).await.unwrap();
}

async fn read_response(stream: &mut TcpStream) -> Value {
    let frame = read_frame(stream).await.unwrap();
    serde_json::from_slice(&frame).unwrap()
}

async fn call_tool(stream: &mut TcpStream, id: u64, tool: &str, args: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": tool,
            "arguments": args
        }
    });
    send_request(stream, &req).await;
    read_response(stream).await
}

fn extract_text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_error_response(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Tests: Connection & Authentication (MC-1, MC-2, MC-3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mc1_correct_token_connects() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    });
    send_request(&mut stream, &req).await;
    let resp = read_response(&mut stream).await;
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 10);

    env.server.stop();
}

#[tokio::test]
async fn mc2_wrong_token_rejected() {
    let env = setup().await;
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", env.server.port()))
        .await
        .unwrap();

    let init_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "auth_token": "wrong-token", "slot_id": "s1" }
    });
    send_request(&mut stream, &init_req).await;
    let resp = read_response(&mut stream).await;
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Authentication failed")
    );

    env.server.stop();
}

#[tokio::test]
async fn mc3_no_token_rejected() {
    let env = setup().await;
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", env.server.port()))
        .await
        .unwrap();

    let init_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });
    send_request(&mut stream, &init_req).await;
    let resp = read_response(&mut stream).await;
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Authentication failed")
    );

    env.server.stop();
}

// ---------------------------------------------------------------------------
// Tests: tools/list (TTL-1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tools_list_returns_all_10_tools() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let req = json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "tools/list"
    });
    send_request(&mut stream, &req).await;
    let resp = read_response(&mut stream).await;
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 10);

    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"team_send_message"));
    assert!(names.contains(&"team_spawn_agent"));
    assert!(names.contains(&"team_task_create"));
    assert!(names.contains(&"team_task_update"));
    assert!(names.contains(&"team_task_list"));
    assert!(names.contains(&"team_members"));
    assert!(names.contains(&"team_rename_agent"));
    assert!(names.contains(&"team_shutdown_agent"));
    assert!(names.contains(&"team_list_models"));
    assert!(names.contains(&"team_describe_assistant"));

    env.server.stop();
}

// ---------------------------------------------------------------------------
// Tests: team_send_message (TS-1, TS-2, TS-3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ts1_send_message_to_agent() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let resp = call_tool(
        &mut stream,
        2,
        "team_send_message",
        json!({"to": "worker-1", "message": "Hello worker"}),
    )
    .await;

    assert!(!is_error_response(&resp));
    let text = extract_text(&resp);
    assert!(text.contains("worker-1"));

    env.server.stop();
}

#[tokio::test]
async fn ts2_broadcast_message() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let resp = call_tool(
        &mut stream,
        2,
        "team_send_message",
        json!({"to": "*", "message": "Attention all"}),
    )
    .await;

    assert!(!is_error_response(&resp));

    env.server.stop();
}

#[tokio::test]
async fn ts3_send_message_to_nonexistent_agent() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let resp = call_tool(
        &mut stream,
        2,
        "team_send_message",
        json!({"to": "nonexistent", "message": "Hello?"}),
    )
    .await;

    // Mailbox accepts writes to any agent_id; message is written but never read.
    // This is by design: the mailbox layer doesn't enforce agent existence.
    assert!(!is_error_response(&resp));
    let text = extract_text(&resp);
    assert!(text.contains("nonexistent"));

    env.server.stop();
}

// ---------------------------------------------------------------------------
// Tests: team_spawn_agent (SP-1, SP-2, SP-3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sp1_lead_spawns_whitelisted_agent() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let resp = call_tool(
        &mut stream,
        2,
        "team_spawn_agent",
        json!({"name": "Helper", "role": "worker", "backend": "claude"}),
    )
    .await;

    assert!(!is_error_response(&resp));
    let text = extract_text(&resp);
    assert!(text.contains("Helper"));
    assert!(text.contains("spawn"));

    env.server.stop();
}

#[tokio::test]
async fn sp2_non_whitelisted_backend_rejected() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let resp = call_tool(
        &mut stream,
        2,
        "team_spawn_agent",
        json!({"name": "X", "backend": "malicious"}),
    )
    .await;

    assert!(is_error_response(&resp));
    let text = extract_text(&resp);
    assert!(text.contains("not allowed"));

    env.server.stop();
}

#[tokio::test]
async fn sp3_teammate_cannot_spawn() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "worker-1").await;

    let resp = call_tool(
        &mut stream,
        2,
        "team_spawn_agent",
        json!({"name": "Helper", "backend": "claude"}),
    )
    .await;

    assert!(is_error_response(&resp));
    let text = extract_text(&resp);
    assert!(text.contains("Only Lead"));

    env.server.stop();
}

// ---------------------------------------------------------------------------
// Tests: team_task_create / team_task_list (TTC-1, TTC-2, TTL-1, TTL-2)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ttc1_create_basic_task() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let resp = call_tool(
        &mut stream,
        2,
        "team_task_create",
        json!({"subject": "Implement feature X"}),
    )
    .await;

    assert!(!is_error_response(&resp));
    let text = extract_text(&resp);
    assert!(text.contains("Implement feature X"));

    env.server.stop();
}

#[tokio::test]
async fn ttc2_create_task_with_dependency() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    call_tool(
        &mut stream,
        2,
        "team_task_create",
        json!({"subject": "Task A"}),
    )
    .await;

    let list_resp = call_tool(&mut stream, 3, "team_task_list", json!({})).await;
    let tasks: Vec<Value> = serde_json::from_str(&extract_text(&list_resp)).unwrap();
    let task_a_id = tasks[0]["id"].as_str().unwrap();

    let resp = call_tool(
        &mut stream,
        4,
        "team_task_create",
        json!({"subject": "Task B", "blocked_by": [task_a_id]}),
    )
    .await;

    assert!(!is_error_response(&resp));

    let list_resp2 = call_tool(&mut stream, 5, "team_task_list", json!({})).await;
    let tasks2: Vec<Value> = serde_json::from_str(&extract_text(&list_resp2)).unwrap();
    assert_eq!(tasks2.len(), 2);

    let task_b = tasks2.iter().find(|t| t["subject"] == "Task B").unwrap();
    let blocked_by: Vec<String> =
        serde_json::from_value(task_b["blocked_by"].clone()).unwrap_or_default();
    assert!(blocked_by.contains(&task_a_id.to_string()));

    env.server.stop();
}

#[tokio::test]
async fn ttl2_task_list_empty() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let resp = call_tool(&mut stream, 2, "team_task_list", json!({})).await;

    assert!(!is_error_response(&resp));
    let text = extract_text(&resp);
    let tasks: Vec<Value> = serde_json::from_str(&text).unwrap();
    assert!(tasks.is_empty());

    env.server.stop();
}

#[tokio::test]
async fn ttl1_task_list_after_create() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    call_tool(
        &mut stream,
        2,
        "team_task_create",
        json!({"subject": "Task A"}),
    )
    .await;

    let resp = call_tool(&mut stream, 3, "team_task_list", json!({})).await;
    let text = extract_text(&resp);
    let tasks: Vec<Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["subject"], "Task A");

    env.server.stop();
}

// ---------------------------------------------------------------------------
// Tests: team_task_update (TTU-1, TTU-2, TTU-3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ttu1_update_task_status() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    call_tool(
        &mut stream,
        2,
        "team_task_create",
        json!({"subject": "Task A"}),
    )
    .await;

    let list_resp = call_tool(&mut stream, 3, "team_task_list", json!({})).await;
    let tasks: Vec<Value> = serde_json::from_str(&extract_text(&list_resp)).unwrap();
    let task_id = tasks[0]["id"].as_str().unwrap();

    let resp = call_tool(
        &mut stream,
        4,
        "team_task_update",
        json!({"task_id": task_id, "status": "completed"}),
    )
    .await;

    assert!(!is_error_response(&resp));

    let list_resp2 = call_tool(&mut stream, 5, "team_task_list", json!({})).await;
    let tasks2: Vec<Value> = serde_json::from_str(&extract_text(&list_resp2)).unwrap();
    assert_eq!(tasks2[0]["status"], "completed");

    env.server.stop();
}

#[tokio::test]
async fn ttu3_update_nonexistent_task() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let resp = call_tool(
        &mut stream,
        2,
        "team_task_update",
        json!({"task_id": "nonexistent-id", "status": "completed"}),
    )
    .await;

    assert!(is_error_response(&resp));

    env.server.stop();
}

// ---------------------------------------------------------------------------
// Tests: team_members (TM-1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tm1_list_all_members() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let resp = call_tool(&mut stream, 2, "team_members", json!({})).await;

    assert!(!is_error_response(&resp));
    let text = extract_text(&resp);
    let members: Vec<Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(members.len(), 2);

    let names: Vec<&str> = members
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"Leader"));
    assert!(names.contains(&"Worker"));

    env.server.stop();
}

// ---------------------------------------------------------------------------
// Tests: team_rename_agent (TRA-1, TRA-2)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tra1_rename_existing_agent() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let resp = call_tool(
        &mut stream,
        2,
        "team_rename_agent",
        json!({"slot_id": "worker-1", "new_name": "Senior Worker"}),
    )
    .await;

    assert!(!is_error_response(&resp));
    let text = extract_text(&resp);
    assert!(text.contains("renamed"));

    env.server.stop();
}

#[tokio::test]
async fn tra2_rename_nonexistent_agent() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let resp = call_tool(
        &mut stream,
        2,
        "team_rename_agent",
        json!({"slot_id": "nonexistent", "new_name": "X"}),
    )
    .await;

    assert!(is_error_response(&resp));

    env.server.stop();
}

// ---------------------------------------------------------------------------
// Tests: team_shutdown_agent (TSA-1, TSA-4)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tsa1_lead_sends_shutdown_request() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let resp = call_tool(
        &mut stream,
        2,
        "team_shutdown_agent",
        json!({"slot_id": "worker-1", "reason": "Task complete"}),
    )
    .await;

    assert!(!is_error_response(&resp));
    let text = extract_text(&resp);
    assert!(text.contains("Shutdown request sent"));

    env.server.stop();
}

#[tokio::test]
async fn tsa4_non_lead_cannot_shutdown() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "worker-1").await;

    let resp = call_tool(
        &mut stream,
        2,
        "team_shutdown_agent",
        json!({"slot_id": "lead-1"}),
    )
    .await;

    assert!(is_error_response(&resp));
    let text = extract_text(&resp);
    assert!(text.contains("Only Lead"));

    env.server.stop();
}

// ---------------------------------------------------------------------------
// Tests: Unknown method / non-initialize first request
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_method_returns_error() {
    let env = setup().await;
    let mut stream = connect_and_init(env.server.port(), "test-token-123", "lead-1").await;

    let req = json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "unknown/method"
    });
    send_request(&mut stream, &req).await;
    let resp = read_response(&mut stream).await;
    assert!(resp["error"]["code"].as_i64().unwrap() == -32601);

    env.server.stop();
}

#[tokio::test]
async fn non_initialize_first_request_rejected() {
    let env = setup().await;
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", env.server.port()))
        .await
        .unwrap();

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list"
    });
    send_request(&mut stream, &req).await;
    let resp = read_response(&mut stream).await;
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("initialize")
    );

    env.server.stop();
}

// ---------------------------------------------------------------------------
// Tests: Server stop (SS-2)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ss2_stop_server_closes_listener() {
    let env = setup().await;
    let port = env.server.port();

    let _stream = connect_and_init(port, "test-token-123", "lead-1").await;
    env.server.stop();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let result = TcpStream::connect(format!("127.0.0.1:{port}")).await;
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Tests: stdio bridge config (SB-1, SB-3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sb1_bridge_config_generation() {
    use aionui_team::{TeamMcpStdioConfig, TeamMcpStdioServerSpec};

    let env = setup().await;
    let config = TeamMcpStdioConfig {
        team_id: "team-test".into(),
        port: env.server.port(),
        token: env.server.auth_token().to_string(),
        slot_id: "lead-1".into(),
    };

    let spec = TeamMcpStdioServerSpec::from_config("/bin/aionui-backend", &config);
    let env_map: std::collections::HashMap<_, _> = spec.env.iter().cloned().collect();
    assert_eq!(
        env_map[TeamMcpStdioConfig::ENV_PORT],
        env.server.port().to_string()
    );
    assert_eq!(env_map[TeamMcpStdioConfig::ENV_TOKEN], "test-token-123");
    assert_eq!(env_map[TeamMcpStdioConfig::ENV_SLOT_ID], "lead-1");

    env.server.stop();
}

#[tokio::test]
async fn sb3_different_agents_get_different_slot_ids() {
    use aionui_team::{TeamMcpStdioConfig, TeamMcpStdioServerSpec};

    let env = setup().await;
    let port = env.server.port();
    let token = env.server.auth_token().to_string();

    let cfg_lead = TeamMcpStdioConfig {
        team_id: "t".into(),
        port,
        token: token.clone(),
        slot_id: "lead-1".into(),
    };
    let cfg_worker = TeamMcpStdioConfig {
        team_id: "t".into(),
        port,
        token,
        slot_id: "worker-1".into(),
    };
    let spec_lead = TeamMcpStdioServerSpec::from_config("/b", &cfg_lead);
    let spec_worker = TeamMcpStdioServerSpec::from_config("/b", &cfg_worker);
    let kv_lead: std::collections::HashMap<_, _> = spec_lead.env.iter().cloned().collect();
    let kv_worker: std::collections::HashMap<_, _> = spec_worker.env.iter().cloned().collect();

    assert_eq!(
        kv_lead[TeamMcpStdioConfig::ENV_PORT],
        kv_worker[TeamMcpStdioConfig::ENV_PORT]
    );
    assert_ne!(
        kv_lead[TeamMcpStdioConfig::ENV_SLOT_ID],
        kv_worker[TeamMcpStdioConfig::ENV_SLOT_ID]
    );

    env.server.stop();
}
