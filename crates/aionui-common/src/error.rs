use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Application-level error with HTTP status code mapping.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Rate limited")]
    RateLimited,

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Bad gateway: {0}")]
    BadGateway(String),

    #[error("Request timeout: {0}")]
    Timeout(String),

    #[error("Unprocessable entity: {0}")]
    UnprocessableEntity(String),

    /// The conversation exists but is archived and cannot be operated on.
    /// Example: legacy Gemini runtime conversations after the runtime was
    /// removed — the row stays readable (list + history) but send_message /
    /// resume should 410 Gone with this code so the client renders a
    /// dedicated "this conversation is archived" UI instead of a generic
    /// bad-request banner.
    #[error("Conversation archived: {0}")]
    ConversationArchived(String),
}

/// Internal error response body matching the `ErrorResponse` format from `aionui-api-types`.
#[derive(Serialize)]
struct ErrorBody {
    success: bool,
    error: String,
    code: String,
}

impl AppError {
    /// HTTP status code for this error variant.
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::BadGateway(_) => StatusCode::BAD_GATEWAY,
            Self::Timeout(_) => StatusCode::BAD_GATEWAY,
            Self::UnprocessableEntity(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::ConversationArchived(_) => StatusCode::GONE,
        }
    }

    /// Machine-readable error code string.
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "NOT_FOUND",
            Self::BadRequest(_) => "BAD_REQUEST",
            Self::Unauthorized(_) => "UNAUTHORIZED",
            Self::Forbidden(message) => {
                if message.contains("outside the allowed sandbox") {
                    "PATH_OUTSIDE_SANDBOX"
                } else {
                    "FORBIDDEN"
                }
            }
            Self::Conflict(_) => "CONFLICT",
            Self::RateLimited => "RATE_LIMITED",
            Self::Internal(_) => "INTERNAL_ERROR",
            Self::BadGateway(_) => "BAD_GATEWAY",
            Self::Timeout(_) => "TIMEOUT",
            Self::UnprocessableEntity(_) => "UNPROCESSABLE_ENTITY",
            Self::ConversationArchived(_) => "CONVERSATION_ARCHIVED",
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = ErrorBody {
            success: false,
            error: self.to_string(),
            code: self.error_code().to_owned(),
        };
        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[test]
    fn test_status_codes() {
        assert_eq!(AppError::NotFound("x".into()).status_code(), StatusCode::NOT_FOUND);
        assert_eq!(AppError::BadRequest("x".into()).status_code(), StatusCode::BAD_REQUEST);
        assert_eq!(
            AppError::Unauthorized("x".into()).status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(AppError::Forbidden("x".into()).status_code(), StatusCode::FORBIDDEN);
        assert_eq!(AppError::Conflict("x".into()).status_code(), StatusCode::CONFLICT);
        assert_eq!(AppError::RateLimited.status_code(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            AppError::Internal("x".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(AppError::BadGateway("x".into()).status_code(), StatusCode::BAD_GATEWAY);
        assert_eq!(AppError::Timeout("x".into()).status_code(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            AppError::UnprocessableEntity("x".into()).status_code(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn test_error_codes() {
        assert_eq!(AppError::NotFound("x".into()).error_code(), "NOT_FOUND");
        assert_eq!(AppError::BadRequest("x".into()).error_code(), "BAD_REQUEST");
        assert_eq!(AppError::Unauthorized("x".into()).error_code(), "UNAUTHORIZED");
        assert_eq!(AppError::Forbidden("x".into()).error_code(), "FORBIDDEN");
        assert_eq!(
            AppError::Forbidden("path '/tmp/x' is outside the allowed sandbox".into()).error_code(),
            "PATH_OUTSIDE_SANDBOX"
        );
        assert_eq!(AppError::Conflict("x".into()).error_code(), "CONFLICT");
        assert_eq!(AppError::RateLimited.error_code(), "RATE_LIMITED");
        assert_eq!(AppError::Internal("x".into()).error_code(), "INTERNAL_ERROR");
        assert_eq!(AppError::BadGateway("x".into()).error_code(), "BAD_GATEWAY");
        assert_eq!(AppError::Timeout("x".into()).error_code(), "TIMEOUT");
        assert_eq!(
            AppError::UnprocessableEntity("x".into()).error_code(),
            "UNPROCESSABLE_ENTITY"
        );
    }

    #[test]
    fn test_error_display() {
        assert_eq!(AppError::NotFound("user 123".into()).to_string(), "Not found: user 123");
        assert_eq!(AppError::RateLimited.to_string(), "Rate limited");
    }

    #[test]
    fn test_into_response_status() {
        let resp = AppError::NotFound("test".into()).into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_into_response_body_format() {
        let resp = AppError::NotFound("user 42".into()).into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["success"], false);
        assert_eq!(json["error"], "Not found: user 42");
        assert_eq!(json["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn test_rate_limited_response_body() {
        let resp = AppError::RateLimited.into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["success"], false);
        assert_eq!(json["error"], "Rate limited");
        assert_eq!(json["code"], "RATE_LIMITED");
    }
}
