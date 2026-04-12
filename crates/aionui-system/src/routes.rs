use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, Query, State};
use axum::routing::get;
use axum::Router;

use aionui_api_types::{
    ApiResponse, ClientPreferencesResponse, SystemSettingsResponse, UpdateClientPreferencesRequest,
    UpdateSettingsRequest,
};
use aionui_common::AppError;

use crate::client_pref::ClientPrefService;
use crate::settings::SettingsService;

/// Shared state for system settings route handlers.
#[derive(Clone)]
pub struct SystemRouterState {
    pub settings_service: SettingsService,
    pub client_pref_service: ClientPrefService,
}

/// Build the system settings router.
///
/// All routes require authentication (applied by the caller).
///
/// Endpoints:
/// - `GET  /api/settings`        — get all backend settings
/// - `PATCH /api/settings`       — partial update backend settings
/// - `GET  /api/settings/client`  — get client preferences
/// - `PUT  /api/settings/client`  — batch update client preferences
pub fn settings_routes(state: SystemRouterState) -> Router {
    Router::new()
        .route("/api/settings", get(get_settings).patch(update_settings))
        .route(
            "/api/settings/client",
            get(get_client_preferences).put(update_client_preferences),
        )
        .with_state(state)
}

// ---------------------------------------------------------------------------
// GET /api/settings
// ---------------------------------------------------------------------------

async fn get_settings(
    State(state): State<SystemRouterState>,
) -> Result<Json<ApiResponse<SystemSettingsResponse>>, AppError> {
    let settings = state.settings_service.get_settings().await?;
    Ok(Json(ApiResponse::ok(settings)))
}

// ---------------------------------------------------------------------------
// PATCH /api/settings
// ---------------------------------------------------------------------------

async fn update_settings(
    State(state): State<SystemRouterState>,
    body: Result<Json<UpdateSettingsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SystemSettingsResponse>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let settings = state.settings_service.update_settings(req).await?;
    Ok(Json(ApiResponse::ok(settings)))
}

// ---------------------------------------------------------------------------
// GET /api/settings/client
// ---------------------------------------------------------------------------

/// Query parameters for the client preferences endpoint.
#[derive(Debug, serde::Deserialize, Default)]
struct ClientPrefQuery {
    /// Comma-separated list of keys to filter by.
    keys: Option<String>,
}

async fn get_client_preferences(
    State(state): State<SystemRouterState>,
    Query(query): Query<ClientPrefQuery>,
) -> Result<Json<ApiResponse<ClientPreferencesResponse>>, AppError> {
    let keys_filter: Option<Vec<String>> = query.keys.map(|k| {
        k.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });

    let key_refs: Option<Vec<&str>> = keys_filter
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect());

    let prefs = state
        .client_pref_service
        .get_preferences(key_refs.as_deref())
        .await?;
    Ok(Json(ApiResponse::ok(prefs)))
}

// ---------------------------------------------------------------------------
// PUT /api/settings/client
// ---------------------------------------------------------------------------

async fn update_client_preferences(
    State(state): State<SystemRouterState>,
    body: Result<Json<UpdateClientPreferencesRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    state.client_pref_service.update_preferences(req).await?;
    Ok(Json(ApiResponse::success()))
}
