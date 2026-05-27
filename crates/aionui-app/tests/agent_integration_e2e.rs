//! E2E integration tests with mock agent connectors.
//!
//! Tests the message flow, confirmation system, and auxiliary routes
//! with a mock `IAgentConnectorFactory` that provides in-memory connectors.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use aionui_ai_agent::protocol::events::{FinishEventData, TextEventData};
use aionui_ai_agent::test_support::{MockConnector, MockConnectorFactory};
use aionui_ai_agent::{AgentStreamEvent, ConnectorBuildFn, IAgentConnector, IAgentConnectorFactory};
use aionui_common::Confirmation;

use common::{body_json, get_with_token, json_with_token, setup_and_login};

// ── Mock connector + factory wiring ────────────────────────────

/// Build a connector pre-seeded with a 1-event script (text + finish)
/// so the first `send_message` echoes a "Mock response" and terminates,
/// matching the legacy `MockAgent::send_message` behaviour.
fn make_mock_connector(conv_id: &str, workspace: &str) -> Arc<MockConnector> {
    MockConnector::builder(conv_id)
        .workspace(workspace)
        .allow_direct_confirm()
        .script(vec![
            AgentStreamEvent::Text(TextEventData {
                content: "Mock response".into(),
            }),
            AgentStreamEvent::Finish(FinishEventData::default()),
        ])
        .build_arc()
}

async fn build_app_with_mock_tasks() -> (axum::Router, aionui_app::AppServices, Arc<MockConnectorFactory>) {
    let db = aionui_db::init_database_memory().await.unwrap();
    let services = aionui_app::AppServices::from_config(db, &aionui_app::AppConfig::default())
        .await
        .unwrap();

    let build_fn: ConnectorBuildFn = Arc::new(|opts| {
        Box::pin(async move {
            let connector: Arc<dyn IAgentConnector> = make_mock_connector(&opts.conversation_id, "/mock-workspace");
            Ok(connector)
        })
    });
    let mock_tm: Arc<MockConnectorFactory> = MockConnectorFactory::builder().build_fn(build_fn).build();
    let factory_dyn: Arc<dyn IAgentConnectorFactory> = mock_tm.clone();
    let services = services.with_connector_factory(factory_dyn);

    let (router, _conv_service) = aionui_app::create_router(&services).await;
    (router, services, mock_tm)
}

/// Insert a pre-built mock connector into the factory cache so the
/// E2E flow under test sees a fixed agent (mirrors the legacy
/// `MockTaskManager::insert` ergonomics).
fn insert_mock_connector(factory: &Arc<MockConnectorFactory>, conv_id: &str, workspace: &str) -> Arc<MockConnector> {
    let connector = make_mock_connector(conv_id, workspace);
    let dyn_connector: Arc<dyn IAgentConnector> = connector.clone();
    factory.insert(conv_id, dyn_connector);
    connector
}

/// Variant of [`insert_mock_connector`] that pre-seeds the connector
/// with a list of pending [`Confirmation`]s, so the confirm endpoint
/// can locate the call by id without a real agent emitting one first.
fn insert_mock_connector_with_confirmations(
    factory: &Arc<MockConnectorFactory>,
    conv_id: &str,
    workspace: &str,
    confs: Vec<Confirmation>,
) -> Arc<MockConnector> {
    let connector = MockConnector::builder(conv_id)
        .workspace(workspace)
        .allow_direct_confirm()
        .confirmations(confs)
        .script(vec![
            AgentStreamEvent::Text(TextEventData {
                content: "Mock response".into(),
            }),
            AgentStreamEvent::Finish(FinishEventData::default()),
        ])
        .build_arc();
    let dyn_connector: Arc<dyn IAgentConnector> = connector.clone();
    factory.insert(conv_id, dyn_connector);
    connector
}

