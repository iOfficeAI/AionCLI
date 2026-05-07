//! Session-level operations that require dispatching on the concrete
//! [`AgentInstance`] variant (ACP / OpenClaw / …).
//!
//! All handlers go through [`AgentInstance`] match arms: when the running
//! agent is not of the required type the response is a `BadRequest` with
//! an explicit message, not an `Internal` error.
//!
//! Endpoints:
//!
//! - `GET  /api/conversations/{id}/mode`
//! - `PUT  /api/conversations/{id}/mode`
//! - `GET  /api/conversations/{id}/model`
//! - `PUT  /api/conversations/{id}/model`
//! - `GET  /api/conversations/{id}/config`
//! - `PUT  /api/conversations/{id}/config`
//! - `GET  /api/conversations/{id}/config/{configId}`
//! - `PUT  /api/conversations/{id}/config/{configId}`
//! - `GET  /api/conversations/{id}/usage`
//! - `GET  /api/conversations/{id}/agent-capabilities`
//! - `GET  /api/conversations/{id}/openclaw/runtime`
//! - `POST /api/conversations/{id}/side-question`
//! - `GET  /api/conversations/{id}/slash-commands`

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, Path, State};
use axum::routing::{get, post};

use agent_client_protocol::schema::{AgentCapabilities, SessionConfigOption, UsageUpdate};
use aionui_api_types::{
    AgentModeResponse, ApiResponse, GetModelInfoResponse, SetConfigOptionRequest, SetConfigOptionsRequest,
    SetModeRequest, SetModelRequest, SideQuestionRequest, SideQuestionResponse, SlashCommandItem,
};
use aionui_auth::CurrentUser;
use aionui_common::AppError;
use serde::Deserialize;

use crate::routes::SessionRouterState;

#[derive(Debug, Deserialize)]
struct ConfigPathParams {
    id: String,
    #[serde(rename = "configId")]
    config_id: String,
}

/// Build the session-ops router (no auth layer applied — the caller is
/// responsible for wrapping this with the auth middleware).
pub fn session_ops_routes(state: SessionRouterState) -> Router {
    Router::new()
        .route("/api/conversations/{id}/side-question", post(side_question))
        .route("/api/conversations/{id}/slash-commands", get(get_slash_commands))
        .route("/api/conversations/{id}/mode", get(get_mode).put(set_mode))
        .route("/api/conversations/{id}/model", get(get_model).put(set_model))
        .route("/api/conversations/{id}/config", get(get_configs).put(set_configs))
        .route(
            "/api/conversations/{id}/config/{configId}",
            get(get_config).put(set_config),
        )
        .route("/api/conversations/{id}/usage", get(get_usage))
        .route(
            "/api/conversations/{id}/agent-capabilities",
            get(get_agent_capabilities),
        )
        .route("/api/conversations/{id}/openclaw/runtime", get(get_openclaw_runtime))
        .with_state(state)
}

// ── Route handlers ─────────────────────────────────────────────────

async fn side_question(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    Json(req): Json<SideQuestionRequest>,
) -> Result<Json<ApiResponse<SideQuestionResponse>>, AppError> {
    Ok(Json(ApiResponse::ok(
        state.service.handle_side_question(&id, req).await?,
    )))
}

async fn get_slash_commands(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<SlashCommandItem>>>, AppError> {
    Ok(Json(ApiResponse::ok(state.service.get_slash_commands(&id).await?)))
}

async fn get_mode(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<AgentModeResponse>>, AppError> {
    Ok(Json(ApiResponse::ok(state.service.get_mode(&id).await?)))
}

async fn set_mode(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<SetModeRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    state.service.set_mode(&id, req).await?;
    Ok(Json(ApiResponse::success()))
}

async fn get_model(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<GetModelInfoResponse>>, AppError> {
    Ok(Json(ApiResponse::ok(state.service.get_model_info(&id).await?)))
}

async fn set_model(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<SetModelRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    state.service.set_model(&id, req).await?;
    Ok(Json(ApiResponse::success()))
}

async fn get_config(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(params): Path<ConfigPathParams>,
) -> Result<Json<ApiResponse<Option<SessionConfigOption>>>, AppError> {
    Ok(Json(ApiResponse::ok(
        state.service.get_config_option(&params.id, &params.config_id).await?,
    )))
}

async fn set_config(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(params): Path<ConfigPathParams>,
    body: Result<Json<SetConfigOptionRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    state
        .service
        .set_config_option(&params.id, &params.config_id, req)
        .await?;
    Ok(Json(ApiResponse::success()))
}

async fn get_configs(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<SessionConfigOption>>>, AppError> {
    Ok(Json(ApiResponse::ok(state.service.get_configs(&id).await?)))
}

async fn set_configs(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<SetConfigOptionsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    state.service.set_configs_batch(&id, req).await?;
    Ok(Json(ApiResponse::success()))
}

async fn get_usage(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Option<UsageUpdate>>>, AppError> {
    Ok(Json(ApiResponse::ok(state.service.get_usage(&id).await?)))
}

async fn get_agent_capabilities(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Option<AgentCapabilities>>>, AppError> {
    Ok(Json(ApiResponse::ok(state.service.get_agent_capabilities(&id).await?)))
}

async fn get_openclaw_runtime(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    Ok(Json(ApiResponse::ok(state.service.get_openclaw_runtime(&id).await?)))
}
