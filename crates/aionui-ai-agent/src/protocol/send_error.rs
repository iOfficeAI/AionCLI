use aionui_api_types::{AgentErrorCode, AgentErrorOwnership, AgentStreamErrorData};
use aionui_common::AppError;

use super::error::AcpError;

const MAX_DETAIL_CHARS: usize = 1000;

#[derive(Debug, Clone)]
pub struct AgentSendError {
    stream_error: AgentStreamErrorData,
}

impl AgentSendError {
    pub fn new(
        message: impl Into<String>,
        code: AgentErrorCode,
        ownership: AgentErrorOwnership,
        detail: Option<String>,
        retryable: bool,
        feedback_recommended: bool,
    ) -> Self {
        Self {
            stream_error: AgentStreamErrorData::classified(
                message,
                code,
                ownership,
                detail.map(|d| sanitize_error_detail(&d)),
                retryable,
                feedback_recommended,
            ),
        }
    }

    pub fn from_app_error(err: AppError) -> Self {
        Self::from_app_error_ref(&err)
    }

    pub fn from_app_error_ref(err: &AppError) -> Self {
        let detail = strip_error_prefix(&err.to_string());
        match err {
            AppError::Internal(_) => Self::new(
                "AionUI failed while sending the message",
                AgentErrorCode::AionuiInternalError,
                AgentErrorOwnership::Aionui,
                Some(detail),
                true,
                true,
            ),
            AppError::Forbidden(_) => Self::new(
                "AionUI blocked the request before it reached the Agent",
                AgentErrorCode::AionuiPermissionError,
                AgentErrorOwnership::Aionui,
                Some(detail),
                false,
                true,
            ),
            AppError::Unauthorized(_) => Self::new(
                "The selected Agent requires authentication",
                AgentErrorCode::UserAgentAuthRequired,
                AgentErrorOwnership::UserAgent,
                Some(detail),
                false,
                false,
            ),
            AppError::NotFound(msg) if msg.starts_with("Session not found") => Self::new(
                "The Agent session was not found",
                AgentErrorCode::UserAgentSessionNotFound,
                AgentErrorOwnership::UserAgent,
                Some(detail),
                true,
                false,
            ),
            AppError::BadRequest(msg) if msg.contains("Method not supported") => Self::new(
                "The selected Agent does not support this operation",
                AgentErrorCode::UserAgentUnsupportedMethod,
                AgentErrorOwnership::UserAgent,
                Some(detail),
                false,
                false,
            ),
            AppError::BadRequest(msg) if msg.contains("Invalid parameters") => Self::new(
                "The selected Agent rejected the request parameters",
                AgentErrorCode::UserAgentInvalidParams,
                AgentErrorOwnership::UserAgent,
                Some(detail),
                false,
                false,
            ),
            AppError::Timeout(_) => Self::new(
                "The model provider did not respond in time",
                AgentErrorCode::UserLlmProviderTimeout,
                AgentErrorOwnership::UserLlmProvider,
                Some(detail),
                true,
                false,
            ),
            AppError::RateLimited => Self::new(
                "The model provider rate limited the request",
                AgentErrorCode::UserLlmProviderRateLimited,
                AgentErrorOwnership::UserLlmProvider,
                Some(detail),
                true,
                false,
            ),
            AppError::BadGateway(_) => classify_upstream_detail(&detail),
            _ => Self::new(
                "The upstream Agent failed while handling the request",
                AgentErrorCode::UnknownUpstreamError,
                AgentErrorOwnership::UnknownUpstream,
                Some(detail),
                true,
                true,
            ),
        }
    }

    pub fn stream_error(&self) -> &AgentStreamErrorData {
        &self.stream_error
    }

    pub fn into_stream_error(self) -> AgentStreamErrorData {
        self.stream_error
    }

    pub fn code(&self) -> Option<AgentErrorCode> {
        self.stream_error.code
    }

    pub fn ownership(&self) -> Option<AgentErrorOwnership> {
        self.stream_error.ownership
    }
}

impl std::fmt::Display for AgentSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.stream_error.message)
    }
}

impl std::error::Error for AgentSendError {}

impl From<AppError> for AgentSendError {
    fn from(err: AppError) -> Self {
        Self::from_app_error(err)
    }
}

