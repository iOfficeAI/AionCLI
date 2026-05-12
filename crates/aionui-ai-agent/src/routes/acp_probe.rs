use std::sync::Arc;

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, State};
use axum::routing::{get, post};

use aionui_api_types::{
    AcpEnvResponse, AcpHealthCheckRequest, AcpHealthCheckResponse, ApiResponse, DetectCliRequest, DetectCliResponse,
    ProbeModelRequest,
};
use aionui_auth::CurrentUser;
use aionui_common::AppError;

use crate::registry::AgentRegistry;
use crate::services::AgentService;
use crate::task_manager::IWorkerTaskManager;
use aionui_api_types::AcpModelInfo;

/// Router state for ACP management routes.
#[derive(Clone)]
pub struct AcpRouterState {
    pub worker_task_manager: Arc<dyn IWorkerTaskManager>,
    pub agent_registry: Arc<AgentRegistry>,
    pub service: Arc<AgentService>,
}

/// Build the ACP management router.
///
/// Includes global ACP routes.
/// All routes require authentication (applied by the caller).
pub fn acp_routes(state: AcpRouterState) -> Router {
    Router::new()
        // Global ACP management routes
        .route("/api/acp/env", get(get_env))
        .route("/api/acp/detect-cli", post(detect_cli))
        .route("/api/acp/health-check", post(health_check))
        .route("/api/acp/probe-model", post(probe_model))
        .with_state(state)
}

// ── Global ACP routes ────────────────────────────────────────────

async fn detect_cli(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<DetectCliRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<DetectCliResponse>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Json(ApiResponse::ok(state.service.detect_cli(req).await?)))
}

async fn health_check(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<AcpHealthCheckRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AcpHealthCheckResponse>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Json(ApiResponse::ok(state.service.acp_health_check(req).await?)))
}

async fn get_env(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<AcpEnvResponse>>, AppError> {
    Ok(Json(ApiResponse::ok(state.service.acp_env()?)))
}

async fn probe_model(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<ProbeModelRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Option<AcpModelInfo>>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Json(ApiResponse::ok(state.service.probe_model(req).await?)))
}
