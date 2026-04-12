use std::collections::HashMap;
use std::sync::Arc;

use aionui_api_types::{
    CreateProviderRequest, ProviderResponse, UpdateProviderRequest,
};
use aionui_common::{AppError, decrypt_string, encrypt_string};
use aionui_db::{CreateProviderParams, IProviderRepository, UpdateProviderParams, models::Provider};
use serde::de::DeserializeOwned;

/// Business logic for model provider CRUD with API key encryption/masking.
#[derive(Clone)]
pub struct ProviderService {
    repo: Arc<dyn IProviderRepository>,
    encryption_key: [u8; 32],
}

impl ProviderService {
    pub fn new(repo: Arc<dyn IProviderRepository>, encryption_key: [u8; 32]) -> Self {
        Self {
            repo,
            encryption_key,
        }
    }

    /// List all providers with masked API keys.
    pub async fn list(&self) -> Result<Vec<ProviderResponse>, AppError> {
        let rows = self.repo.list().await?;
        rows.into_iter()
            .map(|row| self.row_to_response(row))
            .collect()
    }

    /// Create a new provider. The API key is encrypted before storage.
    pub async fn create(
        &self,
        req: CreateProviderRequest,
    ) -> Result<ProviderResponse, AppError> {
        validate_create_request(&req)?;

        let encrypted_key = encrypt_string(&req.api_key, &self.encryption_key)?;
        let models_json = serialize_json(&req.models, "models")?;
        let capabilities_json = serialize_json(&req.capabilities, "capabilities")?;
        let bedrock_json = serialize_opt(&req.bedrock_config, "bedrock_config")?;

        let params = CreateProviderParams {
            platform: &req.platform,
            name: &req.name,
            base_url: &req.base_url,
            api_key_encrypted: &encrypted_key,
            models: &models_json,
            enabled: req.enabled,
            capabilities: &capabilities_json,
            context_limit: req.context_limit,
            model_protocols: None,
            model_enabled: None,
            model_health: None,
            bedrock_config: bedrock_json.as_deref(),
        };

        let row = self.repo.create(params).await?;
        self.row_to_response(row)
    }

    /// Update an existing provider. Only provided fields are changed.
    pub async fn update(
        &self,
        id: &str,
        req: UpdateProviderRequest,
    ) -> Result<ProviderResponse, AppError> {
        validate_update_request(&req)?;

        let encrypted_key = req
            .api_key
            .as_deref()
            .map(|k| encrypt_string(k, &self.encryption_key))
            .transpose()?;
        let models_json = serialize_opt(&req.models, "models")?;
        let capabilities_json = serialize_opt(&req.capabilities, "capabilities")?;
        let model_protocols_json = serialize_opt(&req.model_protocols, "model_protocols")?;
        let model_enabled_json = serialize_opt(&req.model_enabled, "model_enabled")?;
        let model_health_json = serialize_opt(&req.model_health, "model_health")?;
        let bedrock_json = serialize_opt(&req.bedrock_config, "bedrock_config")?;

        let params = UpdateProviderParams {
            platform: req.platform.as_deref(),
            name: req.name.as_deref(),
            base_url: req.base_url.as_deref(),
            api_key_encrypted: encrypted_key.as_deref(),
            models: models_json.as_deref(),
            enabled: req.enabled,
            capabilities: capabilities_json.as_deref(),
            context_limit: req.context_limit.map(Some),
            model_protocols: model_protocols_json
                .as_ref()
                .map(|s| Some(s.as_str())),
            model_enabled: model_enabled_json
                .as_ref()
                .map(|s| Some(s.as_str())),
            model_health: model_health_json
                .as_ref()
                .map(|s| Some(s.as_str())),
            bedrock_config: bedrock_json
                .as_ref()
                .map(|s| Some(s.as_str())),
        };

        let row = self.repo.update(id, params).await?;
        self.row_to_response(row)
    }

