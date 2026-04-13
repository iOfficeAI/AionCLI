//! Shared test helpers for aionui-app E2E tests.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;
use wiremock::MockServer;

use aionui_app::{
    AppServices, build_acp_state, build_conversation_state, build_remote_agent_state,
    build_system_state, create_router, create_router_with_system_state,
};
use aionui_system::VersionCheckService;

pub async fn build_app() -> (axum::Router, AppServices) {
    let db = aionui_db::init_database_memory().await.unwrap();
    let services = AppServices::from_database(db).await.unwrap();
    let router = create_router(&services);
    (router, services)
}

pub async fn build_app_with_mock_version(
    current_version: &str,
    mock_server: &MockServer,
) -> (axum::Router, AppServices) {
    let db = aionui_db::init_database_memory().await.unwrap();
    let services = AppServices::from_database(db).await.unwrap();
    let mut system_state = build_system_state(&services);
    system_state.version_check_service = VersionCheckService::with_api_base(
        reqwest::Client::new(),
        current_version.to_owned(),
        mock_server.uri(),
    );
    let conversation_state = build_conversation_state(&services);
    let remote_agent_state = build_remote_agent_state(&services);
    let acp_state = build_acp_state(&services);
    let router = create_router_with_system_state(
        &services,
        system_state,
        conversation_state,
        remote_agent_state,
        acp_state,
    );
    (router, services)
}

pub async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

pub fn extract_csrf_token(resp: &axum::response::Response) -> Option<String> {
    resp.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|s| s.starts_with("aionui-csrf-token="))
        .map(|s| {
            s.strip_prefix("aionui-csrf-token=")
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        })
}

pub fn get_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

pub fn get_with_token(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

pub fn json_with_token(
    method_str: &str,
    uri: &str,
    body: serde_json::Value,
    token: &str,
    csrf: &str,
) -> Request<Body> {
    Request::builder()
        .method(method_str)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .header("x-csrf-token", csrf)
        .header("cookie", format!("aionui-csrf-token={csrf}"))
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

pub fn delete_with_token(uri: &str, token: &str, csrf: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("x-csrf-token", csrf)
        .header("cookie", format!("aionui-csrf-token={csrf}"))
        .body(Body::empty())
        .unwrap()
}

/// Set up a user and login, returning (session_token, csrf_token).
pub async fn setup_and_login(
    app: &mut axum::Router,
    services: &AppServices,
    username: &str,
    password: &str,
) -> (String, String) {
    let hash = aionui_auth::hash_password(password).unwrap();
    services.user_repo.create_user(username, &hash).await.unwrap();

    let resp = app.clone().oneshot(get_request("/api/auth/status")).await.unwrap();
    let csrf = extract_csrf_token(&resp).expect("CSRF cookie should be set");

    let body = format!(r#"{{"username":"{username}","password":"{password}"}}"#);
    let req = Request::builder()
        .method("POST")
        .uri("/login")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "login should succeed");

    let json = body_json(resp).await;
    let token = json["token"].as_str().unwrap().to_owned();

    (token, csrf)
}
