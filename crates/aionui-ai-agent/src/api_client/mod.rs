//! Generic LLM API client layer with multi-key rotation and protocol conversion.
//!
//! Provides [`ApiKeyManager`] for key pool management and [`RotatingClient`]
//! for automatic retry + key failover. Three provider-specific clients
//! ([`OpenAIRotatingClient`], [`GeminiRotatingClient`], [`AnthropicRotatingClient`])
//! wrap the base with correct auth headers and URL construction.
//!
//! Use [`create_rotating_client`] to obtain the right client for a given `authType`.

pub mod anthropic;
pub mod client;
pub mod gemini;
pub mod key_manager;
pub mod openai;

pub use anthropic::AnthropicRotatingClient;
pub use client::{ApiClientError, RotatingClient, is_retryable_status, normalize_base_url};
pub use gemini::{GeminiRotatingClient, clean_function_name};
pub use key_manager::{ApiKeyManager, ApiKeyStatus};
pub use openai::OpenAIRotatingClient;

use std::sync::Arc;

/// Options for creating a rotating client via the factory.
#[derive(Debug, Clone, Default)]
pub struct ClientOptions {
    pub max_retries: Option<usize>,
    pub retry_delay_ms: Option<u64>,
}

/// A type-erased LLM client returned by the factory.
pub enum LlmClient {
    OpenAI(OpenAIRotatingClient),
    Gemini(GeminiRotatingClient),
    Anthropic(AnthropicRotatingClient),
}

impl LlmClient {
    /// Convenience: send an OpenAI-compatible chat completion through
    /// whichever provider this client wraps.
    pub async fn create_chat_completion(
        &self,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiClientError> {
        match self {
            Self::OpenAI(c) => c.create_chat_completion(request).await,
            Self::Gemini(c) => c.create_chat_completion(request).await,
            Self::Anthropic(c) => c.create_chat_completion(request).await,
        }
    }
}

/// Create a rotating LLM client based on the provider's `authType`.
///
/// | authType | Client | Env var |
/// |----------|--------|---------|
/// | `USE_OPENAI` / default | OpenAI | `OPENAI_API_KEY` |
/// | `USE_GEMINI` / `USE_VERTEX_AI` | Gemini | `GEMINI_API_KEY` |
/// | `USE_ANTHROPIC` | Anthropic | `ANTHROPIC_API_KEY` |
pub fn create_rotating_client(
    auth_type: &str,
    api_key: &str,
    base_url: &str,
    options: ClientOptions,
) -> LlmClient {
    match auth_type {
        "USE_GEMINI" | "USE_VERTEX_AI" => {
            let km = Arc::new(ApiKeyManager::new(api_key, Some("GEMINI_API_KEY".into())));
            LlmClient::Gemini(GeminiRotatingClient::new(
                km,
                base_url,
                options.max_retries,
                options.retry_delay_ms,
            ))
        }
        "USE_ANTHROPIC" => {
            let km = Arc::new(ApiKeyManager::new(
                api_key,
                Some("ANTHROPIC_API_KEY".into()),
            ));
            LlmClient::Anthropic(AnthropicRotatingClient::new(
                km,
                base_url,
                options.max_retries,
                options.retry_delay_ms,
            ))
        }
        _ => {
            // USE_OPENAI or any unrecognized type defaults to OpenAI
            let km = Arc::new(ApiKeyManager::new(
                api_key,
                Some("OPENAI_API_KEY".into()),
            ));
            LlmClient::OpenAI(OpenAIRotatingClient::new(
                km,
                base_url,
                options.max_retries,
                options.retry_delay_ms,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_creates_openai_by_default() {
        let client = create_rotating_client("USE_OPENAI", "sk-test", "https://api.openai.com", ClientOptions::default());
        assert!(matches!(client, LlmClient::OpenAI(_)));
    }

    #[test]
    fn factory_creates_openai_for_unknown() {
        let client = create_rotating_client("UNKNOWN", "sk-test", "https://custom.api.com", ClientOptions::default());
        assert!(matches!(client, LlmClient::OpenAI(_)));
    }

    #[test]
    fn factory_creates_gemini() {
        let client = create_rotating_client("USE_GEMINI", "key", "https://generativelanguage.googleapis.com", ClientOptions::default());
        assert!(matches!(client, LlmClient::Gemini(_)));
    }

    #[test]
    fn factory_creates_gemini_for_vertex() {
        let client = create_rotating_client("USE_VERTEX_AI", "key", "https://vertex.googleapis.com", ClientOptions::default());
        assert!(matches!(client, LlmClient::Gemini(_)));
    }

    #[test]
    fn factory_creates_anthropic() {
        let client = create_rotating_client("USE_ANTHROPIC", "sk-ant-test", "https://api.anthropic.com", ClientOptions::default());
        assert!(matches!(client, LlmClient::Anthropic(_)));
    }
}
