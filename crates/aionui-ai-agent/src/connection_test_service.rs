use std::time::Duration;

use aionui_api_types::{BedrockAuthMethod, BedrockConfig, GeminiSubscriptionData};
use aionui_common::AppError;
use aws_sdk_bedrock::config::Credentials;
use tracing::{info, warn};

/// Default Bedrock model for lightweight connection testing.
const DEFAULT_BEDROCK_TEST_MODEL: &str = "anthropic.claude-sonnet-4-5-20250929-v1:0";

/// Timeout for Bedrock connection test.
const BEDROCK_TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout for Gemini subscription status check.
const GEMINI_STATUS_TIMEOUT: Duration = Duration::from_secs(10);

/// Base URL for Gemini generative AI API.
const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com";

/// Service for external connection testing (Bedrock credentials, Gemini subscription).
#[derive(Clone)]
pub struct ConnectionTestService {
    http_client: reqwest::Client,
}

impl ConnectionTestService {
    pub fn new(http_client: reqwest::Client) -> Self {
        Self { http_client }
    }

    /// Test AWS Bedrock credentials by performing a lightweight API call.
    ///
    /// Constructs an isolated credential provider (no global env pollution)
    /// and calls `get_foundation_model` as a zero-cost validation.
    pub async fn test_bedrock_connection(&self, config: BedrockConfig) -> Result<(), AppError> {
        validate_bedrock_config(&config)?;

        let aws_config = build_aws_config(&config).await;
        let bedrock_config = aws_sdk_bedrock::config::Builder::from(&aws_config)
            .timeout_config(
                aws_config::timeout::TimeoutConfig::builder()
                    .operation_timeout(BEDROCK_TEST_TIMEOUT)
                    .build(),
            )
            .build();
        let client = aws_sdk_bedrock::Client::from_conf(bedrock_config);

        client
            .get_foundation_model()
            .model_identifier(DEFAULT_BEDROCK_TEST_MODEL)
            .send()
            .await
            .map_err(|e| {
                warn!(error = %e, "Bedrock connection test failed");
                AppError::BadRequest(format!("Bedrock credentials invalid: {e}"))
            })?;

        info!("Bedrock connection test passed");
        Ok(())
    }

    /// Query Gemini subscription status via the generative AI API.
    ///
    /// Uses a lightweight model listing request to verify API access.
    /// The caller provides the API key (typically from `GEMINI_API_KEY` env var).
    /// An optional HTTP proxy can be specified.
    pub async fn get_gemini_subscription_status(
        &self,
        api_key: &str,
        proxy: Option<&str>,
    ) -> Result<GeminiSubscriptionData, AppError> {
        let client = match proxy {
            Some(proxy_url) => {
                let proxy = reqwest::Proxy::all(proxy_url).map_err(|e| {
                    AppError::BadRequest(format!("Invalid proxy URL: {e}"))
                })?;
                reqwest::Client::builder()
                    .proxy(proxy)
                    .timeout(GEMINI_STATUS_TIMEOUT)
                    .build()
                    .map_err(|e| AppError::Internal(format!("Failed to build HTTP client: {e}")))?
            }
            None => self.http_client.clone(),
        };

        let url = format!("{GEMINI_API_BASE}/v1beta/models?key={api_key}&pageSize=1");

        let resp = client.get(&url).timeout(GEMINI_STATUS_TIMEOUT).send().await.map_err(|e| {
            warn!(error = %e, "Gemini subscription check failed");
            AppError::BadGateway(format!("Gemini service unreachable: {e}"))
        })?;

        let status = resp.status();
        if status.is_success() {
            info!("Gemini subscription status: active");
            Ok(GeminiSubscriptionData {
                subscription_status: "active".into(),
            })
        } else if status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN
        {
            Ok(GeminiSubscriptionData {
                subscription_status: "inactive".into(),
            })
        } else {
            let body = resp.text().await.unwrap_or_default();
            warn!(status = %status, body = %body, "Gemini subscription check returned error");
            Err(AppError::BadGateway(format!(
                "Gemini API returned status {status}"
            )))
        }
    }
}

/// Validate required fields in BedrockConfig based on auth method.
fn validate_bedrock_config(config: &BedrockConfig) -> Result<(), AppError> {
    if config.region.is_empty() {
        return Err(AppError::BadRequest("region is required".into()));
    }

    match config.auth_method {
        BedrockAuthMethod::AccessKey => {
            if config.access_key_id.as_deref().unwrap_or("").is_empty() {
                return Err(AppError::BadRequest(
                    "accessKeyId is required for accessKey auth method".into(),
                ));
            }
            if config.secret_access_key.as_deref().unwrap_or("").is_empty() {
                return Err(AppError::BadRequest(
                    "secretAccessKey is required for accessKey auth method".into(),
                ));
            }
        }
        BedrockAuthMethod::Profile => {
            if config.profile.as_deref().unwrap_or("").is_empty() {
                return Err(AppError::BadRequest(
                    "profile is required for profile auth method".into(),
                ));
            }
        }
    }

    Ok(())
}

