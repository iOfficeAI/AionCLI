use std::sync::Arc;

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, Path, State};
use axum::routing::{get, post, put};

use aionui_api_types::{
    AcpAgentInfo, AcpEnvResponse, AcpHealthCheckRequest, AcpHealthCheckResponse, AcpModeResponse,
    ApiResponse, DetectCliRequest, DetectCliResponse, ProbeModelRequest, SetConfigOptionRequest,
    SetModeRequest, SetModelRequest, TestCustomAgentRequest, TestCustomAgentResponse,
};
use aionui_auth::CurrentUser;
use aionui_common::{AgentType, AppError};

use crate::acp_agent::AcpAgentManager;
use crate::acp_service;
use crate::agent_manager::AgentManagerHandle;
use crate::agent_registry::AgentRegistry;
use crate::task_manager::IWorkerTaskManager;
use crate::types::{AcpModelInfo, AcpSessionConfigOption};

/// Router state for ACP management routes.
#[derive(Clone)]
pub struct AcpRouterState {
    pub worker_task_manager: Arc<dyn IWorkerTaskManager>,
    pub agent_registry: Arc<AgentRegistry>,
}

/// Build the ACP management router.
///
/// Includes both global ACP routes and per-conversation session routes.
/// All routes require authentication (applied by the caller).
pub fn acp_routes(state: AcpRouterState) -> Router {
    Router::new()
        // Global ACP management routes
        .route("/api/acp/detect-cli", post(detect_cli))
        .route("/api/acp/agents", get(list_agents))
        .route("/api/acp/agents/refresh", post(refresh_agents))
        .route("/api/acp/agents/test", post(test_custom_agent))
        .route("/api/acp/health-check", post(health_check))
        .route("/api/acp/env", get(get_env))
        .route("/api/acp/probe-model", post(probe_model))
        // Per-conversation ACP session routes
        .route(
            "/api/conversations/{id}/acp/mode",
            get(get_mode).put(set_mode),
        )
        .route(
            "/api/conversations/{id}/acp/model",
            get(get_model).put(set_model),
        )
        .route("/api/conversations/{id}/acp/config", get(get_config))
        .route(
            "/api/conversations/{id}/acp/config/{configId}",
            put(set_config_option),
        )
        .with_state(state)
}

/// Get the active ACP agent task for a conversation.
///
/// Returns the handle (keeping the Arc alive) so callers can downcast.
/// Returns `NotFound` if no active task exists, `BadRequest` if not ACP.
fn require_acp_task(
    state: &AcpRouterState,
    conversation_id: &str,
) -> Result<AgentManagerHandle, AppError> {
    let handle = state
        .worker_task_manager
        .get_task(conversation_id)
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "No active agent for conversation '{conversation_id}'"
            ))
        })?;

    if handle.agent_type() != AgentType::Acp {
        return Err(AppError::BadRequest(
            "This operation is only available for ACP agents".into(),
        ));
    }

    Ok(handle)
}

/// Downcast an `AgentManagerHandle` to `&AcpAgentManager`.
fn downcast_acp(handle: &AgentManagerHandle) -> Result<&AcpAgentManager, AppError> {
    handle
        .as_any()
        .downcast_ref::<AcpAgentManager>()
        .ok_or_else(|| AppError::Internal("Failed to downcast agent to AcpAgentManager".into()))
}

// ── Global ACP routes ────────────────────────────────────────────

