//! App-level integration test proving that `GET /api/analytics/agent-usage`
//! is protected by the auth middleware in the fully-composed application router.
//!
//! **Why this test exists (design.md §322)**
//!
//! The `aionui-analytics` crate's own unit tests use a naked `analytics_routes`
//! router (no auth layer), so they cannot verify that auth protection is wired
//! correctly.  This file is the **only automated proof** that the route is
//! reachable only with a valid token, because it goes through the real
//! `create_router` / `create_router_with_states` call that applies:
//!
//! ```text
//! analytics_routes(states.analytics)
//!     .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware))
//! ```
//!
//! **Status-code convention**: `aionui_auth::middleware::auth_middleware` returns
//! `403 Forbidden` for *any* authentication failure (missing token, bad token,
//! expired token).  This matches `assistants_e2e.rs::list_requires_auth` and the
//! assertion in `assistants_e2e.rs` comments.  design.md §322 also specifies 403.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Test: unauthenticated GET returns 403
// ---------------------------------------------------------------------------

/// Sends `GET /api/analytics/agent-usage` with NO `Authorization` header to
/// the fully-composed application router and asserts the auth middleware
/// rejects it with `403 Forbidden`.
///
/// This exercises the exact composition path in `create_router_with_all_state`:
/// ```text
/// let analytics_authenticated =
///     analytics_routes(states.analytics)
///         .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));
/// // ...
/// Router::new().merge(analytics_authenticated)
/// ```
#[tokio::test]
async fn analytics_agent_usage_requires_auth() {
    // Build the full composed app router (identical to `build_app` in common/mod.rs).
    // `AppConfig::default()` sets `local = false`, so the auth middleware is
    // active for all authenticated routes.
    let (app, _services) = common::build_app().await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/analytics/agent-usage")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();

    // `auth_middleware` returns 403 Forbidden for any authentication failure
    // (missing token, invalid token, expired token).
    // See `aionui_auth::middleware::auth_middleware` and the analogous
    // `list_requires_auth` test in `assistants_e2e.rs`.
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "unauthenticated request to /api/analytics/agent-usage must be rejected with 403 Forbidden"
    );
}
