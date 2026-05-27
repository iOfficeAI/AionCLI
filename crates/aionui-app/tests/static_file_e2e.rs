//! E2E tests for the static file endpoint:
//! `GET /api/conversations/{id}/files/{*path}`

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use common::{body_json, build_app, json_with_token, setup_and_login};

/// Create a conversation with a real temp workspace on disk.
async fn create_conversation_with_workspace(
    app: &mut axum::Router,
    token: &str,
    csrf: &str,
) -> (String, std::path::PathBuf) {
    let workspace = tempfile::tempdir().unwrap();
    let workspace_path = workspace.path().to_path_buf();

    let body = json!({
        "type": "acp",
        "name": "file-test",
        "extra": { "workspace": workspace_path.to_str().unwrap() }
    });
    let req = json_with_token("POST", "/api/conversations", body, token, csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = body_json(resp).await;
    let id = json["data"]["id"].as_str().unwrap().to_owned();

    // Leak the tempdir so it stays alive for the test
    std::mem::forget(workspace);

    (id, workspace_path)
}

fn get_file_request(conv_id: &str, path: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(format!("/api/conversations/{conv_id}/files/{path}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn get_file_request_with_range(conv_id: &str, path: &str, token: &str, range: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(format!("/api/conversations/{conv_id}/files/{path}"))
        .header("authorization", format!("Bearer {token}"))
        .header("range", range)
        .body(Body::empty())
        .unwrap()
}

// ── Happy Path ──────────────────────────────────────────────────────

#[tokio::test]
async fn serves_text_file_with_correct_content_type() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let (conv_id, workspace) = create_conversation_with_workspace(&mut app, &token, &csrf).await;

    std::fs::write(workspace.join("hello.txt"), "world").unwrap();

    let req = get_file_request(&conv_id, "hello.txt", &token);
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), "text/plain");
    assert_eq!(resp.headers().get(header::CONTENT_LENGTH).unwrap(), "5");
    assert!(resp.headers().get(header::ETAG).is_some());
    assert_eq!(resp.headers().get(header::CACHE_CONTROL).unwrap(), "public, max-age=60");
    assert_eq!(resp.headers().get(header::ACCEPT_RANGES).unwrap(), "bytes");

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"world");
}

#[tokio::test]
async fn serves_svg_with_correct_content_type() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let (conv_id, workspace) = create_conversation_with_workspace(&mut app, &token, &csrf).await;

    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><circle r="5"/></svg>"#;
    std::fs::write(workspace.join("output.svg"), svg).unwrap();

    let req = get_file_request(&conv_id, "output.svg", &token);
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), "image/svg+xml");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], svg.as_bytes());
}

#[tokio::test]
async fn serves_nested_path() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let (conv_id, workspace) = create_conversation_with_workspace(&mut app, &token, &csrf).await;

    std::fs::create_dir_all(workspace.join("sub/dir")).unwrap();
    std::fs::write(workspace.join("sub/dir/deep.json"), r#"{"ok":true}"#).unwrap();

    let req = get_file_request(&conv_id, "sub/dir/deep.json", &token);
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), "application/json");
}

// ── Range Requests ──────────────────────────────────────────────────

#[tokio::test]
async fn range_request_returns_partial_content() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let (conv_id, workspace) = create_conversation_with_workspace(&mut app, &token, &csrf).await;

    std::fs::write(workspace.join("data.bin"), b"0123456789").unwrap();

    let req = get_file_request_with_range(&conv_id, "data.bin", &token, "bytes=5-9");
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(resp.headers().get(header::CONTENT_RANGE).unwrap(), "bytes 5-9/10");
    assert_eq!(resp.headers().get(header::CONTENT_LENGTH).unwrap(), "5");

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"56789");
}

#[tokio::test]
async fn range_request_open_ended() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let (conv_id, workspace) = create_conversation_with_workspace(&mut app, &token, &csrf).await;

    std::fs::write(workspace.join("data.bin"), b"abcdefghij").unwrap();

    let req = get_file_request_with_range(&conv_id, "data.bin", &token, "bytes=7-");
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"hij");
}

// ── Security: Path Traversal ────────────────────────────────────────

#[tokio::test]
async fn rejects_path_traversal() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let (conv_id, _workspace) = create_conversation_with_workspace(&mut app, &token, &csrf).await;

    let req = get_file_request(&conv_id, "../../../etc/passwd", &token);
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ── Security: Cross-user Access ─────────────────────────────────────

#[tokio::test]
async fn rejects_other_users_conversation() {
    let (mut app, services) = build_app().await;
    let (token_a, csrf_a) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let (conv_id, workspace) = create_conversation_with_workspace(&mut app, &token_a, &csrf_a).await;

    std::fs::write(workspace.join("secret.txt"), "mine").unwrap();

    // Create a second user
    services
        .user_repo
        .create_user("attacker", &aionui_auth::hash_password("Attack3r!").unwrap())
        .await
        .unwrap();
    let body = r#"{"username":"attacker","password":"Attack3r!"}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/login")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    let token_b = json["token"].as_str().unwrap();

    // Attacker tries to read user A's conversation files
    let req = get_file_request(&conv_id, "secret.txt", token_b);
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── File Not Found ──────────────────────────────────────────────────

#[tokio::test]
async fn returns_404_for_missing_file() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let (conv_id, _workspace) = create_conversation_with_workspace(&mut app, &token, &csrf).await;

    let req = get_file_request(&conv_id, "nonexistent.txt", &token);
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Unauthenticated Request ─────────────────────────────────────────

#[tokio::test]
async fn rejects_unauthenticated_request() {
    let (app, _services) = build_app().await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/conversations/fake-id/files/anything.txt")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    // Auth middleware returns 403 for all auth failures (per API spec)
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