    /// Delete a provider by ID.
    pub async fn delete(&self, id: &str) -> Result<(), AppError> {
        self.repo.delete(id).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Convert a DB row into a response DTO with masked API key and
    /// deserialized JSON fields.
    fn row_to_response(&self, row: Provider) -> Result<ProviderResponse, AppError> {
        let decrypted_key = decrypt_string(&row.api_key_encrypted, &self.encryption_key)?;
        let masked_key = mask_api_key(&decrypted_key);

        let models: Vec<String> = serde_json::from_str(&row.models)
            .map_err(|e| AppError::Internal(format!("Failed to parse models JSON: {e}")))?;
        let capabilities = serde_json::from_str(&row.capabilities)
            .map_err(|e| AppError::Internal(format!("Failed to parse capabilities JSON: {e}")))?;
        let model_protocols: Option<HashMap<String, String>> =
            deserialize_opt(&row.model_protocols, "model_protocols")?;
        let model_enabled: Option<HashMap<String, bool>> =
            deserialize_opt(&row.model_enabled, "model_enabled")?;
        let model_health = deserialize_opt(&row.model_health, "model_health")?;
        let bedrock_config = deserialize_opt(&row.bedrock_config, "bedrock_config")?;

        Ok(ProviderResponse {
            id: row.id,
            platform: row.platform,
            name: row.name,
            base_url: row.base_url,
            api_key: masked_key,
            models,
            enabled: row.enabled,
            capabilities,
            context_limit: row.context_limit,
            model_protocols,
            model_enabled,
            model_health,
            bedrock_config,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

// ---------------------------------------------------------------------------
// JSON helpers (M-1 / M-2 refactor)
// ---------------------------------------------------------------------------

/// Serialize an optional value to JSON string.
fn serialize_opt<T: serde::Serialize>(
    val: &Option<T>,
    field: &str,
) -> Result<Option<String>, AppError> {
    val.as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| AppError::Internal(format!("Failed to serialize {field}: {e}")))
}

/// Serialize a value to JSON string.
fn serialize_json<T: serde::Serialize>(val: &T, field: &str) -> Result<String, AppError> {
    serde_json::to_string(val)
        .map_err(|e| AppError::Internal(format!("Failed to serialize {field}: {e}")))
}

/// Deserialize an optional JSON string into a typed value.
pub(crate) fn deserialize_opt<T: DeserializeOwned>(
    json: &Option<String>,
    field: &str,
) -> Result<Option<T>, AppError> {
    json.as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| AppError::Internal(format!("Failed to parse {field} JSON: {e}")))
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn validate_create_request(req: &CreateProviderRequest) -> Result<(), AppError> {
    if req.platform.trim().is_empty() {
        return Err(AppError::BadRequest(
            "platform is required".into(),
        ));
    }
    if req.name.trim().is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    validate_base_url(&req.base_url)?;
    if req.api_key.trim().is_empty() {
        return Err(AppError::BadRequest(
            "apiKey is required".into(),
        ));
    }
    Ok(())
}

fn validate_update_request(req: &UpdateProviderRequest) -> Result<(), AppError> {
    if let Some(ref platform) = req.platform
        && platform.trim().is_empty()
    {
        return Err(AppError::BadRequest(
            "platform cannot be empty".into(),
        ));
    }
    if let Some(ref name) = req.name
        && name.trim().is_empty()
    {
        return Err(AppError::BadRequest(
            "name cannot be empty".into(),
        ));
    }
    if let Some(ref url) = req.base_url {
        validate_base_url(url)?;
    }
    Ok(())
}

fn validate_base_url(url: &str) -> Result<(), AppError> {
    if url.trim().is_empty() {
        return Err(AppError::BadRequest(
            "baseUrl is required".into(),
        ));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(AppError::BadRequest(
            "baseUrl must start with http:// or https://".into(),
        ));
    }
    Ok(())
}

/// Mask an API key for display: preserve the prefix (up to the last dash
/// before the secret part) and the last 4 characters, replacing the middle
/// with `***`.
///
/// Examples:
/// - `sk-ant-api03-abcdefghijkl` → `sk-ant-***ijkl`
/// - `sk-proj-abcdefgh` → `sk-proj-***efgh`
/// - `short` → `***ort`
/// - empty → `***`
pub(crate) fn mask_api_key(key: &str) -> String {
    if key.is_empty() {
        return "***".to_string();
    }

    let tail_len = 4;

    // Find prefix: everything up to and including the last '-' that precedes
    // the final segment. For "sk-ant-api03-secret", prefix = "sk-ant-".
    // We find the last '-' that has at least `tail_len` chars after it.
    let prefix_end = key
        .rmatch_indices('-')
        .find(|(i, _)| key.len() - i > tail_len)
        .map(|(i, _)| i + 1);

    match prefix_end {
        Some(pe) => {
            let suffix_start = key.len().saturating_sub(tail_len);
            let prefix = &key[..pe];
            let suffix = &key[suffix_start..];
            format!("{prefix}***{suffix}")
        }
        None => {
            // No suitable dash — just show ***<last 4 chars>
            let suffix_start = key.len().saturating_sub(tail_len);
            let suffix = &key[suffix_start..];
            format!("***{suffix}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_db::{SqliteProviderRepository, init_database_memory};

    // A fixed 32-byte key for testing
    const TEST_KEY: [u8; 32] = [0x42; 32];

    async fn setup() -> ProviderService {
        let db = init_database_memory().await.unwrap();
        let repo = Arc::new(SqliteProviderRepository::new(db.pool().clone()));
        std::mem::forget(db);
        ProviderService::new(repo, TEST_KEY)
    }

    fn sample_create_request() -> CreateProviderRequest {
        CreateProviderRequest {
            platform: "anthropic".into(),
            name: "Anthropic".into(),
            base_url: "https://api.anthropic.com".into(),
            api_key: "sk-ant-api03-test1234".into(),
            models: vec!["claude-sonnet-4-20250514".into()],
            enabled: true,
            capabilities: vec![],
            context_limit: None,
            bedrock_config: None,
        }
    }

    // -- mask_api_key tests --

    #[test]
    fn mask_api_key_standard() {
        assert_eq!(
            mask_api_key("sk-ant-api03-abcdefghijkl"),
            "sk-ant-api03-***ijkl"
        );
    }

    #[test]
    fn mask_api_key_short_prefix() {
        assert_eq!(mask_api_key("sk-proj-abcdefgh"), "sk-proj-***efgh");
    }

    #[test]
    fn mask_api_key_no_dash() {
        assert_eq!(mask_api_key("abcdefgh"), "***efgh");
    }

    #[test]
    fn mask_api_key_short_key() {
        assert_eq!(mask_api_key("abc"), "***abc");
    }

    #[test]
    fn mask_api_key_empty() {
        assert_eq!(mask_api_key(""), "***");
    }

    #[test]
    fn mask_api_key_exactly_four() {
        assert_eq!(mask_api_key("abcd"), "***abcd");
    }

    // -- validation tests --

    #[test]
    fn validate_create_missing_platform() {
        let req = CreateProviderRequest {
            platform: "".into(),
            ..sample_create_request()
        };
        assert!(validate_create_request(&req).is_err());
    }

    #[test]
    fn validate_create_missing_name() {
        let req = CreateProviderRequest {
            name: "  ".into(),
            ..sample_create_request()
        };
        assert!(validate_create_request(&req).is_err());
    }

    #[test]
    fn validate_create_missing_base_url() {
        let req = CreateProviderRequest {
            base_url: "".into(),
            ..sample_create_request()
        };
        assert!(validate_create_request(&req).is_err());
    }

    #[test]
    fn validate_create_invalid_url() {
        let req = CreateProviderRequest {
            base_url: "not-a-url".into(),
            ..sample_create_request()
        };
        assert!(validate_create_request(&req).is_err());
    }

    #[test]
    fn validate_create_missing_api_key() {
        let req = CreateProviderRequest {
            api_key: "  ".into(),
            ..sample_create_request()
        };
        assert!(validate_create_request(&req).is_err());
    }

    #[test]
    fn validate_create_valid() {
        assert!(validate_create_request(&sample_create_request()).is_ok());
    }

    #[test]
    fn validate_update_empty_name_rejected() {
        let req = UpdateProviderRequest {
            name: Some("".into()),
            ..Default::default()
        };
        assert!(validate_update_request(&req).is_err());
    }

    #[test]
    fn validate_update_empty_request_ok() {
        assert!(validate_update_request(&UpdateProviderRequest::default()).is_ok());
    }

    #[test]
    fn validate_base_url_http() {
        assert!(validate_base_url("http://localhost:8080").is_ok());
    }

    #[test]
    fn validate_base_url_https() {
        assert!(validate_base_url("https://api.example.com").is_ok());
    }

    #[test]
    fn validate_base_url_ftp_rejected() {
        assert!(validate_base_url("ftp://files.example.com").is_err());
    }

    // -- service integration tests --

    #[tokio::test]
    async fn list_empty() {
        let svc = setup().await;
        let result = svc.list().await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn create_and_list() {
        let svc = setup().await;
        let created = svc.create(sample_create_request()).await.unwrap();

        assert!(created.id.starts_with("prov_"));
        assert_eq!(created.platform, "anthropic");
        assert_eq!(created.name, "Anthropic");
        assert_eq!(created.base_url, "https://api.anthropic.com");
        // API key should be masked
        assert!(created.api_key.contains("***"));
        assert!(!created.api_key.contains("test1234"));
        assert!(created.api_key.ends_with("1234"));
        assert_eq!(created.models, vec!["claude-sonnet-4-20250514"]);
        assert!(created.enabled);

        let all = svc.list().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, created.id);
    }

    #[tokio::test]
    async fn create_invalid_request_rejected() {
        let svc = setup().await;
        let req = CreateProviderRequest {
            platform: "".into(),
            ..sample_create_request()
        };
        let err = svc.create(req).await.unwrap_err();
        assert_eq!(err.status_code(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_name() {
        let svc = setup().await;
        let created = svc.create(sample_create_request()).await.unwrap();

        let updated = svc
            .update(
                &created.id,
                UpdateProviderRequest {
                    name: Some("New Name".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.name, "New Name");
        assert_eq!(updated.platform, "anthropic");
    }

    #[tokio::test]
    async fn update_api_key_re_encrypts() {
        let svc = setup().await;
        let created = svc.create(sample_create_request()).await.unwrap();

        let updated = svc
            .update(
                &created.id,
                UpdateProviderRequest {
                    api_key: Some("new-key-abcdefgh".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // Masked key should reflect the new key
        assert!(updated.api_key.ends_with("efgh"));
    }

    #[tokio::test]
    async fn update_nonexistent_returns_not_found() {
        let svc = setup().await;
        let err = svc
            .update("no_such_id", UpdateProviderRequest::default())
            .await
            .unwrap_err();
        assert_eq!(err.status_code(), axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_existing() {
        let svc = setup().await;
        let created = svc.create(sample_create_request()).await.unwrap();

        svc.delete(&created.id).await.unwrap();
        let all = svc.list().await.unwrap();
        assert!(all.is_empty());
    }

    #[tokio::test]
    async fn delete_nonexistent_returns_not_found() {
        let svc = setup().await;
        let err = svc.delete("no_such_id").await.unwrap_err();
        assert_eq!(err.status_code(), axum::http::StatusCode::NOT_FOUND);
    }
}
