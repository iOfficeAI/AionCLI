//! HTTP request access-log layer.

use axum::Router;
use tower_http::trace::TraceLayer;

pub(super) fn with_access_log(router: Router) -> Router {
    router.layer(
        TraceLayer::new_for_http()
            .make_span_with(|req: &axum::http::Request<_>| {
                tracing::info_span!(
                    "http",
                    method = %req.method(),
                    path = %req.uri().path(),
                )
            })
            .on_response(
                |res: &axum::http::Response<_>, latency: std::time::Duration, _span: &tracing::Span| {
                    let status = res.status().as_u16();
                    let latency_ms = latency.as_millis() as u64;
                    if status >= 500 {
                        tracing::error!(status, latency_ms, "response");
                    } else if status >= 400 {
                        tracing::warn!(status, latency_ms, "response");
                    } else {
                        tracing::info!(status, latency_ms, "response");
                    }
                },
            )
            .on_failure(
                |error: tower_http::classify::ServerErrorsFailureClass,
                 latency: std::time::Duration,
                 _span: &tracing::Span| {
                    tracing::error!(
                        %error,
                        latency_ms = latency.as_millis() as u64,
                        "request failed"
                    );
                },
            ),
    )
}
