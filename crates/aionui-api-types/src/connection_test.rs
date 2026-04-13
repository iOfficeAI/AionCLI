use serde::{Deserialize, Serialize};

use super::provider::BedrockConfig;

/// Request body for `POST /api/bedrock/test-connection`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestBedrockConnectionRequest {
    pub bedrock_config: BedrockConfig,
}

/// Query parameters for `GET /api/gemini/subscription-status`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct GeminiSubscriptionQuery {
    pub proxy: Option<String>,
}

/// Response data for `GET /api/gemini/subscription-status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiSubscriptionData {
    pub subscription_status: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- TestBedrockConnectionRequest --

    #[test]
    fn test_bedrock_request_access_key() {
        let raw = json!({
            "bedrockConfig": {
                "authMethod": "accessKey",
                "region": "us-east-1",
                "accessKeyId": "AKIAIOSFODNN7",
                "secretAccessKey": "wJalrXUtnFEMI"
            }
        });
        let req: TestBedrockConnectionRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(
            req.bedrock_config.auth_method,
            crate::BedrockAuthMethod::AccessKey
        );
        assert_eq!(req.bedrock_config.region, "us-east-1");
        assert_eq!(
            req.bedrock_config.access_key_id.as_deref(),
            Some("AKIAIOSFODNN7")
        );
    }

    #[test]
    fn test_bedrock_request_profile() {
        let raw = json!({
            "bedrockConfig": {
                "authMethod": "profile",
                "region": "eu-west-1",
                "profile": "my-profile"
            }
        });
        let req: TestBedrockConnectionRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(
            req.bedrock_config.auth_method,
            crate::BedrockAuthMethod::Profile
        );
        assert_eq!(req.bedrock_config.profile.as_deref(), Some("my-profile"));
    }

    #[test]
    fn test_bedrock_request_missing_config() {
        let raw = json!({});
        let result = serde_json::from_value::<TestBedrockConnectionRequest>(raw);
        assert!(result.is_err());
    }

    #[test]
    fn test_bedrock_request_missing_region() {
        let raw = json!({
            "bedrockConfig": {
                "authMethod": "accessKey",
                "accessKeyId": "AKIA...",
                "secretAccessKey": "secret"
            }
        });
        let result = serde_json::from_value::<TestBedrockConnectionRequest>(raw);
        assert!(result.is_err());
    }

    // -- GeminiSubscriptionQuery --

    #[test]
    fn test_gemini_query_with_proxy() {
        let raw = json!({ "proxy": "http://proxy.example.com:8080" });
        let query: GeminiSubscriptionQuery = serde_json::from_value(raw).unwrap();
        assert_eq!(
            query.proxy.as_deref(),
            Some("http://proxy.example.com:8080")
        );
    }

    #[test]
    fn test_gemini_query_without_proxy() {
        let raw = json!({});
        let query: GeminiSubscriptionQuery = serde_json::from_value(raw).unwrap();
        assert!(query.proxy.is_none());
    }

    // -- GeminiSubscriptionData --

    #[test]
    fn test_gemini_subscription_data_active() {
        let data = GeminiSubscriptionData {
            subscription_status: "active".into(),
        };
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(json["subscriptionStatus"], "active");
    }

    #[test]
    fn test_gemini_subscription_data_roundtrip() {
        let raw = json!({ "subscriptionStatus": "inactive" });
        let data: GeminiSubscriptionData = serde_json::from_value(raw).unwrap();
        assert_eq!(data.subscription_status, "inactive");
    }
}
