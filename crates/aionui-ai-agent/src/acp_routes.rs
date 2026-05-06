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

use crate::acp_agent_service;
use crate::agent_registry::AgentRegistry;
use crate::task_manager::IWorkerTaskManager;
use aionui_api_types::AcpModelInfo;

/// Router state for ACP management routes.
#[derive(Clone)]
pub struct AcpRouterState {
    pub worker_task_manager: Arc<dyn IWorkerTaskManager>,
    pub agent_registry: Arc<AgentRegistry>,
}

/// Build the ACP management router.
///
/// Includes global ACP routes.
/// All routes require authentication (applied by the caller).
pub fn acp_routes(state: AcpRouterState) -> Router {
    Router::new()
        // Global ACP management routes
        .route("/api/acp/detect-cli", post(detect_cli))
        .route("/api/acp/health-check", post(health_check))
        .route("/api/acp/env", get(get_env))
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
    let result = acp_agent_service::detect_cli(&state.agent_registry, &req.backend).await;
    Ok(Json(ApiResponse::ok(result)))
}

async fn health_check(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<AcpHealthCheckRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AcpHealthCheckResponse>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let result = acp_agent_service::health_check(&state.agent_registry, &req.backend).await;
    Ok(Json(ApiResponse::ok(result)))
}

async fn get_env(
    State(_state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<AcpEnvResponse>>, AppError> {
    let result = acp_agent_service::get_env();
    Ok(Json(ApiResponse::ok(result)))
}

async fn probe_model(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<ProbeModelRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Option<AcpModelInfo>>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    // Probe model requires a running ACP session; for now verify CLI availability
    let detection = acp_agent_service::detect_cli(&state.agent_registry, &req.backend).await;
    if detection.path.is_none() {
        return Err(AppError::BadRequest(format!(
            "Backend '{}' CLI not found, cannot probe model",
            req.backend
        )));
    }
    // Full model probing will be wired when integrated with real ACP sessions (6.15)
    Ok(Json(ApiResponse::ok(None)))
}
