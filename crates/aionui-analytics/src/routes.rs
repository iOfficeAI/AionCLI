use axum::Router;
use axum::extract::{Json, Query, State};
use axum::http::HeaderMap;
use axum::routing::get;

use aionui_api_types::{AgentUsageQuery, AgentUsageResponse, ApiResponse};
use aionui_common::AppError;

use crate::service::{AgentUsageService, UsageRequest};

/// WebHost 反向代理在转发 WebUI 远程请求时注入的标志 header。
/// 安全模型 (回应 review P1, 见 design.md 访问边界):
/// - 本机 Electron 直连后端, 不经 WebHost 代理, 不带此 header → is_remote=false → 不脱敏
/// - WebUI remote 必经 WebHost 代理, WebHost **先剥离客户端同名 header 再注入** → is_remote=true → 脱敏
/// - 即使公网客户端自带伪造 header, 经 WebHost 时会被剥离重置; 退一步即便到达,
///   也只会让该客户端看到**更脱敏**的结果 (安全方向), 不会泄露更多
pub const WEBUI_REMOTE_HEADER: &str = "x-aionui-webui-remote";

#[derive(Clone)]
pub struct AnalyticsRouterState {
    pub service: AgentUsageService,
}

pub fn analytics_routes(state: AnalyticsRouterState) -> Router {
    Router::new()
        .route("/api/analytics/agent-usage", get(get_agent_usage))
        .with_state(state)
}

async fn get_agent_usage(
    State(state): State<AnalyticsRouterState>,
    headers: HeaderMap,
    Query(q): Query<AgentUsageQuery>,
) -> Result<Json<ApiResponse<AgentUsageResponse>>, AppError> {
    let is_remote = headers
        .get(WEBUI_REMOTE_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "1")
        .unwrap_or(false);
    let resp = state
        .service
        .build(UsageRequest {
            trend_granularity: q.trend_granularity.unwrap_or_else(|| "day".into()),
            trend_dimension: q.trend_dimension.unwrap_or_else(|| "agent".into()),
            time_range: q.time_range.unwrap_or_else(|| "30d".into()),
            refresh: q.refresh.unwrap_or(false),
            sessions_limit: q.sessions_limit.unwrap_or(200),
            sessions_offset: q.sessions_offset.unwrap_or(0),
            is_remote,
        })
        .await?;
    Ok(Json(ApiResponse::ok(resp)))
}
