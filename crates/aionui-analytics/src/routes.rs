// Minimal placeholder — full impl in Task 1.9.
use axum::Router;

#[derive(Clone)]
pub struct AnalyticsRouterState;

pub fn analytics_routes(_state: AnalyticsRouterState) -> Router {
    Router::new()
}