async fn detect_cli(
    State(_state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<DetectCliRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<DetectCliResponse>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let result = acp_service::detect_cli(req.backend);
    Ok(Json(ApiResponse::ok(result)))
}

async fn list_agents(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<Vec<AcpAgentInfo>>>, AppError> {
    let agents = state.agent_registry.get_all().await;
    let infos: Vec<AcpAgentInfo> = agents.into_iter().map(Into::into).collect();
    Ok(Json(ApiResponse::ok(infos)))
}

async fn refresh_agents(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<Vec<AcpAgentInfo>>>, AppError> {
    state.agent_registry.refresh_builtins().await;
    let agents = state.agent_registry.get_all().await;
    let infos: Vec<AcpAgentInfo> = agents.into_iter().map(Into::into).collect();
    Ok(Json(ApiResponse::ok(infos)))
}

async fn test_custom_agent(
    State(_state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<TestCustomAgentRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<TestCustomAgentResponse>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let result = acp_service::test_custom_agent(&req.command, &req.acp_args, &req.env)?;
    Ok(Json(ApiResponse::ok(result)))
}

async fn health_check(
    State(_state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<AcpHealthCheckRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AcpHealthCheckResponse>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let result = acp_service::health_check(req.backend);
    Ok(Json(ApiResponse::ok(result)))
}

async fn get_env(
    State(_state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<AcpEnvResponse>>, AppError> {
    let result = acp_service::get_env();
    Ok(Json(ApiResponse::ok(result)))
}

async fn probe_model(
    State(_state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<ProbeModelRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Option<AcpModelInfo>>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    // Probe model requires a running ACP session; for now verify CLI availability
    let detection = acp_service::detect_cli(req.backend);
    if detection.path.is_none() {
        return Err(AppError::BadRequest(format!(
            "Backend {:?} CLI not found, cannot probe model",
            req.backend
        )));
    }
    // Full model probing will be wired when integrated with real ACP sessions (6.15)
    Ok(Json(ApiResponse::ok(None)))
}

// ── Per-conversation ACP session routes ──────────────────────────

async fn get_mode(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<AcpModeResponse>>, AppError> {
    let handle = require_acp_task(&state, &id)?;
    let acp = downcast_acp(&handle)?;
    acp.get_mode().await?;
    // The actual mode is returned via the event stream; provide best-effort sync state
    Ok(Json(ApiResponse::ok(AcpModeResponse {
        mode: String::new(),
        initialized: acp.session_id().await.is_some(),
    })))
}

async fn set_mode(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<SetModeRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    if req.mode.trim().is_empty() {
        return Err(AppError::BadRequest("mode must not be empty".into()));
    }
    let handle = require_acp_task(&state, &id)?;
    let acp = downcast_acp(&handle)?;
    acp.set_mode(&req.mode).await?;
    Ok(Json(ApiResponse::success()))
}

async fn get_model(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Option<AcpModelInfo>>>, AppError> {
    let handle = require_acp_task(&state, &id)?;
    let acp = downcast_acp(&handle)?;
    let info = acp.get_model_info().await;
    Ok(Json(ApiResponse::ok(info)))
}

async fn set_model(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<SetModelRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    if req.model_id.trim().is_empty() {
        return Err(AppError::BadRequest("modelId must not be empty".into()));
    }
    let handle = require_acp_task(&state, &id)?;
    let acp = downcast_acp(&handle)?;
    acp.set_model(&req.model_id).await?;
    Ok(Json(ApiResponse::success()))
}

async fn get_config(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<AcpSessionConfigOption>>>, AppError> {
    let handle = require_acp_task(&state, &id)?;
    let acp = downcast_acp(&handle)?;
    acp.get_config_options().await?;
    // Config options arrive via event stream; return empty for sync response
    Ok(Json(ApiResponse::ok(Vec::new())))
}

#[derive(serde::Deserialize)]
struct ConfigPathParams {
    id: String,
    #[serde(rename = "configId")]
    config_id: String,
}

async fn set_config_option(
    State(state): State<AcpRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(params): Path<ConfigPathParams>,
    body: Result<Json<SetConfigOptionRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let handle = require_acp_task(&state, &params.id)?;
    let acp = downcast_acp(&handle)?;
    acp.set_config_option(&params.config_id, &req.value).await?;
    Ok(Json(ApiResponse::success()))
}