impl From<AcpError> for AgentSendError {
    fn from(err: AcpError) -> Self {
        let detail = err.to_string();
        match &err {
            AcpError::SpawnFailed { .. } => Self::new(
                "The selected Agent executable could not be started",
                AgentErrorCode::UserAgentNotInstalled,
                AgentErrorOwnership::UserAgent,
                Some(detail),
                false,
                false,
            ),
            AcpError::StartupCrash { .. } | AcpError::InitTimeout { .. } => Self::new(
                "The selected Agent failed to start",
                AgentErrorCode::UserAgentStartupFailed,
                AgentErrorOwnership::UserAgent,
                Some(detail),
                true,
                false,
            ),
            AcpError::Disconnected { .. } => Self::new(
                "The selected Agent disconnected",
                AgentErrorCode::UserAgentDisconnected,
                AgentErrorOwnership::UserAgent,
                Some(detail),
                true,
                false,
            ),
            AcpError::AuthRequired => Self::new(
                "The selected Agent requires authentication",
                AgentErrorCode::UserAgentAuthRequired,
                AgentErrorOwnership::UserAgent,
                Some(detail),
                false,
                false,
            ),
            AcpError::SessionNotFound { .. } => Self::new(
                "The Agent session was not found",
                AgentErrorCode::UserAgentSessionNotFound,
                AgentErrorOwnership::UserAgent,
                Some(detail),
                true,
                false,
            ),
            AcpError::MethodNotFound { .. } => Self::new(
                "The selected Agent does not support this operation",
                AgentErrorCode::UserAgentUnsupportedMethod,
                AgentErrorOwnership::UserAgent,
                Some(detail),
                false,
                false,
            ),
            AcpError::InvalidParams { .. } => Self::new(
                "The selected Agent rejected the request parameters",
                AgentErrorCode::UserAgentInvalidParams,
                AgentErrorOwnership::UserAgent,
                Some(detail),
                false,
                false,
            ),
            AcpError::NotConnected => Self::new(
                "AionUI lost its Agent protocol connection",
                AgentErrorCode::AionuiInternalError,
                AgentErrorOwnership::Aionui,
                Some(detail),
                true,
                true,
            ),
            AcpError::AgentInternal { .. } => classify_upstream_detail(&detail),
        }
    }
}

fn classify_upstream_detail(detail: &str) -> AgentSendError {
    let lower = detail.to_ascii_lowercase();
    let (message, code, retryable) = if contains_any(
        &lower,
        &[
            "signable request",
            "canonical request",
            "signature",
            "credential",
            "credentials",
            "access key",
            "secret key",
            "base url",
            "base_url",
        ],
    ) {
        (
            "The model provider configuration is invalid",
            AgentErrorCode::UserLlmProviderConfigError,
            false,
        )
    } else if contains_any(
        &lower,
        &[
            "401",
            "403",
            "unauthorized",
            "forbidden",
            "invalid api key",
            "invalid_api_key",
        ],
    ) {
        (
            "The model provider rejected the request",
            AgentErrorCode::UserLlmProviderAuthFailed,
            false,
        )
    } else if contains_any(
        &lower,
        &[
            "model not found",
            "model does not exist",
            "unknown model",
            "invalid model",
            "model_not_found",
        ],
    ) {
        (
            "The configured model was not found by the provider",
            AgentErrorCode::UserLlmProviderModelNotFound,
            false,
        )
    } else if contains_any(
        &lower,
        &["429", "rate limit", "rate_limit", "quota", "insufficient balance"],
    ) {
        (
            "The model provider rate limited the request",
            AgentErrorCode::UserLlmProviderRateLimited,
            true,
        )
    } else if contains_any(&lower, &["504", "timeout", "deadline exceeded", "gateway timeout"]) {
        (
            "The model provider did not respond in time",
            AgentErrorCode::UserLlmProviderTimeout,
            true,
        )
    } else if contains_any(
        &lower,
        &[
            "dns",
            "connection refused",
            "connection reset",
            "tls",
            "certificate",
            "connection error",
            "connect error",
        ],
    ) {
        (
            "The model provider could not be reached",
            AgentErrorCode::UserLlmProviderNetworkError,
            true,
        )
    } else if contains_any(&lower, &["500", "502", "503", "bad gateway", "service unavailable"]) {
        (
            "The model provider returned a server error",
            AgentErrorCode::UserLlmProviderGatewayError,
            true,
        )
    } else if contains_any(&lower, &["provider error"]) {
        (
            "The model provider returned an error",
            AgentErrorCode::UserLlmProviderGatewayError,
            true,
        )
    } else {
        (
            "The upstream Agent failed while handling the request",
            AgentErrorCode::UnknownUpstreamError,
            true,
        )
    };

    let ownership = if code == AgentErrorCode::UnknownUpstreamError {
        AgentErrorOwnership::UnknownUpstream
    } else {
        AgentErrorOwnership::UserLlmProvider
    };
    let feedback_recommended = ownership != AgentErrorOwnership::UserLlmProvider;

    AgentSendError::new(
        message,
        code,
        ownership,
        Some(detail.to_owned()),
        retryable,
        feedback_recommended,
    )
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn strip_error_prefix(message: &str) -> String {
    message
        .split_once(": ")
        .map(|(_, rest)| rest)
        .unwrap_or(message)
        .to_owned()
}

pub(crate) fn sanitize_error_detail(input: &str) -> String {
    let without_query = redact_url_queries(input);
    let mut out = String::new();
    for line in without_query.lines() {
        if is_sensitive_header_line(line) {
            push_bounded_line(&mut out, "<redacted header>");
        } else {
            push_bounded_line(&mut out, &redact_secret_words(line));
        }
        if out.chars().count() >= MAX_DETAIL_CHARS {
            break;
        }
    }
    truncate_chars(out.trim(), MAX_DETAIL_CHARS)
}

fn push_bounded_line(out: &mut String, line: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(line);
}

fn is_sensitive_header_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("authorization:")
        || lower.contains("x-api-key:")
        || lower.contains("api-key:")
        || lower.contains("api_key:")
}

