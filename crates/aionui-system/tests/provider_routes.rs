//! Black-box integration tests for provider CRUD routes.
//!
//! Tests exercise the HTTP layer (request -> handler -> response) via
//! `tower::ServiceExt::oneshot`, without authentication middleware.
//! Auth protection is verified at the app-level E2E tests (task 3.9).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use aionui_db::{
    SqliteClientPreferenceRepository, SqliteProviderRepository, SqliteSettingsRepository,
    init_database_memory,
};
use aionui_system::{
    ClientPrefService, ModelFetchService, ProviderService, SettingsService, SystemRouterState,
    system_routes,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const TEST_ENCRYPTION_KEY: [u8; 32] = [0x42; 32];

fn build_state(db: &aionui_db::Database) -> SystemRouterState {
    let provider_repo = Arc::new(SqliteProviderRepository::new(db.pool().clone()));
    let http_client = reqwest::Client::new();
    SystemRouterState {
        settings_service: SettingsService::new(Arc::new(SqliteSettingsRepository::new(
            db.pool().clone(),
        ))),
        client_pref_service: ClientPrefService::new(Arc::new(
            SqliteClientPreferenceRepository::new(db.pool().clone()),
        )),
        provider_service: ProviderService::new(provider_repo.clone(), TEST_ENCRYPTION_KEY),
        model_fetch_service: ModelFetchService::new(
            provider_repo,
            TEST_ENCRYPTION_KEY,
            http_client,
        ),
    }
}

async fn setup() -> (axum::Router, aionui_db::Database) {
    let db = init_database_memory().await.unwrap();
    let state = build_state(&db);
    (system_routes(state), db)
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn get_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn json_request(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn delete_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn sample_create_body() -> serde_json::Value {
    json!({
        "platform": "anthropic",
        "name": "Anthropic",
        "baseUrl": "https://api.anthropic.com",
        "apiKey": "sk-ant-api03-test1234"
    })
}

/// Create a provider and return (response_json, provider_id, fresh_router).
async fn create_one(db: &aionui_db::Database) -> (serde_json::Value, String) {
    let app = system_routes(build_state(db));
    let resp = app
        .oneshot(json_request("POST", "/api/providers", sample_create_body()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    let id = json["data"]["id"].as_str().unwrap().to_string();
    (json, id)
}

// ===========================================================================
// GET /api/providers — list
// ===========================================================================

#[tokio::test]
async fn list_providers_empty() {
    let (app, _db) = setup().await;
    let resp = app.oneshot(get_request("/api/providers")).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"], json!([]));
}

#[tokio::test]
async fn list_providers_returns_masked_api_key() {
    let (_app, db) = setup().await;
    create_one(&db).await;

    let app2 = system_routes(build_state(&db));
    let resp = app2.oneshot(get_request("/api/providers")).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let providers = json["data"].as_array().unwrap();
    assert_eq!(providers.len(), 1);

    let api_key = providers[0]["apiKey"].as_str().unwrap();
    assert!(api_key.contains("***"));
    assert!(api_key.ends_with("1234"));
    // Must NOT contain the full key
    assert!(!api_key.contains("test1234"));
}

// ===========================================================================
// POST /api/providers — create
// ===========================================================================

#[tokio::test]
async fn create_provider_success() {
    let (app, _db) = setup().await;
    let resp = app
        .oneshot(json_request("POST", "/api/providers", sample_create_body()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    assert_eq!(json["success"], true);

    let data = &json["data"];
    assert!(data["id"].as_str().unwrap().starts_with("prov_"));
    assert_eq!(data["platform"], "anthropic");
    assert_eq!(data["name"], "Anthropic");
    assert_eq!(data["baseUrl"], "https://api.anthropic.com");
    assert!(data["apiKey"].as_str().unwrap().contains("***"));
    assert!(data["enabled"].as_bool().unwrap());
    assert!(data["models"].as_array().unwrap().is_empty());
    assert!(data["createdAt"].as_i64().unwrap() > 0);
    assert!(data["updatedAt"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn create_provider_with_optional_fields() {
    let (app, _db) = setup().await;
    let body = json!({
        "platform": "bedrock",
        "name": "AWS Bedrock",
        "baseUrl": "https://bedrock.us-east-1.amazonaws.com",
        "apiKey": "test-key-abcd",
        "models": ["anthropic.claude-3-sonnet"],
        "enabled": false,
        "capabilities": [{"type": "text"}, {"type": "vision", "isUserSelected": true}],
        "contextLimit": 200000,
        "bedrockConfig": {
            "authMethod": "accessKey",
            "region": "us-east-1",
            "accessKeyId": "AKIA...",
            "secretAccessKey": "secret"
        }
    });

    let resp = app
        .oneshot(json_request("POST", "/api/providers", body))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    let data = &json["data"];
    assert!(!data["enabled"].as_bool().unwrap());
    assert_eq!(data["models"].as_array().unwrap().len(), 1);
    assert_eq!(data["capabilities"].as_array().unwrap().len(), 2);
    assert_eq!(data["contextLimit"], 200000);
    assert_eq!(data["bedrockConfig"]["authMethod"], "accessKey");
    assert_eq!(data["bedrockConfig"]["region"], "us-east-1");
}

#[tokio::test]
async fn create_provider_missing_platform() {
    let (app, _db) = setup().await;
    let body = json!({
        "name": "Test",
        "baseUrl": "https://api.example.com",
        "apiKey": "sk-test"
    });
    let resp = app
        .oneshot(json_request("POST", "/api/providers", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_provider_missing_name() {
    let (app, _db) = setup().await;
    let body = json!({
        "platform": "openai",
        "baseUrl": "https://api.example.com",
        "apiKey": "sk-test"
    });
    let resp = app
        .oneshot(json_request("POST", "/api/providers", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_provider_missing_base_url() {
    let (app, _db) = setup().await;
    let body = json!({
        "platform": "openai",
        "name": "Test",
        "apiKey": "sk-test"
    });
    let resp = app
        .oneshot(json_request("POST", "/api/providers", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_provider_missing_api_key() {
    let (app, _db) = setup().await;
    let body = json!({
        "platform": "openai",
        "name": "Test",
        "baseUrl": "https://api.example.com"
    });
    let resp = app
        .oneshot(json_request("POST", "/api/providers", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_provider_invalid_url() {
    let (app, _db) = setup().await;
    let body = json!({
        "platform": "openai",
        "name": "Test",
        "baseUrl": "not-a-url",
        "apiKey": "sk-test"
    });
    let resp = app
        .oneshot(json_request("POST", "/api/providers", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ===========================================================================
// PUT /api/providers/{id} — update
// ===========================================================================

#[tokio::test]
async fn update_provider_name() {
    let (_app, db) = setup().await;
    let (_, id) = create_one(&db).await;

    let app2 = system_routes(build_state(&db));
    let resp = app2
        .oneshot(json_request(
            "PUT",
            &format!("/api/providers/{id}"),
            json!({"name": "New Name"}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["data"]["name"], "New Name");
    assert_eq!(json["data"]["platform"], "anthropic");
}

#[tokio::test]
async fn update_provider_api_key_mask_changes() {
    let (_app, db) = setup().await;
    let (_, id) = create_one(&db).await;

    let app2 = system_routes(build_state(&db));
    let resp = app2
        .oneshot(json_request(
            "PUT",
            &format!("/api/providers/{id}"),
            json!({"apiKey": "new-key-abcdefgh"}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let api_key = json["data"]["apiKey"].as_str().unwrap();
    assert!(api_key.ends_with("efgh"));
}

#[tokio::test]
async fn update_provider_nonexistent() {
    let (app, _db) = setup().await;
    let resp = app
        .oneshot(json_request(
            "PUT",
            "/api/providers/nonexistent",
            json!({"name": "X"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ===========================================================================
// DELETE /api/providers/{id}
// ===========================================================================

#[tokio::test]
async fn delete_provider_success() {
    let (_app, db) = setup().await;
    let (_, id) = create_one(&db).await;

    let app2 = system_routes(build_state(&db));
    let resp = app2
        .oneshot(delete_request(&format!("/api/providers/{id}")))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
}

#[tokio::test]
async fn delete_provider_then_list_excludes_deleted() {
    let (_app, db) = setup().await;
    let (_, id) = create_one(&db).await;

    let app2 = system_routes(build_state(&db));
    let resp = app2
        .oneshot(delete_request(&format!("/api/providers/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let app3 = system_routes(build_state(&db));
    let resp = app3.oneshot(get_request("/api/providers")).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["data"], json!([]));
}

#[tokio::test]
async fn delete_provider_nonexistent() {
    let (app, _db) = setup().await;
    let resp = app
        .oneshot(delete_request("/api/providers/nonexistent"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ===========================================================================
// Full CRUD flow
// ===========================================================================

#[tokio::test]
async fn full_crud_flow() {
    let (_app, db) = setup().await;

    // 1. Create
    let (create_json, id) = create_one(&db).await;
    assert_eq!(create_json["data"]["platform"], "anthropic");

    // 2. List — should contain one
    let app2 = system_routes(build_state(&db));
    let resp = app2.oneshot(get_request("/api/providers")).await.unwrap();
    let list_json = body_json(resp).await;
    assert_eq!(list_json["data"].as_array().unwrap().len(), 1);

    // 3. Update
    let app3 = system_routes(build_state(&db));
    let resp = app3
        .oneshot(json_request(
            "PUT",
            &format!("/api/providers/{id}"),
            json!({"name": "Updated", "enabled": false}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let update_json = body_json(resp).await;
    assert_eq!(update_json["data"]["name"], "Updated");
    assert!(!update_json["data"]["enabled"].as_bool().unwrap());

    // 4. Verify update via list
    let app4 = system_routes(build_state(&db));
    let resp = app4.oneshot(get_request("/api/providers")).await.unwrap();
    let list_json = body_json(resp).await;
    assert_eq!(list_json["data"][0]["name"], "Updated");

    // 5. Delete
    let app5 = system_routes(build_state(&db));
    let resp = app5
        .oneshot(delete_request(&format!("/api/providers/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 6. Verify deleted
    let app6 = system_routes(build_state(&db));
    let resp = app6.oneshot(get_request("/api/providers")).await.unwrap();
    let list_json = body_json(resp).await;
    assert_eq!(list_json["data"], json!([]));
}
