//! E2E integration tests for ACP management routes.
//!
//! Tests cover: detect-cli, agents list, agents/refresh, agents/test,
//! health-check, env, probe-model, and session-bound routes (mode/model/config).

mod common;

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use common::{body_json, build_app, get_with_token, json_with_token, setup_and_login};

// ── Global ACP routes ────────────────────────────────────────────

#[tokio::test]
async fn detect_cli_requires_auth() {
    let (app, _services) = build_app().await;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/acp/detect-cli")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(r#"{"backend":"claude"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn detect_cli_returns_path_or_null() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "POST",
        "/api/acp/detect-cli",
        json!({ "backend": "claude" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    // path is either a string or absent (null) — both are valid
}

#[tokio::test]
async fn detect_cli_invalid_backend() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "POST",
        "/api/acp/detect-cli",
        json!({ "backend": "nonexistent_backend" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    // Invalid backend should fail deserialization → 400
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn detect_cli_non_cli_backend_returns_no_path() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    // "custom" backend has no CLI binary
    let req = json_with_token(
        "POST",
        "/api/acp/detect-cli",
        json!({ "backend": "custom" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"]["path"].is_null() || body["data"].get("path").is_none());
}

#[tokio::test]
async fn list_agents_returns_array() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = get_with_token("/api/acp/agents", &token);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"].is_array());
    let agents = body["data"].as_array().unwrap();
    assert!(agents.iter().any(|a| a["backend"] == "aionrs"));
}

#[tokio::test]
async fn refresh_agents_returns_array() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token("POST", "/api/acp/agents/refresh", json!({}), &token, &csrf);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"].is_array());
}

#[tokio::test]
async fn test_custom_agent_nonexistent_command() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "POST",
        "/api/acp/agents/test",
        json!({ "command": "/nonexistent/path/to/agent" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn health_check_returns_status() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "POST",
        "/api/acp/health-check",
        json!({ "backend": "claude" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    // available is a boolean
    assert!(body["data"]["available"].is_boolean());
    // latency should be present
    assert!(body["data"]["latency"].is_number());
}

#[tokio::test]
async fn health_check_non_cli_backend() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "POST",
        "/api/acp/health-check",
        json!({ "backend": "custom" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["data"]["available"], false);
    assert!(body["data"]["error"].is_string());
}

#[tokio::test]
async fn get_env_returns_env_map() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = get_with_token("/api/acp/env", &token);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"]["env"].is_object());
}

#[tokio::test]
async fn probe_model_non_cli_backend() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "POST",
        "/api/acp/probe-model",
        json!({ "backend": "custom" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    // custom backend has no CLI, so probe fails
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── Session-bound ACP routes (no active task → 404) ──────────────

#[tokio::test]
async fn get_mode_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = get_with_token("/api/conversations/nonexistent/acp/mode", &token);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn set_mode_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "PUT",
        "/api/conversations/nonexistent/acp/mode",
        json!({ "mode": "code" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_model_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = get_with_token("/api/conversations/nonexistent/acp/model", &token);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn set_model_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "PUT",
        "/api/conversations/nonexistent/acp/model",
        json!({ "model_id": "claude-sonnet-4" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_config_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = get_with_token("/api/conversations/nonexistent/acp/config", &token);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn set_config_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "PUT",
        "/api/conversations/nonexistent/acp/config/theme",
        json!({ "value": "dark" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