fn redact_secret_words(line: &str) -> String {
    line.split_whitespace()
        .map(|word| {
            let lower = word.to_ascii_lowercase();
            if lower.starts_with("bearer ")
                || lower.starts_with("sk-")
                || lower.contains("api_key=")
                || lower.contains("apikey=")
                || lower.contains("access_token=")
                || lower.contains("token=")
            {
                "<redacted>"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_url_queries(input: &str) -> String {
    input
        .split_whitespace()
        .map(|word| {
            if (word.starts_with("http://") || word.starts_with("https://")) && word.contains('?') {
                let end_punct = word
                    .chars()
                    .last()
                    .filter(|c| matches!(c, '.' | ',' | ';' | ')' | ']'))
                    .map(|c| c.to_string())
                    .unwrap_or_default();
                let trimmed = word.trim_end_matches(['.', ',', ';', ')', ']']);
                let base = trimmed.split_once('?').map(|(base, _)| base).unwrap_or(trimmed);
                format!("{base}?<redacted>{end_punct}")
            } else {
                word.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_chars(value: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in value.chars().take(max) {
        out.push(ch);
    }
    if value.chars().count() > max {
        out.push_str("...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_provider_auth_failure() {
        let err = AgentSendError::from_app_error(AppError::BadGateway("provider returned 401 invalid api key".into()));

        assert_eq!(err.code(), Some(AgentErrorCode::UserLlmProviderAuthFailed));
        assert_eq!(err.ownership(), Some(AgentErrorOwnership::UserLlmProvider));
        assert_eq!(err.stream_error().feedback_recommended, Some(false));
    }

    #[test]
    fn classifies_unknown_upstream_when_heuristics_do_not_match() {
        let err = AgentSendError::from_app_error(AppError::BadGateway("agent exploded".into()));

        assert_eq!(err.code(), Some(AgentErrorCode::UnknownUpstreamError));
        assert_eq!(err.ownership(), Some(AgentErrorOwnership::UnknownUpstream));
        assert_eq!(err.stream_error().feedback_recommended, Some(true));
    }

    #[test]
    fn classifies_provider_error_without_specific_signal_as_provider_gateway() {
        let err = AgentSendError::from_app_error(AppError::BadGateway("Provider error: upstream failed".into()));

        assert_eq!(err.code(), Some(AgentErrorCode::UserLlmProviderGatewayError));
        assert_eq!(err.ownership(), Some(AgentErrorOwnership::UserLlmProvider));
        assert_eq!(err.stream_error().feedback_recommended, Some(false));
    }

    #[test]
    fn classifies_provider_config_errors_as_not_retryable() {
        let err = AgentSendError::from_app_error(AppError::BadGateway(
            "Provider error: Connection error: Signable request error: failed to create canonical request".into(),
        ));

        assert_eq!(err.code(), Some(AgentErrorCode::UserLlmProviderConfigError));
        assert_eq!(err.ownership(), Some(AgentErrorOwnership::UserLlmProvider));
        assert_eq!(err.stream_error().retryable, Some(false));
        assert_eq!(err.stream_error().feedback_recommended, Some(false));
    }

    #[test]
    fn sanitizes_secrets_and_query_strings() {
        let detail = sanitize_error_detail(
            "Authorization: Bearer sk-secret\nGET https://example.com/v1?api_key=sk-secret\ninvalid_api_key sk-secret",
        );

        assert!(!detail.contains("sk-secret"));
        assert!(!detail.contains("api_key=sk"));
        assert!(detail.contains("<redacted header>"));
        assert_eq!(
            redact_url_queries("GET https://example.com/v1?api_key=sk-secret"),
            "GET https://example.com/v1?<redacted>"
        );
    }
}
