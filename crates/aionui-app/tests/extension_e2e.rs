mod common;

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use common::{body_json, build_app, get_with_token, json_with_token, setup_and_login};

// ---------------------------------------------------------------------------
// EQ — Extension query (unauthenticated → rejected)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn eq_unauthenticated_access_rejected() {
    let (app, _) = build_app().await;
    let resp = app
        .oneshot(common::get_request("/api/extensions"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// EQ — Extension query (authenticated)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn eq1_get_loaded_extensions_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert!(json["data"].is_array());
}

#[tokio::test]
async fn eq3_get_themes_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/themes", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
}

#[tokio::test]
async fn eq4_get_assistants_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/assistants", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
}

#[tokio::test]
async fn eq5_get_acp_adapters_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/acp-adapters", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn eq6_get_agents_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/agents", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn eq7_get_mcp_servers_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/mcp-servers", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn eq8_get_skills_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/skills", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn eq9_get_settings_tabs_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/settings-tabs", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn eq10_get_webui_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/webui", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn eq11_get_agent_activity() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/agent-activity", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
}

// ---------------------------------------------------------------------------
// EQ-12: i18n
// ---------------------------------------------------------------------------

#[tokio::test]
async fn eq12_get_i18n_for_locale() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/extensions/i18n",
            json!({"locale": "zh-CN"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    // With no extensions loaded, i18n data should be an empty object
    assert!(json["data"].is_object());
}

// ---------------------------------------------------------------------------
// EQ-13, EQ-14: Permissions / risk level for nonexistent → 404
// ---------------------------------------------------------------------------

#[tokio::test]
async fn eq13_permissions_not_found() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/extensions/permissions",
            json!({"name": "nonexistent-ext"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn eq14_risk_level_not_found() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/extensions/risk-level",
            json!({"name": "nonexistent-ext"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// EM — Extension management
// ---------------------------------------------------------------------------

#[tokio::test]
async fn em3_enable_nonexistent_returns_not_found() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/extensions/enable",
            json!({"name": "nonexistent"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn em4_disable_nonexistent_returns_not_found() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/extensions/disable",
            json!({"name": "nonexistent"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// HM — Hub marketplace
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hm1_get_hub_extensions() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/hub/extensions", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    // Empty index → empty array
    assert!(json["data"].is_array());
}

#[tokio::test]
async fn hm3_install_nonexistent_returns_error() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/hub/install",
            json!({"name": "nonexistent-ext"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    let inner = &json["data"];
    assert_eq!(inner["success"], false);
    assert!(inner["msg"].as_str().is_some());
}

#[tokio::test]
async fn hm5_check_updates_empty() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/hub/check-updates",
            json!({}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert!(json["data"].is_array());
}

// ---------------------------------------------------------------------------
// SM — Skill management
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sm11_get_skill_paths() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/skills/paths", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    let data = &json["data"];
    assert!(data["userSkillsDir"].is_string());
    assert!(data["builtinSkillsDir"].is_string());
}

#[tokio::test]
async fn sm9_detect_paths() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/skills/detect-paths", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert!(json["data"].is_array());
}

// ---------------------------------------------------------------------------
// CP — Custom external paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cp1_get_external_paths_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/skills/external-paths", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert!(json["data"].is_array());
    assert_eq!(json["data"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// AUTH — Auth protection on hub and skill routes too
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_hub_unauthenticated() {
    let (app, _) = build_app().await;
    let resp = app
        .oneshot(common::get_request("/api/hub/extensions"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn auth_skills_unauthenticated() {
    let (app, _) = build_app().await;
    let resp = app
        .oneshot(common::get_request("/api/skills"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// RM — Built-in rule reading
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rm1_read_builtin_rule_not_found() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/skills/builtin-rule",
            json!({"fileName": "nonexistent-rule.md"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    // File not found → returns empty string (graceful degradation)
    assert_eq!(json["data"], "");
}