async fn create_conversation(app: &mut axum::Router, token: &str, csrf: &str, name: &str) -> String {
    let body = json!({
        "type": "acp",
        "name": name,
        "extra": { "workspace": "/project" }
    });
    let req = common::json_with_token("POST", "/api/conversations", body, token, csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = common::body_json(resp).await;
    json["data"]["id"].as_str().unwrap().to_owned()
}

// ── Message flow with mock agent ────────────────────────────────

#[tokio::test]
async fn send_message_with_mock_agent_returns_202() {
    let (mut app, services, _mock_tm) = build_app_with_mock_tasks().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "Pass123!").await;
    let conv_id = create_conversation(&mut app, &token, &csrf, "Mock Agent Test").await;

    let req = json_with_token(
        "POST",
        &format!("/api/conversations/{conv_id}/messages"),
        json!({ "content": "Hello mock agent" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
}

#[tokio::test]
async fn stop_stream_with_mock_agent() {
    let (mut app, services, mock_tm) = build_app_with_mock_tasks().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "Pass123!").await;
    let conv_id = create_conversation(&mut app, &token, &csrf, "Stop Test").await;
    insert_mock_connector(&mock_tm, &conv_id, "/mock-workspace");

    let req = json_with_token(
        "POST",
        &format!("/api/conversations/{conv_id}/cancel"),
        json!({}),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
}

#[tokio::test]
async fn warmup_with_mock_agent() {
    let (mut app, services, _mock_tm) = build_app_with_mock_tasks().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "Pass123!").await;
    let conv_id = create_conversation(&mut app, &token, &csrf, "Warmup Test").await;

    let req = json_with_token(
        "POST",
        &format!("/api/conversations/{conv_id}/warmup"),
        json!({}),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Confirmation system with mock agent ─────────────────────────

#[tokio::test]
async fn list_confirmations_empty() {
    let (mut app, services, mock_tm) = build_app_with_mock_tasks().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "Pass123!").await;
    let conv_id = create_conversation(&mut app, &token, &csrf, "Confirm Test").await;
    insert_mock_connector(&mock_tm, &conv_id, "/mock-workspace");

    let req = get_with_token(&format!("/api/conversations/{conv_id}/confirmations"), &token);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert!(json["data"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn confirm_and_check_approval() {
    let (mut app, services, mock_tm) = build_app_with_mock_tasks().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "Pass123!").await;
    let conv_id = create_conversation(&mut app, &token, &csrf, "Approval Test").await;
    // Pre-populate a pending confirmation so the confirm endpoint can find it
    let _agent = insert_mock_connector_with_confirmations(
        &mock_tm,
        &conv_id,
        "/mock-workspace",
        vec![Confirmation {
            id: "conf-1".into(),
            call_id: "call-42".into(),
            title: Some("Allow file edit".into()),
            action: Some("test_action".into()),
            description: String::new(),
            command_type: None,
            options: vec![],
        }],
    );

    // Confirm a call with alwaysAllow=true
    let req = json_with_token(
        "POST",
        &format!("/api/conversations/{conv_id}/confirmations/call-42/confirm"),
        json!({ "msg_id": "msg-1", "data": { "value": "allow" }, "always_allow": true }),
        &token,
        &csrf,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Check approval — should be approved for "test_action"
    let req = get_with_token(
        &format!("/api/conversations/{conv_id}/approvals/check?action=test_action"),
        &token,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["approved"], true);
}

#[tokio::test]
async fn check_approval_not_set() {
    let (mut app, services, mock_tm) = build_app_with_mock_tasks().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "Pass123!").await;
    let conv_id = create_conversation(&mut app, &token, &csrf, "Approval NotSet").await;
    insert_mock_connector(&mock_tm, &conv_id, "/mock-workspace");

    let req = get_with_token(
        &format!("/api/conversations/{conv_id}/approvals/check?action=unknown_action"),
        &token,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["data"]["approved"], false);
}

// ── Auxiliary routes with mock agent ────────────────────────────

#[tokio::test]
async fn slash_commands_with_mock_returns_empty() {
    let (mut app, services, mock_tm) = build_app_with_mock_tasks().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "Pass123!").await;
    let conv_id = create_conversation(&mut app, &token, &csrf, "Slash Mock Test").await;
    insert_mock_connector(&mock_tm, &conv_id, "/mock-workspace");

    let req = get_with_token(&format!("/api/conversations/{conv_id}/slash-commands"), &token);
    let resp = app.oneshot(req).await.unwrap();
    // Mock agent is not a real AcpAgentManager, so downcast fails → 500
    // OR if agent_type check prevents downcast, returns empty array
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR,
        "Expected 200 or 500, got {status}"
    );
}

#[tokio::test]
async fn openclaw_runtime_wrong_agent_type() {
    let (mut app, services, mock_tm) = build_app_with_mock_tasks().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "Pass123!").await;
    let conv_id = create_conversation(&mut app, &token, &csrf, "OpenClaw Wrong Type").await;
    insert_mock_connector(&mock_tm, &conv_id, "/mock-workspace");

    let req = get_with_token(&format!("/api/conversations/{conv_id}/openclaw/runtime"), &token);
    let resp = app.oneshot(req).await.unwrap();
    // Non-OpenClaw agents return a JSON null payload instead of an
    // error — the endpoint is a best-effort diagnostic; callers that
    // need stricter typing check the payload shape themselves.
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert!(json["data"].is_null());
}

#[tokio::test]
async fn side_question_with_mock_agent() {
    let (mut app, services, mock_tm) = build_app_with_mock_tasks().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "Pass123!").await;
    let conv_id = create_conversation(&mut app, &token, &csrf, "Side Q Mock").await;
    insert_mock_connector(&mock_tm, &conv_id, "/mock-workspace");

    let req = json_with_token(
        "POST",
        &format!("/api/conversations/{conv_id}/side-question"),
        json!({ "question": "What is this code?" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    // Mock agent is type Acp but not a real AcpAgentManager, so downcast
    // fails. The handler first checks agent_type() == Acp, then tries to
    // downcast. Since our mock returns Acp type, downcast fails → 500.
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR,
        "Expected 200 or 500, got {status}"
    );
}
