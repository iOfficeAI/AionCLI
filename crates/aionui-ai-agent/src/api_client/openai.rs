use std::sync::Arc;

use super::client::{ApiClientError, RotatingClient};
use super::key_manager::ApiKeyManager;

/// OpenAI-compatible API client with multi-key rotation.
///
/// Sends requests to the OpenAI REST API (or any compatible endpoint)
/// using `Authorization: Bearer {key}` authentication.
pub struct OpenAIRotatingClient {
    inner: RotatingClient,
}

impl OpenAIRotatingClient {
    pub fn new(
        key_manager: Arc<ApiKeyManager>,
        base_url: &str,
        max_retries: Option<usize>,
        retry_delay_ms: Option<u64>,
    ) -> Self {
        Self {
            inner: RotatingClient::new(key_manager, base_url, max_retries, retry_delay_ms),
        }
    }

    pub fn key_manager(&self) -> &Arc<ApiKeyManager> {
        self.inner.key_manager()
    }

    pub fn base_url(&self) -> &str {
        self.inner.base_url()
    }

    /// POST /v1/chat/completions
    pub async fn create_chat_completion(
        &self,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiClientError> {
        self.inner
            .execute_with_retry(|client, base_url, api_key| {
                client
                    .post(format!("{base_url}/v1/chat/completions"))
                    .bearer_auth(api_key)
                    .json(request)
            })
            .await
    }

    /// POST /v1/images/generations
    pub async fn create_image(
        &self,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiClientError> {
        self.inner
            .execute_with_retry(|client, base_url, api_key| {
                client
                    .post(format!("{base_url}/v1/images/generations"))
                    .bearer_auth(api_key)
                    .json(request)
            })
            .await
    }

    /// POST /v1/embeddings
    pub async fn create_embedding(
        &self,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiClientError> {
        self.inner
            .execute_with_retry(|client, base_url, api_key| {
                client
                    .post(format!("{base_url}/v1/embeddings"))
                    .bearer_auth(api_key)
                    .json(request)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_with_correct_base_url() {
        let km = Arc::new(ApiKeyManager::new("sk-test", None));
        let client = OpenAIRotatingClient::new(km, "https://api.openai.com/v1", None, None);
        // normalize_base_url strips /v1
        assert_eq!(client.base_url(), "https://api.openai.com");
    }

    #[test]
    fn constructs_with_clean_url() {
        let km = Arc::new(ApiKeyManager::new("sk-test", None));
        let client = OpenAIRotatingClient::new(km, "https://api.openai.com", None, None);
        assert_eq!(client.base_url(), "https://api.openai.com");
    }
}
