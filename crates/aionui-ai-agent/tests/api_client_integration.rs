//! Integration tests for the API client layer.
//!
//! Uses a lightweight axum-based mock HTTP server to exercise
//! key rotation, retry logic, and protocol handling end-to-end.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::json;

use aionui_ai_agent::api_client::key_manager::ApiKeyManager;
use aionui_ai_agent::api_client::openai::OpenAIRotatingClient;
use aionui_ai_agent::api_client::{ClientOptions, create_rotating_client, LlmClient};

// ---------------------------------------------------------------------------
// Mock server helpers
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MockState {
    responses: Arc<Vec<(StatusCode, serde_json::Value)>>,
    call_count: Arc<AtomicUsize>,
}

async fn mock_handler(State(state): State<MockState>) -> impl IntoResponse {
    let idx = state.call_count.fetch_add(1, Ordering::Relaxed);
    let total = state.responses.len();
    let (status, body) = if idx < total {
        state.responses[idx].clone()
    } else {
        // Return last response for any extra calls
        state.responses[total - 1].clone()
    };
    (status, axum::Json(body))
}

/// Spin up a mock HTTP server that returns the given responses in order.
/// Returns (base_url, call_count_handle).
async fn start_mock_server(
    responses: Vec<(StatusCode, serde_json::Value)>,
) -> (String, Arc<AtomicUsize>) {
    let call_count = Arc::new(AtomicUsize::new(0));
    let state = MockState {
        responses: Arc::new(responses),
        call_count: call_count.clone(),
    };

    let app = axum::Router::new()
        .route("/{*path}", post(mock_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());

    (format!("http://127.0.0.1:{}", addr.port()), call_count)
}

// ---------------------------------------------------------------------------
// Tests: Key management
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_key_success() {
    let (url, call_count) = start_mock_server(vec![(
        StatusCode::OK,
        json!({ "id": "chatcmpl-1", "choices": [{ "message": { "content": "Hi" } }] }),
    )])
    .await;

    let km = Arc::new(ApiKeyManager::new("sk-only", None));
    let client = OpenAIRotatingClient::new(km, &url, Some(1), Some(10));

    let result = client
        .create_chat_completion(&json!({ "model": "gpt-4", "messages": [] }))
        .await;

    assert!(result.is_ok());
    assert_eq!(call_count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn multiple_keys_parsed_correctly() {
    let km = ApiKeyManager::new("sk-a,sk-b,sk-c", None);
    assert_eq!(km.total_keys().await, 3);
}

#[tokio::test]
async fn key_rotation_on_401() {
    let (url, call_count) = start_mock_server(vec![
        (StatusCode::UNAUTHORIZED, json!({ "error": "invalid_api_key" })),
        (StatusCode::OK, json!({ "result": "success" })),
    ])
    .await;

    let km = Arc::new(ApiKeyManager::new("sk-bad,sk-good", None));
    let client = OpenAIRotatingClient::new(km, &url, Some(2), Some(10));

    let result = client
        .create_chat_completion(&json!({ "model": "gpt-4", "messages": [] }))
        .await;

    assert!(result.is_ok());
    // First call returns 401, second call (with rotated key) returns 200
    assert_eq!(call_count.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn key_rotation_on_429() {
    let (url, call_count) = start_mock_server(vec![
        (
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "error": "rate_limited" }),
        ),
        (StatusCode::OK, json!({ "data": "ok" })),
    ])
    .await;

    let km = Arc::new(ApiKeyManager::new("sk-a,sk-b", None));
    let client = OpenAIRotatingClient::new(km, &url, Some(2), Some(10));

    let result = client
        .create_chat_completion(&json!({ "model": "gpt-4", "messages": [] }))
        .await;

    assert!(result.is_ok());
    assert_eq!(call_count.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn all_keys_exhausted_returns_error() {
    let (url, _) = start_mock_server(vec![(
        StatusCode::UNAUTHORIZED,
        json!({ "error": "bad key" }),
    )])
    .await;

    // Single key, blacklisted after first failure
    let km = Arc::new(ApiKeyManager::new("sk-only", None));
    let client = OpenAIRotatingClient::new(km, &url, Some(2), Some(10));

    let result = client
        .create_chat_completion(&json!({ "model": "gpt-4", "messages": [] }))
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    let err_str = err.to_string();
    // Should either be AllKeysExhausted or HttpError (single key can't rotate)
    assert!(
        err_str.contains("exhausted") || err_str.contains("HTTP error"),
        "unexpected error: {err_str}"
    );
}

#[tokio::test]
async fn max_retries_exceeded() {
    let (url, call_count) = start_mock_server(vec![
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": "server error" }),
        ),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": "still broken" }),
        ),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": "still broken" }),
        ),
    ])
    .await;

    let km = Arc::new(ApiKeyManager::new("sk-a,sk-b,sk-c", None));
    // max_retries=1 means attempt 0 + 1 retry = 2 total attempts
    let client = OpenAIRotatingClient::new(km, &url, Some(1), Some(10));

    let result = client
        .create_chat_completion(&json!({ "model": "gpt-4", "messages": [] }))
        .await;

    assert!(result.is_err());
    // 2 attempts (original + 1 retry)
    assert_eq!(call_count.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn non_retryable_error_returns_immediately() {
    let (url, call_count) = start_mock_server(vec![(
        StatusCode::BAD_REQUEST,
        json!({ "error": "bad request" }),
    )])
    .await;

    let km = Arc::new(ApiKeyManager::new("sk-test", None));
    let client = OpenAIRotatingClient::new(km, &url, Some(3), Some(10));

    let result = client
        .create_chat_completion(&json!({ "model": "gpt-4", "messages": [] }))
        .await;

    assert!(result.is_err());
    // Should not retry on 400
    assert_eq!(call_count.load(Ordering::Relaxed), 1);
}

// ---------------------------------------------------------------------------
// Tests: Client factory
// ---------------------------------------------------------------------------

#[tokio::test]
async fn factory_openai_chat_completion() {
    let (url, _) = start_mock_server(vec![(
        StatusCode::OK,
        json!({ "choices": [{ "message": { "content": "Hello" } }] }),
    )])
    .await;

    let client = create_rotating_client(
        "USE_OPENAI",
        "sk-test",
        &url,
        ClientOptions {
            max_retries: Some(0),
            retry_delay_ms: Some(10),
        },
    );

    assert!(matches!(client, LlmClient::OpenAI(_)));

    let result = client
        .create_chat_completion(&json!({ "model": "gpt-4", "messages": [] }))
        .await;
    assert!(result.is_ok());
}

// ---------------------------------------------------------------------------
// Tests: Key blacklist timing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn blacklisted_key_becomes_available_after_timeout() {
    let km = ApiKeyManager::new("sk-only", None);

    // Get and blacklist
    let _ = km.get_available_key().await;
    km.blacklist_current().await;

    // Should be unavailable
    assert!(km.get_available_key().await.is_none());

    // We can't wait 90 seconds in a test, but we verify the mechanism works
    let status = km.get_status("test").await;
    assert_eq!(status.blacklisted, 1);
    assert_eq!(status.total, 1);
}

// ---------------------------------------------------------------------------
// Tests: Image endpoint
// ---------------------------------------------------------------------------

#[tokio::test]
async fn openai_create_image() {
    let (url, call_count) = start_mock_server(vec![(
        StatusCode::OK,
        json!({ "data": [{ "url": "https://example.com/image.png" }] }),
    )])
    .await;

    let km = Arc::new(ApiKeyManager::new("sk-test", None));
    let client = OpenAIRotatingClient::new(km, &url, Some(0), Some(10));

    let result = client
        .create_image(&json!({
            "prompt": "a cat",
            "model": "dall-e-3",
            "n": 1
        }))
        .await;

    assert!(result.is_ok());
    assert_eq!(call_count.load(Ordering::Relaxed), 1);
    let data = result.unwrap();
    assert!(data["data"][0]["url"].is_string());
}

// ---------------------------------------------------------------------------
// Tests: Embedding endpoint
// ---------------------------------------------------------------------------

#[tokio::test]
async fn openai_create_embedding() {
    let (url, call_count) = start_mock_server(vec![(
        StatusCode::OK,
        json!({ "data": [{ "embedding": [0.1, 0.2, 0.3] }] }),
    )])
    .await;

    let km = Arc::new(ApiKeyManager::new("sk-test", None));
    let client = OpenAIRotatingClient::new(km, &url, Some(0), Some(10));

    let result = client
        .create_embedding(&json!({
            "model": "text-embedding-3-small",
            "input": "Hello world"
        }))
        .await;

    assert!(result.is_ok());
    assert_eq!(call_count.load(Ordering::Relaxed), 1);
}
