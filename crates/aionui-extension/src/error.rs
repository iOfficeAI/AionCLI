use aionui_common::AppError;

/// Extension system domain errors.
#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("Manifest validation failed: {0}")]
    ManifestValidation(String),

    #[error("Extension name '{name}' uses reserved prefix '{prefix}'")]
    ReservedNamePrefix { name: String, prefix: String },

    #[error("Invalid version '{version}': {reason}")]
    InvalidVersion { version: String, reason: String },

    #[error("Undefined environment variable: {0}")]
    UndefinedEnvVariable(String),

    #[error("File reference not found: {0}")]
    FileReferenceNotFound(String),

    #[error("Path traversal detected: {0}")]
    PathTraversal(String),

    #[error("Engine incompatible: extension '{name}' requires aionui {required}, got {actual}")]
    EngineIncompatible {
        name: String,
        required: String,
        actual: String,
    },

    #[error("API version incompatible: extension '{name}' requires API {required}, supported {supported}")]
    ApiVersionIncompatible {
        name: String,
        required: String,
        supported: String,
    },

    #[error("State persistence failed: {0}")]
    StatePersistence(String),

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    JsonParse(#[from] serde_json::Error),
}

impl From<ExtensionError> for AppError {
    fn from(err: ExtensionError) -> Self {
        match err {
            ExtensionError::ManifestValidation(msg) => AppError::BadRequest(msg),
            ExtensionError::ReservedNamePrefix { .. } => AppError::BadRequest(err.to_string()),
            ExtensionError::InvalidVersion { .. } => AppError::BadRequest(err.to_string()),
            ExtensionError::UndefinedEnvVariable(var) => {
                AppError::BadRequest(format!("Undefined environment variable: {var}"))
            }
            ExtensionError::FileReferenceNotFound(path) => {
                AppError::NotFound(format!("File reference not found: {path}"))
            }
            ExtensionError::PathTraversal(path) => {
                AppError::BadRequest(format!("Path traversal detected: {path}"))
            }
            ExtensionError::EngineIncompatible { .. } => AppError::BadRequest(err.to_string()),
            ExtensionError::ApiVersionIncompatible { .. } => {
                AppError::BadRequest(err.to_string())
            }
            ExtensionError::StatePersistence(msg) => AppError::Internal(msg),
            ExtensionError::Io(e) => AppError::Internal(e.to_string()),
            ExtensionError::JsonParse(e) => AppError::BadRequest(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_validation_error_display() {
        let err = ExtensionError::ManifestValidation("name is required".into());
        assert_eq!(err.to_string(), "Manifest validation failed: name is required");
    }

    #[test]
    fn test_reserved_name_prefix_error_display() {
        let err = ExtensionError::ReservedNamePrefix {
            name: "aion-test".into(),
            prefix: "aion-".into(),
        };
        assert_eq!(
            err.to_string(),
            "Extension name 'aion-test' uses reserved prefix 'aion-'"
        );
    }

    #[test]
    fn test_invalid_version_error_display() {
        let err = ExtensionError::InvalidVersion {
            version: "not-semver".into(),
            reason: "unexpected character".into(),
        };
        assert_eq!(
            err.to_string(),
            "Invalid version 'not-semver': unexpected character"
        );
    }

    #[test]
    fn test_undefined_env_variable_error_display() {
        let err = ExtensionError::UndefinedEnvVariable("MY_SECRET".into());
        assert_eq!(err.to_string(), "Undefined environment variable: MY_SECRET");
    }

    #[test]
    fn test_file_reference_not_found_error_display() {
        let err = ExtensionError::FileReferenceNotFound("prompts/system.md".into());
        assert_eq!(
            err.to_string(),
            "File reference not found: prompts/system.md"
        );
    }

    #[test]
    fn test_path_traversal_error_display() {
        let err = ExtensionError::PathTraversal("../../etc/passwd".into());
        assert_eq!(
            err.to_string(),
            "Path traversal detected: ../../etc/passwd"
        );
    }

    #[test]
    fn test_into_app_error_path_traversal() {
        let err = ExtensionError::PathTraversal("../secret".into());
        let app_err: AppError = err.into();
        assert!(matches!(app_err, AppError::BadRequest(_)));
    }

    #[test]
    fn test_into_app_error_bad_request() {
        let err = ExtensionError::ManifestValidation("test".into());
        let app_err: AppError = err.into();
        assert!(matches!(app_err, AppError::BadRequest(_)));
    }

    #[test]
    fn test_into_app_error_not_found() {
        let err = ExtensionError::FileReferenceNotFound("missing.md".into());
        let app_err: AppError = err.into();
        assert!(matches!(app_err, AppError::NotFound(_)));
    }

    #[test]
    fn test_io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err = ExtensionError::from(io_err);
        assert!(matches!(err, ExtensionError::Io(_)));
        let app_err: AppError = err.into();
        assert!(matches!(app_err, AppError::Internal(_)));
    }
}
