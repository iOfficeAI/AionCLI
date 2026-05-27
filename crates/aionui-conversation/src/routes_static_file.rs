use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;

use aionui_auth::CurrentUser;
use aionui_common::AppError;
use aionui_static_file::{RequestContext, ServeError, StaticFileService, parse_range};

use crate::service::ConversationService;

/// Shared state for the static file route.
#[derive(Clone)]
pub struct StaticFileRouterState {
    pub conversation_service: ConversationService,
    pub static_file_service: Arc<StaticFileService>,
}

/// Build the static file router.
///
/// Route: `GET /api/conversations/{id}/files/{*path}`
pub fn conversation_static_file_routes(state: StaticFileRouterState) -> Router {
    Router::new()
        .route("/api/conversations/{id}/files/{*path}", get(serve_conversation_file))
        .with_state(state)
}

async fn serve_conversation_file(
    State(state): State<StaticFileRouterState>,
    Extension(user): Extension<CurrentUser>,
    headers: HeaderMap,
    Path((conversation_id, file_path)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let workspace = state
        .conversation_service
        .get_workspace(&user.id, &conversation_id)
        .await?;

    let context = RequestContext {
        user_id: Some(user.id.clone()),
        conversation_id: Some(conversation_id.clone()),
    };

    let served = state
        .static_file_service
        .resolve(&workspace, &file_path, &context)
        .await
        .map_err(map_serve_error)?;

    let etag = served.last_modified.map(|t| format!("\"{:x}-{:x}\"", t, served.size));

    // Check Range header for partial content support
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| parse_range(v, served.size));

    if let Some(range) = range {
        let file = state
            .static_file_service
            .open_range(&served, range.start)
            .await
            .map_err(map_serve_error)?;

        let stream = ReaderStream::with_capacity(file.take(range.len()), 64 * 1024);
        let body = Body::from_stream(stream);

        let mut builder = Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, &served.content_type)
            .header(header::CONTENT_LENGTH, range.len())
            .header(header::CONTENT_RANGE, range.content_range_header(served.size))
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CACHE_CONTROL, "public, max-age=60");

        if let Some(ref etag_val) = etag {
            builder = builder.header(header::ETAG, etag_val);
        }

        builder.body(body).map_err(|e| AppError::Internal(e.to_string()))
    } else {
        let file = state.static_file_service.open(&served).await.map_err(map_serve_error)?;

        let stream = ReaderStream::with_capacity(file, 64 * 1024);
        let body = Body::from_stream(stream);

        let mut builder = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, &served.content_type)
            .header(header::CONTENT_LENGTH, served.size)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CACHE_CONTROL, "public, max-age=60");

        if let Some(ref etag_val) = etag {
            builder = builder.header(header::ETAG, etag_val);
        }

        builder.body(body).map_err(|e| AppError::Internal(e.to_string()))
    }
}

fn map_serve_error(e: ServeError) -> AppError {
    match e {
        ServeError::Traversal(_) => AppError::Forbidden("Path traversal not allowed".into()),
        ServeError::Denied(d) => AppError::Forbidden(d.reason),
        ServeError::NotFound(p) => AppError::NotFound(format!("File not found: {p}")),
        ServeError::TooLarge { size, limit } => {
            AppError::BadRequest(format!("File too large: {size} bytes exceeds limit of {limit} bytes"))
        }
        ServeError::Io(io) => AppError::Internal(format!("IO error: {io}")),
    }
}
