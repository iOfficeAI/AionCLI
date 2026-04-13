//! E2E integration tests for auxiliary conversation routes.
//!
//! Tests cover: workspace browse, side-question, reload-context,
//! slash-commands, and openclaw-runtime endpoints.

mod common;

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use common::{body_json, get_with_token, json_with_token, setup_and_login};

// ── Helpers ─────────────────────────────────────────────────────

fn create_conv_body(name: &str, agent_type: &str) -> serde_json::Value {
    json!({
        "type": agent_type,
        "name": name,
        "model": { "providerId": "p1", "model": "m1" },
        "extra": { "workspace": "/project" }
    })
}

async fn create_conversation(
    app: &mut axum::Router,
    token: &str,
    csrf: &str,
    name: &str,
    agent_type: &str,
) -> String {
    let req = common::json_with_token(
        "POST",
        "/api/conversations",
        create_conv_body(name, agent_type),
        token,
        csrf,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = common::body_json(resp).await;
    json["data"]["id"].as_str().unwrap().to_owned()
}

async fn build_app() -> (axum::Router, aionui_app::AppServices) {
    let db = aionui_db::init_database_memory().await.unwrap();
    let services = aionui_app::AppServices::from_database(db).await.unwrap();
    let router = aionui_app::create_router(&services);
    (router, services)
}

// ── 9.1 Workspace browse ────────────────────────────────────────

#[tokio::test]
async fn workspace_browse_requires_auth() {
    let (app, _) = build_app().await;
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/api/conversations/test-conv/workspace?path=/src")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn workspace_browse_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;
    let conv_id = create_conversation(&mut app, &token, &csrf, "Test Conv", "gemini").await;

    let req = get_with_token(
        &format!("/api/conversations/{conv_id}/workspace?path=/src"),
        &token,
    );
    let resp = app.oneshot(req).await.unwrap();
    // No active agent task → 404
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn workspace_browse_empty_path() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = get_with_token(
        "/api/conversations/some-conv/workspace?path=",
        &token,
    );
    let resp = app.oneshot(req).await.unwrap();
    // Empty path should return 400 (validated before agent lookup)
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── 9.2 Side question ───────────────────────────────────────────

#[tokio::test]
async fn side_question_requires_auth() {
    let (app, _) = build_app().await;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/conversations/test-conv/side-question")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(r#"{"question":"test?"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn side_question_empty_question() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "POST",
        "/api/conversations/some-conv/side-question",
        json!({ "question": "" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn side_question_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;
    let conv_id = create_conversation(&mut app, &token, &csrf, "Side Q Test", "gemini").await;

    let req = json_with_token(
        "POST",
        &format!("/api/conversations/{conv_id}/side-question"),
        json!({ "question": "What is this?" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    // No active agent → 404
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── 9.3 Reload context ─────────────────────────────────────────

#[tokio::test]
async fn reload_context_requires_auth() {
    let (app, _) = build_app().await;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/conversations/test-conv/reload-context")
        .header("content-type", "application/json")
        .body(axum::body::Body::from("{}"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn reload_context_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;
    let conv_id =
        create_conversation(&mut app, &token, &csrf, "Reload Test", "gemini").await;

    let req = json_with_token(
        "POST",
        &format!("/api/conversations/{conv_id}/reload-context"),
        json!({}),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── 9.4 Slash commands ──────────────────────────────────────────

#[tokio::test]
async fn slash_commands_requires_auth() {
    let (app, _) = build_app().await;
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/api/conversations/test-conv/slash-commands")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn slash_commands_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;
    let conv_id =
        create_conversation(&mut app, &token, &_csrf, "Slash Test", "gemini").await;

    let req = get_with_token(
        &format!("/api/conversations/{conv_id}/slash-commands"),
        &token,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── 9.5 OpenClaw runtime ────────────────────────────────────────

#[tokio::test]
async fn openclaw_runtime_requires_auth() {
    let (app, _) = build_app().await;
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/api/conversations/test-conv/openclaw/runtime")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn openclaw_runtime_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;
    let conv_id =
        create_conversation(&mut app, &token, &_csrf, "OpenClaw Test", "openclawGateway")
            .await;

    let req = get_with_token(
        &format!("/api/conversations/{conv_id}/openclaw/runtime"),
        &token,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Confirmation routes (no active task → graceful defaults) ─────

#[tokio::test]
async fn list_confirmations_no_task() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;
    let conv_id =
        create_conversation(&mut app, &token, &csrf, "Confirm Test", "acp").await;

    let req = get_with_token(
        &format!("/api/conversations/{conv_id}/confirmations"),
        &token,
    );
    let resp = app.oneshot(req).await.unwrap();
    // No active agent → returns empty list gracefully
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json["data"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn confirm_call_no_task() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;
    let conv_id =
        create_conversation(&mut app, &token, &csrf, "Confirm Test", "acp").await;

    let req = json_with_token(
        "POST",
        &format!("/api/conversations/{conv_id}/confirmations/call-1/confirm"),
        json!({ "msgId": "msg-1", "data": { "value": "allow" }, "alwaysAllow": false }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn check_approval_no_task() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;
    let conv_id =
        create_conversation(&mut app, &token, &csrf, "Approval Test", "acp").await;

    let req = get_with_token(
        &format!("/api/conversations/{conv_id}/approvals/check?action=edit_file"),
        &token,
    );
    let resp = app.oneshot(req).await.unwrap();
    // No active agent → returns approved=false gracefully
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["data"]["approved"], false);
}

// ── Stop + Warmup (no active task → error) ────────────────────

#[tokio::test]
async fn stop_stream_no_task() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;
    let conv_id =
        create_conversation(&mut app, &token, &csrf, "Stop Test", "gemini").await;

    let req = json_with_token(
        "POST",
        &format!("/api/conversations/{conv_id}/stop"),
        json!({}),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    // Stop with no active agent → 409 Conflict
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}
