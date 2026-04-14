use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;

use aionui_api_types::{
    ApiResponse, BatchImportMcpServersRequest, CreateMcpServerRequest, DetectedMcpServerResponse,
    McpServerResponse, McpSyncResult, RemoveFromAgentsRequest, SyncToAgentsRequest,
    UpdateMcpServerRequest,
};
use aionui_common::AppError;

use crate::service::McpConfigService;
use crate::sync_service::McpSyncService;

// ---------------------------------------------------------------------------
// Router state
// ---------------------------------------------------------------------------

/// Shared state for MCP route handlers.
#[derive(Clone)]
pub struct McpRouterState {
    pub config_service: McpConfigService,
    pub sync_service: McpSyncService,
}

// ---------------------------------------------------------------------------
// Router builder
// ---------------------------------------------------------------------------

/// Build the MCP router with all `/api/mcp/*` routes.
///
/// Includes CRUD routes and agent sync routes.
/// All routes require authentication (applied by the caller).
pub fn mcp_routes(state: McpRouterState) -> Router {
    Router::new()
        .route(
            "/api/mcp/servers",
            get(list_servers).post(add_server),
        )
        .route("/api/mcp/servers/import", post(batch_import))
        .route(
            "/api/mcp/servers/{id}",
            get(get_server).put(edit_server).delete(delete_server),
        )
        .route("/api/mcp/servers/{id}/toggle", post(toggle_server))
        // Agent sync routes
        .route("/api/mcp/agent-configs", get(get_agent_configs))
        .route("/api/mcp/sync-to-agents", post(sync_to_agents))
        .route("/api/mcp/remove-from-agents", post(remove_from_agents))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// CRUD Handlers
// ---------------------------------------------------------------------------

/// `GET /api/mcp/servers` — list all MCP servers.
async fn list_servers(
    State(state): State<McpRouterState>,
) -> Result<Json<ApiResponse<Vec<McpServerResponse>>>, AppError> {
    let servers = state.config_service.list_servers().await?;
    Ok(Json(ApiResponse::ok(servers)))
}

/// `GET /api/mcp/servers/:id` — get a single MCP server.
async fn get_server(
    State(state): State<McpRouterState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<McpServerResponse>>, AppError> {
    let server = state.config_service.get_server(&id).await?;
    Ok(Json(ApiResponse::ok(server)))
}

/// `POST /api/mcp/servers` — create (or upsert by name) an MCP server.
async fn add_server(
    State(state): State<McpRouterState>,
    body: Result<Json<CreateMcpServerRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ApiResponse<McpServerResponse>>), AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let server = state.config_service.add_server(req).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::ok(server))))
}

/// `PUT /api/mcp/servers/:id` — partial update an MCP server.
async fn edit_server(
    State(state): State<McpRouterState>,
    Path(id): Path<String>,
    body: Result<Json<UpdateMcpServerRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<McpServerResponse>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let server = state.config_service.edit_server(&id, req).await?;
    Ok(Json(ApiResponse::ok(server)))
}

/// `DELETE /api/mcp/servers/:id` — delete an MCP server.
///
/// If the deleted server was enabled, triggers remove-from-agents.
async fn delete_server(
    State(state): State<McpRouterState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let server = state.config_service.get_server(&id).await?;
    let was_enabled = server.enabled;
    let server_name = server.name.clone();
    state.config_service.delete_server(&id).await?;

    if was_enabled {
        let _ = state
            .sync_service
            .remove_from_agents(&[server_name])
            .await;
    }

    Ok(Json(ApiResponse::success()))
}

/// `POST /api/mcp/servers/:id/toggle` — toggle enabled state.
///
/// Triggers sync or remove based on the new enabled state.
async fn toggle_server(
    State(state): State<McpRouterState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<McpServerResponse>>, AppError> {
    let server = state.config_service.toggle_server(&id).await?;

    if server.enabled {
        let _ = state
            .sync_service
            .sync_to_agents(std::slice::from_ref(&server.id))
            .await;
    } else {
        let _ = state
            .sync_service
            .remove_from_agents(std::slice::from_ref(&server.name))
            .await;
    }

    Ok(Json(ApiResponse::ok(server)))
}

/// `POST /api/mcp/servers/import` — batch import MCP servers.
async fn batch_import(
    State(state): State<McpRouterState>,
    body: Result<Json<BatchImportMcpServersRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Vec<McpServerResponse>>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let servers = state.config_service.batch_import(req).await?;
    Ok(Json(ApiResponse::ok(servers)))
}

// ---------------------------------------------------------------------------
// Agent Sync Handlers
// ---------------------------------------------------------------------------

/// `GET /api/mcp/agent-configs` — scan all installed Agent CLIs
/// and return their current MCP server configurations.
async fn get_agent_configs(
    State(state): State<McpRouterState>,
) -> Result<Json<ApiResponse<Vec<DetectedMcpServerResponse>>>, AppError> {
    let configs = state.sync_service.get_agent_configs().await?;
    Ok(Json(ApiResponse::ok(configs)))
}

/// `POST /api/mcp/sync-to-agents` — sync specified servers to all agents.
async fn sync_to_agents(
    State(state): State<McpRouterState>,
    body: Result<Json<SyncToAgentsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<McpSyncResult>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let result = state.sync_service.sync_to_agents(&req.servers).await?;
    Ok(Json(ApiResponse::ok(result)))
}

/// `POST /api/mcp/remove-from-agents` — remove named servers from all agents.
async fn remove_from_agents(
    State(state): State<McpRouterState>,
    body: Result<Json<RemoveFromAgentsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<McpSyncResult>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let result = state
        .sync_service
        .remove_from_agents(&req.server_names)
        .await?;
    Ok(Json(ApiResponse::ok(result)))
}
