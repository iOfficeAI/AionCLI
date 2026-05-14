use agent_client_protocol::{Error as SdkError, ErrorCode};
use aionui_common::AppError;

/// ACP-specific error type for protocol and process lifecycle errors.
///
/// This error is internal to the `aionui-ai-agent` crate. External callers
/// see it only after conversion to [`AppError`] via the `From` impl.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // Variants constructed as error paths mature; kept for complete ACP error model.
pub(crate) enum AcpError {
    // ── Process lifecycle ──────────────────────────────────────────
    /// CLI binary not found or not executable.
    SpawnFailed { message: String },

    /// Process exited before the initialize handshake completed.
    StartupCrash {
        exit_code: Option<i32>,
        signal: Option<String>,
        stderr: String,
    },

    /// Process crashed while a request was in flight.
    Disconnected {
        exit_code: Option<i32>,
        signal: Option<String>,
        stderr: String,
    },

    // ── ACP protocol errors (from SDK ErrorCode) ──────────────────
    /// Agent requires authentication first.
    AuthRequired,

    /// Agent-side session not found.
    SessionNotFound { session_id: String },

    /// Agent does not support the requested method.
    MethodNotFound { method: String },

    /// Invalid request parameters.
    InvalidParams { message: String },

    /// Agent reported an internal error. `data` carries the optional JSON-RPC
    /// `error.data` payload from the agent — see the [`Display`] impl for how
    /// it is rendered.
    ///
    /// [`Display`]: std::fmt::Display
    AgentInternal {
        message: String,
        code: i32,
        data: Option<serde_json::Value>,
    },

    // ── Local errors ──────────────────────────────────────────────
    /// Protocol not connected (used before connect or after disconnect).
    NotConnected,

    /// Initialize handshake timed out.
    InitTimeout { timeout_secs: u64 },
}

/// JSON-RPC default message strings that carry no useful information.
/// When `AgentInternal` arrives with one of these as its `message`, we fall
/// back to a diagnostic display ("Agent internal error (code -32603)").
///
/// These strings are copied from `ErrorCode`'s `strum::Display` attributes in
/// `agent-client-protocol-schema`. If the SDK changes them, update this list
/// to avoid silently reverting to the diagnostic fallback.
const SDK_DEFAULT_MESSAGES: &[&str] = &[
    "Parse error",
    "Invalid request",
    "Method not found",
    "Invalid params",
    "Internal error",
];

impl std::fmt::Display for AcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcpError::SpawnFailed { message } => {
                write!(f, "Failed to spawn agent process: {message}")
            }
            AcpError::StartupCrash { exit_code, signal, .. } => {
                // stderr intentionally NOT included — may carry secrets.
                write!(
                    f,
                    "Agent process exited during startup (exit={exit_code:?}, signal={signal:?})"
                )
            }
            AcpError::Disconnected { exit_code, signal, .. } => {
                write!(
                    f,
                    "Agent process disconnected (exit={exit_code:?}, signal={signal:?})"
                )
            }
            AcpError::AuthRequired => f.write_str("Authentication required"),
            AcpError::SessionNotFound { session_id } => {
                write!(f, "Session not found: {session_id}")
            }
            AcpError::MethodNotFound { method } => {
                write!(f, "Method not supported: {method}")
            }
            AcpError::InvalidParams { message } => {
                write!(f, "Invalid parameters: {message}")
            }
            AcpError::AgentInternal { message, code, data } => {
                let trimmed = message.trim();
                let is_default = trimmed.is_empty()
                    || SDK_DEFAULT_MESSAGES.iter().any(|d| d.eq_ignore_ascii_case(trimmed));
                if is_default {
                    write!(f, "Agent internal error (code {code})")?;
                } else {
                    f.write_str(trimmed)?;
                }
                if let Some(data) = data {
                    // serde_json::to_string on a Value cannot actually fail;
                    // the fallback exists only because Display must be infallible.
                    let compact = serde_json::to_string(data)
                        .unwrap_or_else(|_| "<unserializable data>".to_owned());
                    write!(f, " ({compact})")?;
                }
                Ok(())
            }
            AcpError::NotConnected => f.write_str("ACP protocol not connected"),
            AcpError::InitTimeout { timeout_secs } => {
                write!(f, "Initialize handshake timed out after {timeout_secs}s")
            }
        }
    }
}

