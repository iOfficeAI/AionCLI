//! Agent-related API routes.
//!
//! Endpoints:
//!
//! - `GET  /api/agents`         — list available agents
//! - `POST /api/agents/refresh` — refresh agent list (e.g. after new agent is added to the system)
//! - `POST /api/agents/test`    — test custom agent configuration (e.g. LLM connection)

use std::sync::Arc;

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, State};
use axum::routing::{get, post};

use aionui_api_types::{AgentMetadata, ApiResponse, TestCustomAgentRequest, TestCustomAgentResponse};
use aionui_auth::CurrentUser;
use aionui_common::AppError;

use crate::registry::AgentRegistry;

#[derive(Clone)]
pub struct AgentRouterState {
    pub agent_registry: Arc<AgentRegistry>,
    pub service: Arc<crate::service::AgentService>,
}

pub fn agent_routes(state: AgentRouterState) -> Router {
    Router::new()
        .route("/api/agents", get(list_agents))
        .route("/api/agents/refresh", post(refresh_agents))
        .route("/api/agents/test", post(test_custom_agent))
        .with_state(state)
}

async fn list_agents(
    State(state): State<AgentRouterState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<Vec<AgentMetadata>>>, AppError> {
    Ok(Json(ApiResponse::ok(state.service.list_agents().await?)))
}

async fn refresh_agents(
    State(state): State<AgentRouterState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<Vec<AgentMetadata>>>, AppError> {
    Ok(Json(ApiResponse::ok(state.service.refresh_agents().await?)))
}

async fn test_custom_agent(
    State(state): State<AgentRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<TestCustomAgentRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<TestCustomAgentResponse>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Json(ApiResponse::ok(state.service.test_custom_agent(req)?)))
}
