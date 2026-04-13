use std::collections::HashMap;
use std::time::Instant;

use aionui_api_types::{
    AcpAgentInfo, AcpEnvResponse, AcpHealthCheckResponse, DetectCliResponse,
    TestCustomAgentResponse,
};
use aionui_common::{AcpBackend, AppError};
use tracing::debug;

/// Known ACP backend CLI binary names.
///
/// Returns the expected executable name for a given ACP backend,
/// or `None` for backends that don't have a standalone CLI.
fn cli_binary_name(backend: AcpBackend) -> Option<&'static str> {
    match backend {
        AcpBackend::Claude => Some("claude"),
        AcpBackend::Qwen => Some("qwen"),
        AcpBackend::Codex => Some("codex"),
        AcpBackend::CodeBuddy => Some("codebuddy"),
        AcpBackend::Kiro => Some("kiro"),
        AcpBackend::OpenCode => Some("opencode"),
        AcpBackend::Copilot => Some("copilot"),
        AcpBackend::Goose => Some("goose"),
        AcpBackend::Cursor => Some("cursor"),
        AcpBackend::Droid => Some("droid"),
        AcpBackend::Auggie => Some("auggie"),
        AcpBackend::Kimi => Some("kimi"),
        AcpBackend::Qoder => Some("qoder"),
        AcpBackend::Vibe => Some("vibe"),
        AcpBackend::Nanobot => Some("nanobot"),
        // These backends don't have a direct CLI to detect
        AcpBackend::IFlow => None,
        AcpBackend::Gemini => None,
        AcpBackend::OpenclawGateway => None,
        AcpBackend::Remote => None,
        AcpBackend::Aionrs => None,
        AcpBackend::Custom => None,
    }
}

/// Predefined list of known ACP agents.
fn known_agents() -> Vec<AcpAgentInfo> {
    vec![
        AcpAgentInfo {
            id: "claude".into(),
            name: "Claude".into(),
            backend: AcpBackend::Claude,
            available: false,
        },
        AcpAgentInfo {
            id: "codex".into(),
            name: "Codex".into(),
            backend: AcpBackend::Codex,
            available: false,
        },
        AcpAgentInfo {
            id: "codebuddy".into(),
            name: "CodeBuddy".into(),
            backend: AcpBackend::CodeBuddy,
            available: false,
        },
        AcpAgentInfo {
            id: "qwen".into(),
            name: "Qwen".into(),
            backend: AcpBackend::Qwen,
            available: false,
        },
        AcpAgentInfo {
            id: "kiro".into(),
            name: "Kiro".into(),
            backend: AcpBackend::Kiro,
            available: false,
        },
        AcpAgentInfo {
            id: "opencode".into(),
            name: "OpenCode".into(),
            backend: AcpBackend::OpenCode,
            available: false,
        },
        AcpAgentInfo {
            id: "copilot".into(),
            name: "Copilot".into(),
            backend: AcpBackend::Copilot,
            available: false,
        },
        AcpAgentInfo {
            id: "goose".into(),
            name: "Goose".into(),
            backend: AcpBackend::Goose,
            available: false,
        },
    ]
}

/// Detect the CLI path for a given ACP backend using PATH lookup.
pub fn detect_cli(backend: AcpBackend) -> DetectCliResponse {
    let binary = match cli_binary_name(backend) {
        Some(name) => name,
        None => return DetectCliResponse { path: None },
    };

    let path = which::which(binary)
        .ok()
        .map(|p| p.to_string_lossy().into_owned());

    debug!(backend = ?backend, binary, ?path, "CLI detection result");
    DetectCliResponse { path }
}

/// Get the list of available ACP agents, checking CLI availability.
pub fn get_available_agents() -> Vec<AcpAgentInfo> {
    known_agents()
        .into_iter()
        .map(|mut agent| {
            if let Some(binary) = cli_binary_name(agent.backend) {
                agent.available = which::which(binary).is_ok();
            }
            agent
        })
        .collect()
}

/// Perform a health check for an ACP backend.
///
/// Checks CLI availability and measures detection latency.
pub fn health_check(backend: AcpBackend) -> AcpHealthCheckResponse {
    let start = Instant::now();

    let binary = match cli_binary_name(backend) {
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
    which::which(command).map_err(|_| {
        AppError::BadRequest(format!("Command '{command}' not found in PATH"))
    })?;

    Ok(TestCustomAgentResponse {
        step: "completed".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_binary_name_known_backends() {
        assert_eq!(cli_binary_name(AcpBackend::Claude), Some("claude"));
        assert_eq!(cli_binary_name(AcpBackend::Qwen), Some("qwen"));
        assert_eq!(cli_binary_name(AcpBackend::Codex), Some("codex"));
        assert_eq!(cli_binary_name(AcpBackend::CodeBuddy), Some("codebuddy"));
        assert_eq!(cli_binary_name(AcpBackend::Kiro), Some("kiro"));
    }

    #[test]
    fn cli_binary_name_returns_none_for_non_cli_backends() {
        assert_eq!(cli_binary_name(AcpBackend::IFlow), None);
        assert_eq!(cli_binary_name(AcpBackend::Gemini), None);
        assert_eq!(cli_binary_name(AcpBackend::Custom), None);
        assert_eq!(cli_binary_name(AcpBackend::Remote), None);
        assert_eq!(cli_binary_name(AcpBackend::Aionrs), None);
    }

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
        // PATH should generally be available in any environment
        assert!(resp.env.contains_key("PATH") || resp.env.contains_key("HOME"));
    }

    #[test]
    fn get_available_agents_returns_known_list() {
        let agents = get_available_agents();
        assert!(!agents.is_empty());
        // Claude should be in the list
        assert!(agents.iter().any(|a| a.id == "claude"));
    }

    #[test]
    fn known_agents_list_is_complete() {
        let agents = known_agents();
        let ids: Vec<&str> = agents.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"claude"));
        assert!(ids.contains(&"codex"));
        assert!(ids.contains(&"codebuddy"));
        assert!(ids.contains(&"qwen"));
        assert!(ids.contains(&"kiro"));
    }

    #[test]
    fn test_custom_agent_nonexistent_command() {
        let result = test_custom_agent(
            "/nonexistent/path/to/agent",
            &[],
            &HashMap::new(),
        );
        assert!(result.is_err());
    }
}
