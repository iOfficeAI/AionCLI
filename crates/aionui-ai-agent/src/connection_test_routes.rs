use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, Query, State};
use axum::routing::{get, post};
use axum::Router;

use aionui_api_types::{
    ApiResponse, GeminiSubscriptionData, GeminiSubscriptionQuery, TestBedrockConnectionRequest,
};
use aionui_auth::CurrentUser;
use aionui_common::AppError;

use crate::connection_test_service::ConnectionTestService;

/// Router state for connection test routes.
#[derive(Clone)]
pub struct ConnectionTestRouterState {
    pub service: ConnectionTestService,
}

/// Build the connection test router.
///
/// Routes:
/// - `POST /api/bedrock/test-connection` — test AWS Bedrock credentials
/// - `GET /api/gemini/subscription-status` — query Gemini subscription status
///
/// All routes require authentication (applied by the caller).
pub fn connection_test_routes(state: ConnectionTestRouterState) -> Router {
    Router::new()
        .route("/api/bedrock/test-connection", post(test_bedrock))
        .route(
            "/api/gemini/subscription-status",
            get(gemini_subscription_status),
        )
        .with_state(state)
}

/// POST /api/bedrock/test-connection
///
/// Test AWS Bedrock credentials with a lightweight API call.
/// Returns 200 on success, 400 for validation errors, 422-equivalent for
/// invalid credentials (mapped to 400 with descriptive message).
async fn test_bedrock(
    State(state): State<ConnectionTestRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<TestBedrockConnectionRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    state
        .service
        .test_bedrock_connection(req.bedrock_config)
        .await?;
    Ok(Json(ApiResponse::message("Connection successful")))
}

/// GET /api/gemini/subscription-status
///
/// Query Gemini CLI subscription status. Supports optional proxy parameter.
/// Reads GEMINI_API_KEY from environment.
async fn gemini_subscription_status(
    State(state): State<ConnectionTestRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Query(query): Query<GeminiSubscriptionQuery>,
) -> Result<Json<ApiResponse<GeminiSubscriptionData>>, AppError> {
    let api_key = std::env::var("GEMINI_API_KEY").map_err(|_| {
        AppError::BadRequest("GEMINI_API_KEY environment variable not set".into())
    })?;
    let data = state
        .service
        .get_gemini_subscription_status(&api_key, query.proxy.as_deref())
        .await?;
    Ok(Json(ApiResponse::ok(data)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_router_state_clone() {
        let state = ConnectionTestRouterState {
            service: ConnectionTestService::new(reqwest::Client::new()),
        };
        let _cloned = state.clone();
    }

    #[test]
    fn test_router_construction() {
        let state = ConnectionTestRouterState {
            service: ConnectionTestService::new(reqwest::Client::new()),
        };
        let _router = connection_test_routes(state);
    }
}
