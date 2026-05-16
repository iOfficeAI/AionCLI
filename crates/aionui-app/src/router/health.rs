//! Health check + Guide MCP status diagnostic endpoints.

use aionui_api_types::GuideMcpConfig;
use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub(super) struct HealthResponse {
    status: &'static str,
    version: &'static str,
    build_time: &'static str,
}

pub(super) async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        build_time: env!("BUILD_TIME"),
    })
}

#[derive(Serialize)]
pub(super) struct GuideMcpStatusResponse {
    running: bool,
    port: Option<u16>,
    binary_path: Option<String>,
}

pub(super) async fn guide_mcp_status(
    axum::extract::State(cfg): axum::extract::State<Option<GuideMcpConfig>>,
) -> Json<GuideMcpStatusResponse> {
    Json(match cfg {
        Some(c) => GuideMcpStatusResponse {
            running: true,
            port: Some(c.port),
            binary_path: Some(c.binary_path),
        },
        None => GuideMcpStatusResponse {
            running: false,
            port: None,
            binary_path: None,
        },
    })
}
