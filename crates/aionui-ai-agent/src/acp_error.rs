use agent_client_protocol::{Error as SdkError, ErrorCode};
use aionui_common::AppError;

/// ACP-specific error type for protocol and process lifecycle errors.
///
/// This error is internal to the `aionui-ai-agent` crate. External callers
/// see it only after conversion to [`AppError`] via the `From` impl.
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    // ── Process lifecycle ──────────────────────────────────────────
    /// CLI binary not found or not executable.
    #[error("Failed to spawn agent process: {message}")]
    SpawnFailed { message: String },

    /// Process exited before the initialize handshake completed.
    #[error("Agent process exited during startup (exit={exit_code:?}, signal={signal:?})")]
    StartupCrash {
        exit_code: Option<i32>,
        signal: Option<String>,
        stderr: String,
    },

    /// Process crashed while a request was in flight.
    #[error("Agent process disconnected (exit={exit_code:?}, signal={signal:?})")]
    Disconnected {
        exit_code: Option<i32>,
        signal: Option<String>,
        stderr: String,
    },

    // ── ACP protocol errors (from SDK ErrorCode) ──────────────────
    /// Agent requires authentication first.
    #[error("Authentication required")]
    AuthRequired,

    /// Agent-side session not found.
    #[error("Session not found: {session_id}")]
    SessionNotFound { session_id: String },

    /// Agent does not support the requested method.
    #[error("Method not supported: {method}")]
    MethodNotFound { method: String },

    /// Invalid request parameters.
    #[error("Invalid parameters: {message}")]
    InvalidParams { message: String },

    /// Agent reported an internal error.
    #[error("Agent internal error: {message}")]
    AgentInternal { message: String, code: i32 },

    // ── Local errors ──────────────────────────────────────────────
    /// Protocol not connected (used before connect or after disconnect).
    #[error("ACP protocol not connected")]
    NotConnected,

    /// Initialize handshake timed out.
    #[error("Initialize handshake timed out after {timeout_secs}s")]
    InitTimeout { timeout_secs: u64 },
}

impl AcpError {
    /// Whether the caller may retry the operation.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AcpError::SpawnFailed { .. }
                | AcpError::StartupCrash { .. }
                | AcpError::Disconnected { .. }
                | AcpError::AgentInternal { .. }
                | AcpError::InitTimeout { .. }
        )
    }

    /// Convert an SDK [`Error`](SdkError) into an [`AcpError`].
    ///
    /// Mapping is by [`ErrorCode`], never by message text.
    /// `context` carries the session ID or method name for diagnostics.
    pub fn from_sdk(err: SdkError, context: &str) -> Self {
        match err.code {
            ErrorCode::AuthRequired => AcpError::AuthRequired,
            ErrorCode::ResourceNotFound => AcpError::SessionNotFound {
                session_id: context.to_owned(),
            },
            ErrorCode::MethodNotFound => AcpError::MethodNotFound {
                method: context.to_owned(),
            },
            ErrorCode::InvalidParams => AcpError::InvalidParams {
                message: err.message,
            },
            ErrorCode::ParseError | ErrorCode::InvalidRequest | ErrorCode::InternalError => {
                AcpError::AgentInternal {
                    message: err.message,
                    code: i32::from(err.code),
                }
            }
            _ => {
                let code = i32::from(err.code);
                // -32001, -32002: additional session-not-found codes used by some agents
                if code == -32001 || code == -32002 {
                    AcpError::SessionNotFound {
                        session_id: context.to_owned(),
                    }
                } else {
                    AcpError::AgentInternal {
                        message: err.message,
                        code,
                    }
                }
            }
        }
    }
}

