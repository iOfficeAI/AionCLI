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

use crate::agent_task::AgentInstance;
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
    if req.question.trim().is_empty() {
        return Err(AppError::BadRequest("question must not be empty".into()));
    }

    let instance = get_task(&state, &id)?;

    let AgentInstance::Acp(acp) = &instance else {
        return Ok(Json(ApiResponse::ok(SideQuestionResponse {
            status: "unsupported".into(),
            answer: None,
        })));
    };

    // Side question is gated by the agent's behavior_policy flag.
    if !acp.supports_side_question() {
        return Ok(Json(ApiResponse::ok(SideQuestionResponse {
            status: "unsupported".into(),
            answer: None,
        })));
    }

    // Side question is implemented by sending a special message to the ACP CLI.
    // The actual implementation requires forking the ACP session, which will
    // be fully wired in Phase 6.15 App Integration.
    // For now, return a placeholder indicating the feature exists but is pending integration.
    Ok(Json(ApiResponse::ok(SideQuestionResponse {
        status: "ok".into(),
        answer: Some("Side question support will be fully wired in app integration phase.".into()),
    })))
}

async fn get_slash_commands(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<SlashCommandItem>>>, AppError> {
    let instance = get_task(&state, &id)?;

    // Only ACP agents have slash commands; other agent types return an
    // empty list rather than an error — the UI renders "no commands".
    let AgentInstance::Acp(acp) = &instance else {
        return Ok(Json(ApiResponse::ok(Vec::new())));
    };

    let commands = acp.load_slash_commands().await?;
    Ok(Json(ApiResponse::ok(commands)))
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
    let instance = get_task(&state, &params.id)?;
    let AgentInstance::Acp(acp) = &instance else {
        return Err(AppError::BadRequest(
            "Config options are only available for ACP agents".into(),
        ));
    };
    let config_option = acp
        .config_options()
        .await
        .into_iter()
        .find(|opt| *opt.id.0 == *params.config_id);
    Ok(Json(ApiResponse::ok(config_option)))
}

async fn set_config(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(params): Path<ConfigPathParams>,
    body: Result<Json<SetConfigOptionRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let instance = get_task(&state, &params.id)?;
    let AgentInstance::Acp(acp) = &instance else {
        return Err(AppError::BadRequest(
            "Config updates are not supported for this agent type".into(),
        ));
    };

    acp.set_config_option(&params.config_id, &req.value).await?;
    Ok(Json(ApiResponse::success()))
}

async fn get_configs(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<SessionConfigOption>>>, AppError> {
    let instance = get_task(&state, &id)?;
    let AgentInstance::Acp(acp) = &instance else {
        return Err(AppError::BadRequest(
            "Config options are only available for ACP agents".into(),
        ));
    };
    Ok(Json(ApiResponse::ok(acp.config_options().await)))
}

async fn set_configs(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<SetConfigOptionsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let instance = get_task(&state, &id)?;
    let AgentInstance::Acp(acp) = &instance else {
        return Err(AppError::BadRequest(
            "Config updates are not supported for this agent type".into(),
        ));
    };

    for update in req.config_options {
        if update.config_id.trim().is_empty() {
            return Err(AppError::BadRequest("config_id must not be empty".into()));
        }
        acp.set_config_option(&update.config_id, &update.value).await?;
    }

    Ok(Json(ApiResponse::success()))
}

async fn get_usage(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Option<UsageUpdate>>>, AppError> {
    let instance = get_task(&state, &id)?;
    let AgentInstance::Acp(acp) = &instance else {
        return Err(AppError::BadRequest(
            "Usage stats are only available for ACP agents".into(),
        ));
    };
    let usage = acp.usage().await;
    Ok(Json(ApiResponse::ok(usage)))
}

async fn get_agent_capabilities(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Option<AgentCapabilities>>>, AppError> {
    let instance = get_task(&state, &id)?;
    let AgentInstance::Acp(acp) = &instance else {
        return Err(AppError::BadRequest(
            "Agent capabilities are only available for ACP agents".into(),
        ));
    };
    let capabilities = acp.agent_capabilities().await;
    Ok(Json(ApiResponse::ok(capabilities)))
}

async fn get_openclaw_runtime(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let instance = get_task(&state, &id)?;
    let AgentInstance::OpenClaw(openclaw) = &instance else {
        return Err(AppError::BadRequest(
            "This endpoint is only available for OpenClaw agents".into(),
        ));
    };

    let diagnostics = openclaw.get_diagnostics().await;
    Ok(Json(ApiResponse::ok(diagnostics)))
}

// ── Helpers ────────────────────────────────────────────────────────

fn get_task(state: &SessionRouterState, conversation_id: &str) -> Result<AgentInstance, AppError> {
    state
        .worker_task_manager
        .get_task(conversation_id)
        .ok_or_else(|| AppError::NotFound(format!("No active agent for conversation '{conversation_id}'")))
}