/// Build AWS SDK config from BedrockConfig without polluting global environment.
async fn build_aws_config(config: &BedrockConfig) -> aws_config::SdkConfig {
    let region = aws_config::Region::new(config.region.clone());

    match config.auth_method {
        BedrockAuthMethod::AccessKey => {
            let credentials = Credentials::new(
                config.access_key_id.as_deref().unwrap_or_default(),
                config.secret_access_key.as_deref().unwrap_or_default(),
                None,
                None,
                "aionui-bedrock-test",
            );
            aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(region)
                .credentials_provider(credentials)
                .load()
                .await
        }
        BedrockAuthMethod::Profile => {
            aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(region)
                .profile_name(config.profile.as_deref().unwrap_or_default())
                .load()
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_api_types::BedrockConfig;

    // -- validate_bedrock_config --

    #[test]
    fn test_validate_access_key_ok() {
        let config = BedrockConfig {
            auth_method: BedrockAuthMethod::AccessKey,
            region: "us-east-1".into(),
            access_key_id: Some("AKIAIOSFODNN7".into()),
            secret_access_key: Some("wJalrXUtnFEMI".into()),
            profile: None,
        };
        assert!(validate_bedrock_config(&config).is_ok());
    }

    #[test]
    fn test_validate_profile_ok() {
        let config = BedrockConfig {
            auth_method: BedrockAuthMethod::Profile,
            region: "eu-west-1".into(),
            access_key_id: None,
            secret_access_key: None,
            profile: Some("my-profile".into()),
        };
        assert!(validate_bedrock_config(&config).is_ok());
    }

    #[test]
    fn test_validate_empty_region() {
        let config = BedrockConfig {
            auth_method: BedrockAuthMethod::AccessKey,
            region: "".into(),
            access_key_id: Some("AKIA".into()),
            secret_access_key: Some("secret".into()),
            profile: None,
        };
        let err = validate_bedrock_config(&config).unwrap_err();
        assert!(err.to_string().contains("region"));
    }

    #[test]
    fn test_validate_access_key_missing_key_id() {
        let config = BedrockConfig {
            auth_method: BedrockAuthMethod::AccessKey,
            region: "us-east-1".into(),
            access_key_id: None,
            secret_access_key: Some("secret".into()),
            profile: None,
        };
        let err = validate_bedrock_config(&config).unwrap_err();
        assert!(err.to_string().contains("accessKeyId"));
    }

    #[test]
    fn test_validate_access_key_missing_secret() {
        let config = BedrockConfig {
            auth_method: BedrockAuthMethod::AccessKey,
            region: "us-east-1".into(),
            access_key_id: Some("AKIA".into()),
            secret_access_key: None,
            profile: None,
        };
        let err = validate_bedrock_config(&config).unwrap_err();
        assert!(err.to_string().contains("secretAccessKey"));
    }

    #[test]
    fn test_validate_access_key_empty_key_id() {
        let config = BedrockConfig {
            auth_method: BedrockAuthMethod::AccessKey,
            region: "us-east-1".into(),
            access_key_id: Some("".into()),
            secret_access_key: Some("secret".into()),
            profile: None,
        };
        let err = validate_bedrock_config(&config).unwrap_err();
        assert!(err.to_string().contains("accessKeyId"));
    }

    #[test]
    fn test_validate_profile_missing() {
        let config = BedrockConfig {
            auth_method: BedrockAuthMethod::Profile,
            region: "us-east-1".into(),
            access_key_id: None,
            secret_access_key: None,
            profile: None,
        };
        let err = validate_bedrock_config(&config).unwrap_err();
        assert!(err.to_string().contains("profile"));
    }

    #[test]
    fn test_validate_profile_empty() {
        let config = BedrockConfig {
            auth_method: BedrockAuthMethod::Profile,
            region: "us-east-1".into(),
            access_key_id: None,
            secret_access_key: None,
            profile: Some("".into()),
        };
        let err = validate_bedrock_config(&config).unwrap_err();
        assert!(err.to_string().contains("profile"));
    }

    #[test]
    fn test_default_bedrock_test_model() {
        assert!(DEFAULT_BEDROCK_TEST_MODEL.starts_with("anthropic.claude"));
    }

    // -- ConnectionTestService construction --

    #[test]
    fn test_service_construction() {
        let client = reqwest::Client::new();
        let _service = ConnectionTestService::new(client);
    }
}
