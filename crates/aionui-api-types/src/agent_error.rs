use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentErrorOwnership {
    Aionui,
    UserAgent,
    UserLlmProvider,
    UnknownUpstream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AgentErrorCode {
    AionuiStreamBroken,
    AionuiStateInconsistent,
    AionuiPermissionError,
    AionuiInternalError,
    UserAgentNotInstalled,
    UserAgentStartupFailed,
    UserAgentDisconnected,
    UserAgentAuthRequired,
    UserAgentSessionNotFound,
    UserAgentUnsupportedMethod,
    UserAgentInvalidParams,
    UserLlmProviderAuthFailed,
    UserLlmProviderModelNotFound,
    UserLlmProviderRateLimited,
    UserLlmProviderTimeout,
    UserLlmProviderNetworkError,
    UserLlmProviderGatewayError,
    UnknownUpstreamError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStreamErrorData {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<AgentErrorCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<AgentErrorOwnership>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback_recommended: Option<bool>,
}

impl AgentStreamErrorData {
    pub fn legacy(message: impl Into<String>, code: Option<AgentErrorCode>) -> Self {
        Self {
            message: message.into(),
            code,
            ownership: None,
            detail: None,
            retryable: None,
            feedback_recommended: None,
        }
    }

    pub fn classified(
        message: impl Into<String>,
        code: AgentErrorCode,
        ownership: AgentErrorOwnership,
        detail: Option<String>,
        retryable: bool,
        feedback_recommended: bool,
    ) -> Self {
        Self {
            message: message.into(),
            code: Some(code),
            ownership: Some(ownership),
            detail,
            retryable: Some(retryable),
            feedback_recommended: Some(feedback_recommended),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classified_error_serializes_as_public_contract() {
        let payload = AgentStreamErrorData::classified(
            "The model provider rejected the request",
            AgentErrorCode::UserLlmProviderAuthFailed,
            AgentErrorOwnership::UserLlmProvider,
            Some("Provider returned 401.".into()),
            false,
            false,
        );

        let json = serde_json::to_value(payload).unwrap();
        assert_eq!(json["message"], "The model provider rejected the request");
        assert_eq!(json["code"], "USER_LLM_PROVIDER_AUTH_FAILED");
        assert_eq!(json["ownership"], "user_llm_provider");
        assert_eq!(json["retryable"], false);
        assert_eq!(json["feedback_recommended"], false);
    }

    #[test]
    fn legacy_error_payload_deserializes() {
        let json = serde_json::json!({
            "message": "legacy failure",
            "code": "UNKNOWN_UPSTREAM_ERROR"
        });

        let payload: AgentStreamErrorData = serde_json::from_value(json).unwrap();
        assert_eq!(payload.message, "legacy failure");
        assert_eq!(payload.code, Some(AgentErrorCode::UnknownUpstreamError));
        assert_eq!(payload.ownership, None);
        assert_eq!(payload.retryable, None);
        assert_eq!(payload.feedback_recommended, None);
    }
}
