//! E2E integration tests for ACP management routes.
//!
//! Tests cover: agents list, agents/refresh, agents/test, health-check,
//! and session-bound routes (mode/model).

mod common;

use aionui_db::UpsertAgentMetadataParams;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;

use common::{body_json, build_app, extract_csrf_token, get_request, get_with_token, json_with_token, setup_and_login};

// ── Global ACP routes ────────────────────────────────────────────

#[tokio::test]
async fn list_agents_returns_array() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = get_with_token("/api/agents", &token);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"].is_array());
    let agents = body["data"].as_array().unwrap();
    assert!(agents.iter().any(|a| a["agent_type"] == "aionrs"));
}

#[tokio::test]
async fn refresh_agents_returns_array() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token("POST", "/api/agents/refresh", json!({}), &token, &csrf);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"].is_array());
}

#[tokio::test]
async fn warmup_agents_empty_backends_returns_empty_results() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "POST",
        "/api/agents/warmup",
        json!({ "backends": [], "reason": "idle" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["results"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn warmup_agents_unknown_backend_is_structured_skip() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "POST",
        "/api/agents/warmup",
        json!({ "backends": ["not-real-agent"], "reason": "user_select" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["results"][0]["backend"], "not-real-agent");
    assert_eq!(body["data"]["results"][0]["status"], "skipped");
    assert!(
        body["data"]["results"][0]["error"]
            .as_str()
            .unwrap()
            .contains("not registered")
    );
}

#[tokio::test]
async fn warmup_agents_missing_builtin_cli_is_structured_skip() {
    let (mut app, services) = build_app().await;
    services
        .agent_registry
        .repo_handle()
        .upsert(&UpsertAgentMetadataParams {
            id: "missing-warmup",
            icon: None,
            name: "Missing Warmup Agent",
            name_i18n: None,
            description: Some("missing warmup test row"),
            description_i18n: None,
            backend: Some("missing-warmup"),
            agent_type: "acp",
            agent_source: "builtin",
            agent_source_info: Some(r#"{"binary_name":"aionui-definitely-missing-warmup"}"#),
            enabled: true,
            command: Some("aionui-definitely-missing-warmup"),
            args: Some("[]"),
            env: Some("[]"),
            native_skills_dirs: None,
            behavior_policy: None,
            yolo_id: None,
            agent_capabilities: None,
            auth_methods: None,
            config_options: None,
            available_modes: None,
            available_models: None,
            available_commands: None,
            sort_order: 9900,
        })
        .await
        .unwrap();
    services.agent_registry.invalidate_and_rehydrate().await.unwrap();

    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;
    let req = json_with_token(
        "POST",
        "/api/agents/warmup",
        json!({ "backends": ["missing-warmup"], "reason": "before_send" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["results"][0]["backend"], "missing-warmup");
    assert_eq!(body["data"]["results"][0]["status"], "skipped");
    assert_eq!(body["data"]["results"][0]["agent_id"], "missing-warmup");
    assert!(
        body["data"]["results"][0]["error"]
            .as_str()
            .unwrap()
            .contains("not available")
    );
}

#[tokio::test]
async fn warmup_agents_rejects_unauthenticated_request() {
    let (app, _services) = build_app().await;
    let csrf_resp = app.clone().oneshot(get_request("/api/auth/status")).await.unwrap();
    let csrf = extract_csrf_token(&csrf_resp).expect("CSRF cookie should be set");

    let req = Request::builder()
        .method("POST")
        .uri("/api/agents/warmup")
        .header("content-type", "application/json")
        .header("x-csrf-token", &csrf)
        .header("cookie", format!("aionui-csrf-token={csrf}"))
        .body(Body::from(r#"{"backends":["codex"],"reason":"idle"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn warmup_agents_requires_csrf() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/agents/warmup")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(r#"{"backends":["codex"]}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_custom_agent_nonexistent_command() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    // Endpoint was renamed from /api/agents/test to /api/agents/custom/try-connect
    // when the custom-agent CRUD routes were introduced.  The new endpoint always
    // returns HTTP 200 and encodes failure in the JSON body (step = "fail_cli" or
    // "fail_acp"), so we assert on the body rather than the HTTP status.
    let req = json_with_token(
        "POST",
        "/api/agents/custom/try-connect",
        json!({ "command": "/nonexistent/path/to/agent" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = common::body_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["step"], "fail_cli");
}

#[tokio::test]
async fn health_check_returns_status() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "POST",
        "/api/agents/health-check",
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
async fn health_check_unknown_backend_reports_unavailable() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    // Same rationale as `detect_cli_unknown_backend_returns_null_path`:
    // unknown backends are valid at the request layer and surface as
    // `available: false` with an error string.
    let req = json_with_token(
        "POST",
        "/api/agents/health-check",
        json!({ "backend": "iFlow" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["available"], false);
}

// ── Session-bound ACP routes (no active task → 404) ──────────────

#[tokio::test]
async fn get_mode_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = get_with_token("/api/conversations/nonexistent/mode", &token);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn set_mode_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "PUT",
        "/api/conversations/nonexistent/mode",
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

    let req = get_with_token("/api/conversations/nonexistent/model", &token);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn set_model_no_active_task() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let req = json_with_token(
        "PUT",
        "/api/conversations/nonexistent/model",
        json!({ "model_id": "claude-sonnet-4" }),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
