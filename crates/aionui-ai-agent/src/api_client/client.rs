use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, warn};

use super::key_manager::ApiKeyManager;

/// Errors returned by the API client layer.
#[derive(Debug, thiserror::Error)]
pub enum ApiClientError {
    #[error("no API keys configured")]
    NoKeysConfigured,

    #[error("all API keys exhausted (all blacklisted)")]
    AllKeysExhausted,

    #[error("HTTP error {status}: {body}")]
    HttpError {
        status: u16,
        body: String,
        retryable: bool,
    },

    #[error("request failed: {0}")]
    RequestFailed(#[from] reqwest::Error),

    #[error("max retries exceeded after {attempts} attempts: {last_error}")]
    MaxRetriesExceeded { attempts: usize, last_error: String },
}

/// Default maximum retries for API calls.
const DEFAULT_MAX_RETRIES: usize = 3;
/// Default base delay for exponential backoff (ms).
const DEFAULT_RETRY_DELAY_MS: u64 = 1000;

/// Core rotating client that wraps an HTTP client with multi-key
/// failover and exponential backoff retry.
pub struct RotatingClient {
    key_manager: Arc<ApiKeyManager>,
    http_client: reqwest::Client,
    base_url: String,
    max_retries: usize,
    retry_delay_ms: u64,
}

impl RotatingClient {
    pub fn new(
        key_manager: Arc<ApiKeyManager>,
        base_url: &str,
        max_retries: Option<usize>,
        retry_delay_ms: Option<u64>,
    ) -> Self {
        Self {
            key_manager,
            http_client: reqwest::Client::new(),
            base_url: normalize_base_url(base_url),
            max_retries: max_retries.unwrap_or(DEFAULT_MAX_RETRIES),
            retry_delay_ms: retry_delay_ms.unwrap_or(DEFAULT_RETRY_DELAY_MS),
        }
    }

    pub fn key_manager(&self) -> &Arc<ApiKeyManager> {
        &self.key_manager
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Execute an HTTP request with automatic key rotation and retry.
    ///
    /// `build_request` receives `(http_client, base_url, api_key)` and
    /// must return a ready-to-send [`reqwest::RequestBuilder`].
    pub async fn execute_with_retry<F>(
        &self,
        build_request: F,
    ) -> Result<serde_json::Value, ApiClientError>
    where
        F: Fn(&reqwest::Client, &str, &str) -> reqwest::RequestBuilder,
    {
        let mut last_error = String::new();

        for attempt in 0..=self.max_retries {
            let key = self.key_manager.get_available_key().await.ok_or(
                if attempt == 0 {
                    ApiClientError::NoKeysConfigured
                } else {
                    ApiClientError::AllKeysExhausted
                },
            )?;

            let request = build_request(&self.http_client, &self.base_url, &key);

            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        return response
                            .json::<serde_json::Value>()
                            .await
                            .map_err(ApiClientError::RequestFailed);
                    }

                    let status_code = status.as_u16();
                    let body = response.text().await.unwrap_or_default();
                    let retryable = is_retryable_status(status_code);

                    if retryable && attempt < self.max_retries {
                        warn!(
                            status = status_code,
                            attempt,
                            "retryable error, rotating key"
                        );
                        self.key_manager.blacklist_current().await;
                        last_error = format!("HTTP {status_code}: {body}");
                        let delay = self.retry_delay_ms * (attempt as u64 + 1);
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        continue;
                    }

                    return Err(ApiClientError::HttpError {
                        status: status_code,
                        body,
                        retryable,
                    });
                }
                Err(e) => {
                    if attempt < self.max_retries {
                        warn!(attempt, error = %e, "request failed, retrying");
                        self.key_manager.blacklist_current().await;
                        last_error = e.to_string();
                        let delay = self.retry_delay_ms * (attempt as u64 + 1);
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        continue;
                    }
                    return Err(ApiClientError::RequestFailed(e));
                }
            }
        }

        debug!(
            attempts = self.max_retries + 1,
            "max retries exceeded"
        );
        Err(ApiClientError::MaxRetriesExceeded {
            attempts: self.max_retries + 1,
            last_error,
        })
    }
}

/// Check if an HTTP status code indicates a retryable error.
///
/// Retryable: 401 (bad key), 429 (rate limit), 503 (service unavailable),
/// or any 5xx server error.
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 401 | 429 | 503) || (500..600).contains(&status)
}

/// Strip trailing `/v1` or `/v1beta` from a base URL so that each
/// client can append its own API version prefix.
///
/// This handles the "new-api" platform convention where users may
/// paste a URL that already includes the version path.
pub fn normalize_base_url(url: &str) -> String {
    let url = url.trim_end_matches('/');
    url.strip_suffix("/v1beta")
        .or_else(|| url.strip_suffix("/v1"))
        .unwrap_or(url)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_401() {
        assert!(is_retryable_status(401));
    }

    #[test]
    fn retryable_429() {
        assert!(is_retryable_status(429));
    }

    #[test]
    fn retryable_503() {
        assert!(is_retryable_status(503));
    }

    #[test]
    fn retryable_500() {
        assert!(is_retryable_status(500));
    }

    #[test]
    fn retryable_502() {
        assert!(is_retryable_status(502));
    }

    #[test]
    fn not_retryable_200() {
        assert!(!is_retryable_status(200));
    }

    #[test]
    fn not_retryable_400() {
        assert!(!is_retryable_status(400));
    }

    #[test]
    fn not_retryable_403() {
        assert!(!is_retryable_status(403));
    }

    #[test]
    fn not_retryable_404() {
        assert!(!is_retryable_status(404));
    }

    #[test]
    fn normalize_strips_v1() {
        assert_eq!(
            normalize_base_url("https://api.example.com/v1"),
            "https://api.example.com"
        );
    }

    #[test]
    fn normalize_strips_v1beta() {
        assert_eq!(
            normalize_base_url("https://api.example.com/v1beta"),
            "https://api.example.com"
        );
    }

    #[test]
    fn normalize_strips_trailing_slash() {
        assert_eq!(
            normalize_base_url("https://api.example.com/v1/"),
            "https://api.example.com"
        );
    }

    #[test]
    fn normalize_leaves_clean_url() {
        assert_eq!(
            normalize_base_url("https://api.example.com"),
            "https://api.example.com"
        );
    }

    #[test]
    fn normalize_preserves_other_paths() {
        assert_eq!(
            normalize_base_url("https://api.example.com/custom"),
            "https://api.example.com/custom"
        );
    }
}
