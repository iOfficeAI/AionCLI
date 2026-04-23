use std::collections::HashMap;
use std::time::Instant;

use aionui_api_types::{
    AcpEnvResponse, AcpHealthCheckResponse, DetectCliResponse, TestCustomAgentResponse,
};
use aionui_common::{AcpBackend, AppError};
use tracing::debug;

/// Detect the CLI path for a given ACP backend using PATH lookup.
pub fn detect_cli(backend: AcpBackend) -> DetectCliResponse {
    let binary = match backend.cli_binary_name() {
        Some(name) => name,
        None => return DetectCliResponse { path: None },
    };

    let path = which::which(binary)
        .ok()
        .map(|p| p.to_string_lossy().into_owned());

    debug!(backend = ?backend, binary, ?path, "CLI detection result");
    DetectCliResponse { path }
}

/// Perform a health check for an ACP backend.
///
/// Checks CLI availability and measures detection latency.
pub fn health_check(backend: AcpBackend) -> AcpHealthCheckResponse {
    let start = Instant::now();

    let binary = match backend.cli_binary_name() {
        Some(name) => name,
        None => {
            return AcpHealthCheckResponse {
                available: false,
                latency: None,
                error: Some(format!("Backend {backend:?} has no CLI binary")),
            };
        }
    };

    let available = which::which(binary).is_ok();
    let latency_ms = start.elapsed().as_millis() as u64;

    AcpHealthCheckResponse {
        available,
        latency: Some(latency_ms),
        error: if available {
            None
        } else {
            Some(format!("CLI '{binary}' not found in PATH"))
        },
    }
}

/// Get relevant environment variables for ACP operations.
pub fn get_env() -> AcpEnvResponse {
    let keys = ["PATH", "HOME", "USER", "SHELL", "LANG", "TERM"];
    let env: HashMap<String, String> = keys
        .iter()
        .filter_map(|&key| std::env::var(key).ok().map(|val| (key.into(), val)))
        .collect();

    AcpEnvResponse { env }
}

/// Test a custom ACP agent by verifying the command exists.
pub fn test_custom_agent(
    command: &str,
    _acp_args: &[String],
    _env: &HashMap<String, String>,
) -> Result<TestCustomAgentResponse, AppError> {
    // Verify the command exists
    which::which(command)
        .map_err(|_| AppError::BadRequest(format!("Command '{command}' not found in PATH")))?;

    Ok(TestCustomAgentResponse {
        step: "completed".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_cli_non_cli_backend_returns_none() {
        let resp = detect_cli(AcpBackend::Custom);
        assert!(resp.path.is_none());
    }

    #[test]
    fn health_check_non_cli_backend() {
        let resp = health_check(AcpBackend::Custom);
        assert!(!resp.available);
        assert!(resp.error.is_some());
    }

    #[test]
    fn get_env_returns_at_least_path() {
        let resp = get_env();
        assert!(resp.env.contains_key("PATH") || resp.env.contains_key("HOME"));
    }

    #[test]
    fn test_custom_agent_nonexistent_command() {
        let result = test_custom_agent("/nonexistent/path/to/agent", &[], &HashMap::new());
        assert!(result.is_err());
    }
}
