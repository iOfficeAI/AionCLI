//! Conversation-level operations that do **not** depend on the concrete
//! [`AgentInstance`] variant.
//!
//! Endpoints:
//!
//! - `GET  /api/conversations/{id}/workspace`       — workspace browse
//! - `POST /api/conversations/{id}/reload-context`  — trigger context reload

use std::path::Component;

use axum::Router;
use axum::extract::{Extension, Json, Path, Query, State};
use axum::routing::{get, post};

use aionui_api_types::{ApiResponse, WorkspaceBrowseQuery, WorkspaceEntry};
use aionui_auth::CurrentUser;
use aionui_common::AppError;

use crate::routes::SessionRouterState;

// ── Max depth for workspace traversal ──────────────────────────────
const MAX_DIR_DEPTH: usize = 10;

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
    if query.path.trim().is_empty() {
        return Err(AppError::BadRequest("path must not be empty".into()));
    }

    let row = state
        .conversation_repo
        .get(&id)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to load conversation: {e}")))?
        .ok_or_else(|| AppError::NotFound(format!("Conversation '{id}' not found")))?;

    let extra: serde_json::Value =
        serde_json::from_str(&row.extra).map_err(|e| AppError::Internal(format!("Invalid extra JSON: {e}")))?;
    let workspace = extra
        .get("workspace")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    if workspace.is_empty() {
        return Err(AppError::BadRequest("Conversation has no workspace assigned".into()));
    }

    let relative_path = query.path.trim_start_matches('/');
    let relative_path_obj = std::path::Path::new(relative_path);
    if relative_path_obj
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(AppError::BadRequest(
            "Path traversal outside workspace is not allowed".into(),
        ));
    }

    // Resolve the browsed path relative to the workspace root
    let base = std::path::Path::new(&workspace);
    let browse_path = if relative_path.is_empty() {
        base.to_path_buf()
    } else {
        base.join(relative_path_obj)
    };

    // Security: reject direct traversal outside the workspace root, but allow
    // symlinked directories mounted inside the workspace (e.g. native skill
    // dirs that point at the builtin skills corpus under data-dir).
    let canonical_base = base
        .canonicalize()
        .map_err(|e| AppError::Internal(format!("Failed to resolve workspace path: {e}")))?;
    let canonical_browse = browse_path
        .canonicalize()
        .map_err(|_| AppError::NotFound("Directory not found".into()))?;
    if !browse_path.starts_with(base) && !canonical_browse.starts_with(&canonical_base) {
        return Err(AppError::BadRequest(
            "Path traversal outside workspace is not allowed".into(),
        ));
    }

    // Check depth limit
    let depth = relative_path_obj.components().count();
    if depth > MAX_DIR_DEPTH {
        return Err(AppError::BadRequest(format!(
            "Directory depth exceeds maximum of {MAX_DIR_DEPTH}"
        )));
    }

    let mut entries = Vec::new();
    let mut dir_reader = tokio::fs::read_dir(&canonical_browse)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read directory: {e}")))?;

    while let Ok(Some(entry)) = dir_reader.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();

        // Apply search filter if provided
        if let Some(ref search) = query.search
            && !search.is_empty()
            && !name.to_lowercase().contains(&search.to_lowercase())
        {
            continue;
        }

        let entry_path = entry.path();
        let metadata = tokio::fs::metadata(&entry_path)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to read entry metadata: {e}")))?;

        let entry_type = if metadata.is_dir() { "directory" } else { "file" };

        entries.push(WorkspaceEntry {
            name,
            entry_type: entry_type.into(),
        });
    }

    // Sort: directories first, then alphabetically
    entries.sort_by(|a, b| {
        let type_cmp = a.entry_type.cmp(&b.entry_type);
        if type_cmp == std::cmp::Ordering::Equal {
            a.name.to_lowercase().cmp(&b.name.to_lowercase())
        } else {
            type_cmp
        }
    });

    Ok(Json(ApiResponse::ok(entries)))
}

async fn reload_context(
    State(state): State<SessionRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    state.service.reload_context(&id).await?;
    Ok(Json(ApiResponse::success()))
}