/// Conversion from [`AcpError`] to [`AppError`] — the only way `AcpError`
/// leaves this crate.
///
/// **Security:** `StartupCrash` and `Disconnected` contain `stderr` which may
/// hold sensitive data. The `Display` impl (from `thiserror`) only includes
/// `exit_code` and `signal`. `stderr` is available for structured logging
/// (`tracing`) but never serialized into HTTP responses.
impl From<AcpError> for AppError {
    fn from(err: AcpError) -> Self {
        match &err {
            // Process lifecycle → 502 Bad Gateway (upstream failure)
            AcpError::SpawnFailed { .. }
            | AcpError::StartupCrash { .. }
            | AcpError::Disconnected { .. } => AppError::BadGateway(err.to_string()),

            // Authentication → 401
            AcpError::AuthRequired => {
                AppError::Unauthorized("Agent requires authentication".into())
            }

            // Session not found → 404
            AcpError::SessionNotFound { .. } => AppError::NotFound(err.to_string()),

            // Method not found → 400
            AcpError::MethodNotFound { .. } => AppError::BadRequest(err.to_string()),

            // Invalid parameters → 400
            AcpError::InvalidParams { .. } => AppError::BadRequest(err.to_string()),

            // Agent internal error → 502 (upstream failure)
            AcpError::AgentInternal { .. } => AppError::BadGateway(err.to_string()),

            // Not connected → 500 (our bug)
            AcpError::NotConnected => AppError::Internal("ACP protocol not connected".into()),

            // Init timeout → 502
            AcpError::InitTimeout { .. } => AppError::BadGateway(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn retryable_variants() {
        assert!(
            AcpError::SpawnFailed {
                message: "not found".into()
            }
            .is_retryable()
        );
        assert!(
            AcpError::StartupCrash {
                exit_code: Some(1),
                signal: None,
                stderr: String::new(),
            }
            .is_retryable()
        );
        assert!(
            AcpError::Disconnected {
                exit_code: None,
                signal: Some("SIGKILL".into()),
                stderr: String::new(),
            }
            .is_retryable()
        );
        assert!(
            AcpError::AgentInternal {
                message: "oops".into(),
                code: -32603,
            }
            .is_retryable()
        );
        assert!(AcpError::InitTimeout { timeout_secs: 30 }.is_retryable());
    }

    #[test]
    fn non_retryable_variants() {
        assert!(!AcpError::AuthRequired.is_retryable());
        assert!(
            !AcpError::SessionNotFound {
                session_id: "s1".into()
            }
            .is_retryable()
        );
        assert!(
            !AcpError::MethodNotFound {
                method: "foo".into()
            }
            .is_retryable()
        );
        assert!(
            !AcpError::InvalidParams {
                message: "bad".into()
            }
            .is_retryable()
        );
        assert!(!AcpError::NotConnected.is_retryable());
    }

    #[test]
    fn from_sdk_auth_required() {
        let sdk_err = SdkError::auth_required();
        let acp = AcpError::from_sdk(sdk_err, "sess-1");
        assert!(matches!(acp, AcpError::AuthRequired));
    }

    #[test]
    fn from_sdk_resource_not_found() {
        let sdk_err = SdkError::resource_not_found(None);
        let acp = AcpError::from_sdk(sdk_err, "sess-42");
        match acp {
            AcpError::SessionNotFound { session_id } => assert_eq!(session_id, "sess-42"),
            other => panic!("Expected SessionNotFound, got {other:?}"),
        }
    }

    #[test]
    fn from_sdk_method_not_found() {
        let sdk_err = SdkError::method_not_found();
        let acp = AcpError::from_sdk(sdk_err, "session/magic");
        match acp {
            AcpError::MethodNotFound { method } => assert_eq!(method, "session/magic"),
            other => panic!("Expected MethodNotFound, got {other:?}"),
        }
    }

    #[test]
    fn from_sdk_invalid_params() {
        let sdk_err = SdkError::invalid_params();
        let acp = AcpError::from_sdk(sdk_err, "ignored");
        assert!(matches!(acp, AcpError::InvalidParams { .. }));
    }

    #[test]
    fn from_sdk_internal_error() {
        let sdk_err = SdkError::internal_error();
        let acp = AcpError::from_sdk(sdk_err, "context");
        match acp {
            AcpError::AgentInternal { code, .. } => assert_eq!(code, -32603),
            other => panic!("Expected AgentInternal, got {other:?}"),
        }
    }

    #[test]
    fn from_sdk_other_code_session_related() {
        let sdk_err = SdkError::new(-32001, "session expired");
        let acp = AcpError::from_sdk(sdk_err, "sess-old");
        assert!(matches!(acp, AcpError::SessionNotFound { .. }));
    }

    #[test]
    fn from_sdk_other_code_unknown() {
        let sdk_err = SdkError::new(-32099, "custom error");
        let acp = AcpError::from_sdk(sdk_err, "ctx");
        match acp {
            AcpError::AgentInternal { code, message } => {
                assert_eq!(code, -32099);
                assert_eq!(message, "custom error");
            }
            other => panic!("Expected AgentInternal, got {other:?}"),
        }
    }

    #[test]
    fn to_app_error_status_codes() {
        let cases: Vec<(AcpError, StatusCode)> = vec![
            (
                AcpError::SpawnFailed {
                    message: "x".into(),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (AcpError::AuthRequired, StatusCode::UNAUTHORIZED),
            (
                AcpError::SessionNotFound {
                    session_id: "s".into(),
                },
                StatusCode::NOT_FOUND,
            ),
            (
                AcpError::MethodNotFound { method: "m".into() },
                StatusCode::BAD_REQUEST,
            ),
            (
                AcpError::InvalidParams {
                    message: "p".into(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                AcpError::AgentInternal {
                    message: "e".into(),
                    code: -1,
                },
                StatusCode::BAD_GATEWAY,
            ),
            (AcpError::NotConnected, StatusCode::INTERNAL_SERVER_ERROR),
            (
                AcpError::InitTimeout { timeout_secs: 30 },
                StatusCode::BAD_GATEWAY,
            ),
        ];

        for (acp_err, expected_status) in cases {
            let app_err: AppError = acp_err.into();
            assert_eq!(
                app_err.status_code(),
                expected_status,
                "Mismatch for {app_err:?}"
            );
        }
    }

    #[test]
    fn display_does_not_contain_stderr() {
        let err = AcpError::StartupCrash {
            exit_code: Some(1),
            signal: None,
            stderr: "SUPER SECRET API KEY abc123".into(),
        };
        let display = err.to_string();
        assert!(
            !display.contains("SUPER SECRET"),
            "Display should not leak stderr: {display}"
        );
    }
}
