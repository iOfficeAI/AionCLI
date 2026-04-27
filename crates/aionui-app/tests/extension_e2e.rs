mod common;

use axum::http::StatusCode;
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;

use common::{
    body_json, build_app, build_app_with_skill_paths, get_with_token, json_with_token,
    setup_and_login,
};

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
    assert!(data["user_skills_dir"].is_string());
    assert!(data["builtin_skills_dir"].is_string());
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
            json!({"file_name": "nonexistent-rule.md"}),
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

#[tokio::test]
async fn rm2_read_builtin_rule_happy_path_returns_file_content() {
    let tmp = TempDir::new().unwrap();
    let (mut app, services, paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    std::fs::write(
        paths.builtin_rules_dir.join("code-review.md"),
        "# Code Review Rules\n\nBe kind.\n",
    )
    .unwrap();

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/skills/builtin-rule",
            json!({"file_name": "code-review.md"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"], "# Code Review Rules\n\nBe kind.\n");
}

#[tokio::test]
async fn rm3_read_builtin_rule_rejects_path_traversal() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/skills/builtin-rule",
            json!({"file_name": "../etc/passwd"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let json = body_json(resp).await;
    assert_eq!(json["success"], false);
}

// ---------------------------------------------------------------------------
// SK — Built-in skill file reading (E4 / `POST /api/skills/builtin-skill`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sk1_read_builtin_skill_not_found() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/skills/builtin-skill",
            json!({"file_name": "nonexistent.md"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"], "");
}

#[tokio::test]
async fn sk2_read_builtin_skill_happy_path_returns_file_content() {
    let tmp = TempDir::new().unwrap();
    let (mut app, services, paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    std::fs::write(
        paths.builtin_skills_dir.join("cowork-skills.md"),
        "## Cowork skills\n\n- git\n- bash\n",
    )
    .unwrap();

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/skills/builtin-skill",
            json!({"file_name": "cowork-skills.md"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"], "## Cowork skills\n\n- git\n- bash\n");
}

#[tokio::test]
async fn sk3_read_builtin_skill_rejects_path_traversal() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    // Path traversal / absolute-path attempts must be rejected. Relative
    // paths with `/` are now legitimate (e.g. `auto-inject/cron/SKILL.md`)
    // and handled by the valid-path code path further below.
    for bad in ["../escape.md", "/etc/passwd", "foo/../etc/passwd", ""] {
        let resp = app
            .clone()
            .oneshot(json_with_token(
                "POST",
                "/api/skills/builtin-skill",
                json!({"file_name": bad}),
                &token,
                &csrf,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "file_name={bad:?} should be rejected",
        );
    }
}

// ---------------------------------------------------------------------------
// SI — Skill info (E5 / `POST /api/skills/info`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn si1_read_skill_info_from_directory_path() {
    let tmp = TempDir::new().unwrap();
    let (mut app, services, _paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let skill_dir = tmp.path().join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: my-skill\ndescription: Handy little thing\n---\nBody",
    )
    .unwrap();

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/skills/info",
            json!({ "skill_path": skill_dir.to_str().unwrap() }),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["name"], "my-skill");
    assert_eq!(json["data"]["description"], "Handy little thing");
}

#[tokio::test]
async fn si2_read_skill_info_falls_back_to_directory_name_when_name_empty() {
    let tmp = TempDir::new().unwrap();
    let (mut app, services, _paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let skill_dir = tmp.path().join("fallback-dir");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: \ndescription: Empty-name skill\n---\nBody",
    )
    .unwrap();

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/skills/info",
            json!({ "skill_path": skill_dir.to_str().unwrap() }),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["name"], "fallback-dir");
    assert_eq!(json["data"]["description"], "Empty-name skill");
}

#[tokio::test]
async fn si3_read_skill_info_returns_not_found_for_missing_path() {
    let tmp = TempDir::new().unwrap();
    let (mut app, services, _paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let missing = tmp.path().join("no-such-skill");

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/skills/info",
            json!({ "skill_path": missing.to_str().unwrap() }),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let json = body_json(resp).await;
    assert_eq!(json["success"], false);
}

// ---------------------------------------------------------------------------
// SL — Skill listing (E1 / `GET /api/skills`)
// ---------------------------------------------------------------------------

fn write_skill(dir: &std::path::Path, name: &str, description: &str) {
    let skill = dir.join(name);
    std::fs::create_dir_all(&skill).unwrap();
    let frontmatter = format!("---\nname: {name}\ndescription: {description}\n---\nBody");
    std::fs::write(skill.join("SKILL.md"), frontmatter).unwrap();
}

#[tokio::test]
async fn sl1_list_skills_tags_builtin_and_custom_with_source_field() {
    let tmp = TempDir::new().unwrap();
    let (mut app, services, paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let builtin_dir = paths.builtin_skills_dir.clone();
    write_skill(&builtin_dir, "review", "Built-in review skill");
    write_skill(&paths.user_skills_dir, "my-skill", "A user-imported skill");

    let resp = app
        .oneshot(get_with_token("/api/skills", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    let arr = json["data"].as_array().unwrap();
    assert_eq!(arr.len(), 2);

    let by_name: std::collections::HashMap<_, _> = arr
        .iter()
        .map(|v| (v["name"].as_str().unwrap().to_owned(), v.clone()))
        .collect();

    let review = &by_name["review"];
    assert_eq!(review["source"], "builtin");
    assert_eq!(review["is_custom"], false);
    assert!(
        review["location"].as_str().unwrap().contains("review"),
        "location should point at the skill dir",
    );

    let my_skill = &by_name["my-skill"];
    assert_eq!(my_skill["source"], "custom");
    assert_eq!(my_skill["is_custom"], true);
}

#[tokio::test]
async fn sl2_list_skills_user_custom_overrides_builtin() {
    let tmp = TempDir::new().unwrap();
    let (mut app, services, paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let builtin_dir = paths.builtin_skills_dir.clone();
    write_skill(&builtin_dir, "review", "Built-in review");
    write_skill(&paths.user_skills_dir, "review", "Custom review override");

    let resp = app
        .oneshot(get_with_token("/api/skills", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["data"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["description"], "Custom review override");
    assert_eq!(arr[0]["source"], "custom");
}

#[tokio::test]
async fn sl3_list_skills_returns_empty_array_when_no_skills() {
    let tmp = TempDir::new().unwrap();
    let (mut app, services, _paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/skills", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// BA — Built-in auto skills (E2 / `GET /api/skills/builtin-auto`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ba1_auto_skills_lists_underscore_builtin_entries() {
    let tmp = TempDir::new().unwrap();
    let (mut app, services, paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let builtin_dir = paths.builtin_skills_dir.clone();
    let auto_dir = builtin_dir.join("auto-inject");
    write_skill(&auto_dir, "cron", "Schedule recurring tasks");
    write_skill(&auto_dir, "skill-creator", "Scaffold a new skill");
    // A top-level builtin that must NOT appear in the auto list.
    write_skill(&builtin_dir, "review", "Top-level");

    let resp = app
        .oneshot(get_with_token("/api/skills/builtin-auto", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    let arr = json["data"].as_array().unwrap();
    assert_eq!(arr.len(), 2);
    let names: std::collections::HashSet<_> =
        arr.iter().map(|v| v["name"].as_str().unwrap()).collect();
    assert!(names.contains("cron"));
    assert!(names.contains("skill-creator"));
    assert!(!names.contains("review"));
    // Must be `{ name, description, location }` — no path / is_custom leak.
    for item in arr {
        assert!(item.get("path").is_none());
        assert!(item.get("is_custom").is_none());
        assert!(item.get("is_custom").is_none());
        assert!(item["description"].is_string());
    }
}

#[tokio::test]
async fn ba2_auto_skills_returns_empty_array_when_subdir_missing() {
    let tmp = TempDir::new().unwrap();
    let (mut app, services, _paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/skills/builtin-auto", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn ba3_auto_skills_unauthenticated_rejected() {
    let (app, _) = build_app().await;
    let resp = app
        .oneshot(common::get_request("/api/skills/builtin-auto"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// DE — `GET /api/skills/detect-external` (source slug contract)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn de1_detect_external_populates_custom_source_slug() {
    // The renderer uses `source` as a React key / `data-testid` suffix
    // (`external-source-tab-${source}` in `SkillsHubSettings.tsx`). Custom
    // paths MUST produce slugs prefixed with `custom-` per the e2e contract
    // in `tests/e2e/features/settings/skills/edge-cases.e2e.ts`.
    let tmp = TempDir::new().unwrap();
    let (mut app, services, _paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let ext_dir = tmp.path().join("external-skills");
    let skill_dir = ext_dir.join("my-ext-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: my-ext-skill\ndescription: External skill\n---\nBody",
    )
    .unwrap();
    let ext_path_str = ext_dir.to_string_lossy().into_owned();

    // Register the custom path through the HTTP surface so the state the
    // handler reads is the same as production.
    let resp = app
        .clone()
        .oneshot(json_with_token(
            "POST",
            "/api/skills/external-paths",
            json!({"name": "E2E Custom", "path": ext_path_str}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(get_with_token("/api/skills/detect-external", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    let arr = json["data"].as_array().expect("data should be an array");
    let custom = arr
        .iter()
        .find(|s| s["name"] == "E2E Custom")
        .expect("custom source should be returned");
    assert_eq!(custom["source"], format!("custom-{ext_path_str}"));
    assert!(
        custom["source"].as_str().unwrap().starts_with("custom-"),
        "custom source must start with `custom-` for e2e testid contract",
    );
    assert_eq!(custom["skill_count"], 1);
}

#[tokio::test]
async fn de2_detect_external_source_slugs_are_unique() {
    let tmp = TempDir::new().unwrap();
    let (mut app, services, _paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let mk = |p: &std::path::Path, skill: &str| {
        let dir = p.join(skill);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {skill}\ndescription: d\n---\nBody"),
        )
        .unwrap();
    };
    let dir_a = tmp.path().join("src-a");
    let dir_b = tmp.path().join("src-b");
    mk(&dir_a, "skill-a");
    mk(&dir_b, "skill-b");
    let path_a = dir_a.to_string_lossy().into_owned();
    let path_b = dir_b.to_string_lossy().into_owned();

    for (name, p) in [("Alpha", &path_a), ("Beta", &path_b)] {
        let resp = app
            .clone()
            .oneshot(json_with_token(
                "POST",
                "/api/skills/external-paths",
                json!({"name": name, "path": p}),
                &token,
                &csrf,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    let resp = app
        .oneshot(get_with_token("/api/skills/detect-external", &token))
        .await
        .unwrap();
    let json = body_json(resp).await;
    let arr = json["data"].as_array().unwrap();
    let slugs: Vec<&str> = arr
        .iter()
        .filter(|s| s["name"] == "Alpha" || s["name"] == "Beta")
        .map(|s| s["source"].as_str().unwrap())
        .collect();
    assert_eq!(slugs.len(), 2);
    assert_ne!(slugs[0], slugs[1], "distinct custom paths → distinct slugs");
}