impl AcpError {
    /// Whether the caller may retry the operation.
    #[allow(dead_code)] // Will be used once retry logic is wired into the send path.
    pub(crate) fn is_retryable(&self) -> bool {
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
            ErrorCode::InvalidParams => AcpError::InvalidParams { message: err.message },
            ErrorCode::ParseError | ErrorCode::InvalidRequest | ErrorCode::InternalError => {
                AcpError::AgentInternal {
                    message: err.message,
                    code: i32::from(err.code),
                    data: err.data,
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
                        data: err.data,
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
/// hold sensitive data. The `Display` impl only includes
/// `exit_code` and `signal`. `stderr` is available for structured logging
/// (`tracing`) but never serialized into HTTP responses.
impl From<AcpError> for AppError {
    fn from(err: AcpError) -> Self {
        match &err {
            // Process lifecycle → 502 Bad Gateway (upstream failure)
            AcpError::SpawnFailed { .. } | AcpError::StartupCrash { .. } | AcpError::Disconnected { .. } => {
                AppError::BadGateway(err.to_string())
            }

            // Authentication → 401
            AcpError::AuthRequired => AppError::Unauthorized("Agent requires authentication".into()),

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
                data: None,
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
        assert!(!AcpError::MethodNotFound { method: "foo".into() }.is_retryable());
        assert!(!AcpError::InvalidParams { message: "bad".into() }.is_retryable());
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
            AcpError::AgentInternal { code, message, .. } => {
                assert_eq!(code, -32099);
                assert_eq!(message, "custom error");
            }
            other => panic!("Expected AgentInternal, got {other:?}"),
        }
    }

    #[test]
    fn to_app_error_status_codes() {
        let cases: Vec<(AcpError, StatusCode)> = vec![
            (AcpError::SpawnFailed { message: "x".into() }, StatusCode::BAD_GATEWAY),
            (AcpError::AuthRequired, StatusCode::UNAUTHORIZED),
            (
                AcpError::SessionNotFound { session_id: "s".into() },
                StatusCode::NOT_FOUND,
            ),
            (AcpError::MethodNotFound { method: "m".into() }, StatusCode::BAD_REQUEST),
            (AcpError::InvalidParams { message: "p".into() }, StatusCode::BAD_REQUEST),
            (
                AcpError::AgentInternal {
                    message: "e".into(),
                    code: -1,
                    data: None,
                },
                StatusCode::BAD_GATEWAY,
            ),
            (AcpError::NotConnected, StatusCode::INTERNAL_SERVER_ERROR),
            (AcpError::InitTimeout { timeout_secs: 30 }, StatusCode::BAD_GATEWAY),
        ];

        for (acp_err, expected_status) in cases {
            let app_err: AppError = acp_err.into();
            assert_eq!(app_err.status_code(), expected_status, "Mismatch for {app_err:?}");
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

    #[test]
    fn from_sdk_captures_data_payload() {
        let sdk_err = SdkError::internal_error()
            .data(serde_json::json!({"reason": "rate_limited", "retry_after": 30}));
        let acp = AcpError::from_sdk(sdk_err, "context");
        match acp {
            AcpError::AgentInternal { code, message, data } => {
                assert_eq!(code, -32603);
                assert_eq!(message, "Internal error");
                let data = data.expect("data must be preserved");
                assert_eq!(data["reason"], "rate_limited");
                assert_eq!(data["retry_after"], 30);
            }
            other => panic!("Expected AgentInternal, got {other:?}"),
        }
    }

    #[test]
    fn from_sdk_no_data_yields_none() {
        let sdk_err = SdkError::internal_error();
        let acp = AcpError::from_sdk(sdk_err, "context");
        match acp {
            AcpError::AgentInternal { data, .. } => assert!(data.is_none()),
            other => panic!("Expected AgentInternal, got {other:?}"),
        }
    }

    #[test]
    fn agent_internal_display_uses_message_only_when_no_data() {
        let err = AcpError::AgentInternal {
            message: "API Error: Internal server error".into(),
            code: -32603,
            data: None,
        };
        assert_eq!(
            err.to_string(),
            "API Error: Internal server error",
            "Display must NOT prefix with 'Agent internal error:' when message carries upstream context"
        );
    }

    #[test]
    fn agent_internal_display_falls_back_when_message_is_sdk_default() {
        // SDK default for ErrorCode::InternalError is the plain string "Internal error".
        // When that's all we have, the user sees nothing useful, so add a hint.
        let err = AcpError::AgentInternal {
            message: "Internal error".into(),
            code: -32603,
            data: None,
        };
        let display = err.to_string();
        assert!(
            display.contains("Agent internal error"),
            "Display must include 'Agent internal error' hint when SDK gave us its default message; got {display}"
        );
        assert!(
            display.contains("-32603"),
            "Display must include the JSON-RPC code as a diagnostic when message is empty/default; got {display}"
        );
    }

    #[test]
    fn agent_internal_display_appends_data_when_message_is_sdk_default() {
        // Real-world shape: SDK returned its default `"Internal error"` but
        // attached structured data. Display must use the diagnostic header
        // AND append the data.
        let err = AcpError::AgentInternal {
            message: "Internal error".into(),
            code: -32603,
            data: Some(serde_json::json!({"retry_after": 30})),
        };
        let display = err.to_string();
        assert!(
            display.contains("Agent internal error"),
            "header must use diagnostic fallback when message is the SDK default; got {display}"
        );
        assert!(
            display.contains("-32603"),
            "header must include the code; got {display}"
        );
        assert!(display.contains("retry_after"), "data must be appended; got {display}");
        assert!(display.contains("30"), "data value must be appended; got {display}");
        assert!(
            !display.contains('\n'),
            "data must be inline; got {display}"
        );
    }

    #[test]
    fn agent_internal_display_appends_data_inline() {
        let err = AcpError::AgentInternal {
            message: "API Error".into(),
            code: -32603,
            data: Some(serde_json::json!({"upstream_status": 503})),
        };
        let display = err.to_string();
        assert!(display.contains("API Error"), "got {display}");
        assert!(display.contains("upstream_status"), "got {display}");
        assert!(display.contains("503"), "got {display}");
        assert!(
            !display.contains('\n'),
            "data must be appended on a single line, not pretty-printed; got {display}"
        );
    }
}
