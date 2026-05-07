//! Conversation-level operations that do **not** depend on the concrete
//! [`AgentInstance`] variant.
//!
//! Endpoints:
//!
//! - `GET  /api/conversations/{id}/workspace`       — workspace browse
//! - `POST /api/conversations/{id}/reload-context`  — trigger context reload

use axum::Router;
use axum::extract::{Extension, Json, Path, Query, State};
use axum::routing::{get, post};

use aionui_api_types::{ApiResponse, WorkspaceBrowseQuery, WorkspaceEntry};
use aionui_auth::CurrentUser;
use aionui_common::AppError;

use crate::routes::SessionRouterState;

/// Build the conversation-ops router (no auth layer applied — the caller
/// is responsible for wrapping this with the auth middleware).
pub fn conversation_ops_routes(state: SessionRouterState) -> Router {
    Router::new()
        .route("/api/conversations/{id}/workspace", get(browse_workspace))
        .route("/api/conversations/{id}/reload-context", post(reload_context))
        .with_state(state)
}

// ── Route handlers ─────────────────────────────────────────────────

async fn browse_workspace(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    Query(query): Query<WorkspaceBrowseQuery>,
) -> Result<Json<ApiResponse<Vec<WorkspaceEntry>>>, AppError> {
    Ok(Json(ApiResponse::ok(state.service.browse_workspace(&id, query).await?)))
}

async fn reload_context(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    state.service.reload_context(&id).await?;
    Ok(Json(ApiResponse::success()))
}
