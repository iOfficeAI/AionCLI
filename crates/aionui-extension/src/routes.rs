use std::collections::HashMap;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, State};
use axum::routing::{get, post};
use axum::Router;

use aionui_api_types::{
    ApiResponse, DisableExtensionRequest, EnableExtensionRequest, ExtensionSummaryResponse,
    GetI18nRequest, GetPermissionsRequest, GetRiskLevelRequest, PermissionDetailResponse,
    PermissionSummaryResponse,
};
use aionui_common::AppError;

use crate::permission::{build_permission_summary, calculate_risk_level};
use crate::registry::ExtensionRegistry;

// ---------------------------------------------------------------------------
// Router state
// ---------------------------------------------------------------------------

/// Shared state for extension route handlers.
#[derive(Clone)]
pub struct ExtensionRouterState {
    pub registry: ExtensionRegistry,
}

// ---------------------------------------------------------------------------
// Router builder
// ---------------------------------------------------------------------------

/// Build the extension router with all `/api/extensions/*` routes.
///
/// Includes query routes and management routes.
/// All routes require authentication (applied by the caller).
pub fn extension_routes(state: ExtensionRouterState) -> Router {
    Router::new()
        // Query routes
        .route("/api/extensions", get(get_loaded_extensions))
        .route("/api/extensions/themes", get(get_themes))
        .route("/api/extensions/assistants", get(get_assistants))
        .route("/api/extensions/acp-adapters", get(get_acp_adapters))
        .route("/api/extensions/agents", get(get_agents))
        .route("/api/extensions/mcp-servers", get(get_mcp_servers))
        .route("/api/extensions/skills", get(get_skills))
        .route("/api/extensions/settings-tabs", get(get_settings_tabs))
        .route("/api/extensions/webui", get(get_webui))
        .route("/api/extensions/agent-activity", get(get_agent_activity))
        // Query routes with body
        .route("/api/extensions/i18n", post(get_i18n))
        .route("/api/extensions/permissions", post(get_permissions))
        .route("/api/extensions/risk-level", post(get_risk_level))
        // Management routes
        .route("/api/extensions/enable", post(enable_extension))
        .route("/api/extensions/disable", post(disable_extension))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Query handlers
// ---------------------------------------------------------------------------

/// `GET /api/extensions` — list all loaded extensions.
async fn get_loaded_extensions(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<Vec<ExtensionSummaryResponse>>>, AppError> {
    let summaries = state.registry.get_loaded_extensions().await;
    let resp: Vec<ExtensionSummaryResponse> = summaries
        .into_iter()
        .map(|s| {
            let source_str = serde_json::to_value(s.source)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "local".to_string());
            ExtensionSummaryResponse {
                name: s.name,
                version: s.version,
                display_name: s.display_name,
                description: s.description,
                enabled: s.enabled,
                source: source_str,
            }
        })
        .collect();
    Ok(Json(ApiResponse::ok(resp)))
}

/// `GET /api/extensions/themes` — get all resolved themes.
async fn get_themes(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let themes = state.registry.get_themes().await;
    let value = serde_json::to_value(&themes).unwrap_or_default();
    Ok(Json(ApiResponse::ok(value)))
}

/// `GET /api/extensions/assistants` — get all resolved assistants.
async fn get_assistants(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let assistants = state.registry.get_assistants().await;
    let value = serde_json::to_value(&assistants).unwrap_or_default();
    Ok(Json(ApiResponse::ok(value)))
}

/// `GET /api/extensions/acp-adapters` — get all resolved ACP adapters.
async fn get_acp_adapters(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let adapters = state.registry.get_acp_adapters().await;
    let value = serde_json::to_value(&adapters).unwrap_or_default();
    Ok(Json(ApiResponse::ok(value)))
}

/// `GET /api/extensions/agents` — get all resolved agents.
async fn get_agents(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let agents = state.registry.get_agents().await;
    let value = serde_json::to_value(&agents).unwrap_or_default();
    Ok(Json(ApiResponse::ok(value)))
}

/// `GET /api/extensions/mcp-servers` — get all resolved MCP servers.
async fn get_mcp_servers(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let servers = state.registry.get_mcp_servers().await;
    let value = serde_json::to_value(&servers).unwrap_or_default();
    Ok(Json(ApiResponse::ok(value)))
}

/// `GET /api/extensions/skills` — get all resolved skills.
async fn get_skills(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let skills = state.registry.get_skills().await;
    let value = serde_json::to_value(&skills).unwrap_or_default();
    Ok(Json(ApiResponse::ok(value)))
}

/// `GET /api/extensions/settings-tabs` — get all resolved settings tabs.
async fn get_settings_tabs(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let tabs = state.registry.get_settings_tabs().await;
    let value = serde_json::to_value(&tabs).unwrap_or_default();
    Ok(Json(ApiResponse::ok(value)))
}

/// `GET /api/extensions/webui` — get all WebUI contributions.
async fn get_webui(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let webui = state.registry.get_webui_contributions().await;
    let value = serde_json::to_value(&webui).unwrap_or_default();
    Ok(Json(ApiResponse::ok(value)))
}

/// `GET /api/extensions/agent-activity` — get agent activity snapshot.
///
/// Returns an empty object as a placeholder; real implementation will
/// integrate with the agent subsystem's activity tracking.
async fn get_agent_activity(
    State(_state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    // Agent activity snapshot is a cross-module concern;
    // return an empty object for now.
    Ok(Json(ApiResponse::ok(serde_json::json!({}))))
}

/// `POST /api/extensions/i18n` — get i18n data for a locale.
async fn get_i18n(
    State(state): State<ExtensionRouterState>,
    body: Result<Json<GetI18nRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<HashMap<String, HashMap<String, String>>>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let data = state.registry.get_i18n_for_locale(&req.locale).await;
    Ok(Json(ApiResponse::ok(data)))
}

/// `POST /api/extensions/permissions` — get permission summary for an extension.
async fn get_permissions(
    State(state): State<ExtensionRouterState>,
    body: Result<Json<GetPermissionsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<PermissionSummaryResponse>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;

    let ext = state
        .registry
        .get_extension_by_name(&req.name)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Extension not found: {}", req.name)))?;

    let permissions = ext.manifest.permissions.clone().unwrap_or_default();
    let summary = build_permission_summary(&permissions);
    let risk_level = calculate_risk_level(&permissions);

    let details: Vec<PermissionDetailResponse> = summary
        .details
        .into_iter()
        .map(|d| PermissionDetailResponse {
            permission: d.permission,
            level: enum_to_string(&d.level),
            description: d.description,
        })
        .collect();

    let resp = PermissionSummaryResponse {
        permissions: serde_json::to_value(&permissions).unwrap_or_default(),
        risk_level: enum_to_string(&risk_level),
        details,
    };

    Ok(Json(ApiResponse::ok(resp)))
}

/// `POST /api/extensions/risk-level` — get risk level for an extension.
async fn get_risk_level(
    State(state): State<ExtensionRouterState>,
    body: Result<Json<GetRiskLevelRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;

    let ext = state
        .registry
        .get_extension_by_name(&req.name)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Extension not found: {}", req.name)))?;

    let permissions = ext.manifest.permissions.clone().unwrap_or_default();
    let risk_level = calculate_risk_level(&permissions);

    Ok(Json(ApiResponse::ok(
        serde_json::json!({ "riskLevel": enum_to_string(&risk_level) }),
    )))
}

// ---------------------------------------------------------------------------
// Management handlers
// ---------------------------------------------------------------------------

/// `POST /api/extensions/enable` — enable an extension.
async fn enable_extension(
    State(state): State<ExtensionRouterState>,
    body: Result<Json<EnableExtensionRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    state.registry.enable_extension(&req.name).await?;
    Ok(Json(ApiResponse::success()))
}

/// `POST /api/extensions/disable` — disable an extension.
async fn disable_extension(
    State(state): State<ExtensionRouterState>,
    body: Result<Json<DisableExtensionRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    state
        .registry
        .disable_extension(&req.name, req.reason.as_deref())
        .await?;
    Ok(Json(ApiResponse::success()))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Serialize a serde enum to its JSON string representation.
fn enum_to_string<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::state::ExtensionStateStore;
    use aionui_realtime::BroadcastEventBus;

    fn make_state() -> ExtensionRouterState {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = ExtensionStateStore::new(tmp.path().join("states.json"));
        let bus = Arc::new(BroadcastEventBus::new(64));
        std::mem::forget(tmp);
        let registry = ExtensionRegistry::new(store, bus, "1.0.0".into());
        ExtensionRouterState { registry }
    }

    #[test]
    fn extension_routes_builds_router() {
        let state = make_state();
        let _router = extension_routes(state);
    }
}
