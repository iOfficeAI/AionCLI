use std::collections::HashMap;

use aionui_common::AcpBackend;
use serde::{Deserialize, Serialize};

/// Request body for detecting an ACP CLI executable.
#[derive(Debug, Deserialize)]
pub struct DetectCliRequest {
    pub backend: AcpBackend,
}

/// Response for CLI detection.
#[derive(Debug, Serialize)]
pub struct DetectCliResponse {
    /// Path to the detected CLI, `None` if not found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Information about an available ACP agent (public API shape).
#[derive(Debug, Clone, Serialize)]
pub struct AcpAgentInfo {
    pub id: String,
    pub name: String,
    pub backend: AcpBackend,
    pub available: bool,
    pub source: crate::AgentSource,
}

impl From<crate::DetectedAgent> for AcpAgentInfo {
    fn from(a: crate::DetectedAgent) -> Self {
        Self {
            id: a.id,
            name: a.name,
            backend: a.backend,
            available: a.available,
            source: a.source,
        }
    }
}

/// Request body for ACP health check.
#[derive(Debug, Deserialize)]
pub struct AcpHealthCheckRequest {
    pub backend: AcpBackend,
}

/// Response for ACP health check.
#[derive(Debug, Serialize)]
pub struct AcpHealthCheckResponse {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response for ACP environment variables.
#[derive(Debug, Serialize)]
pub struct AcpEnvResponse {
    pub env: HashMap<String, String>,
}

/// Response for ACP session mode.
#[derive(Debug, Serialize)]
pub struct AcpModeResponse {
    pub mode: String,
    pub initialized: bool,
}

/// Request body for setting ACP session mode.
#[derive(Debug, Deserialize)]
pub struct SetModeRequest {
    pub mode: String,
}

/// Request body for setting ACP session model.
#[derive(Debug, Deserialize)]
pub struct SetModelRequest {
    pub model_id: String,
}

/// Request body for probing model information.
#[derive(Debug, Deserialize)]
pub struct ProbeModelRequest {
    pub backend: AcpBackend,
}

/// Request body for setting a config option.
#[derive(Debug, Deserialize)]
pub struct SetConfigOptionRequest {
    pub value: String,
}

/// Request body for testing a custom ACP agent.
#[derive(Debug, Deserialize)]
pub struct TestCustomAgentRequest {
    pub command: String,
    #[serde(default)]
    pub acp_args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Response for testing a custom ACP agent.
#[derive(Debug, Serialize)]
pub struct TestCustomAgentResponse {
    pub step: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detect_cli_request_serde() {
        let json = json!({ "backend": "claude" });
        let req: DetectCliRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.backend, AcpBackend::Claude);
    }

    #[test]
    fn detect_cli_response_with_path() {
        let resp = DetectCliResponse {
            path: Some("/usr/local/bin/claude".into()),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["path"], "/usr/local/bin/claude");
    }

    #[test]
    fn detect_cli_response_without_path() {
        let resp = DetectCliResponse { path: None };
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("path").is_none());
    }

    #[test]
    fn acp_agent_info_serde() {
        let info = AcpAgentInfo {
            id: "claude".into(),
            name: "Claude".into(),
            backend: AcpBackend::Claude,
            available: true,
            source: crate::AgentSource::Builtin,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["id"], "claude");
        assert_eq!(json["backend"], "claude");
        assert_eq!(json["available"], true);
        assert_eq!(json["source"], "builtin");
    }

    #[test]
    fn health_check_response_available() {
        let resp = AcpHealthCheckResponse {
            available: true,
            latency: Some(120),
            error: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["available"], true);
        assert_eq!(json["latency"], 120);
        assert!(json.get("error").is_none());
    }

    #[test]
    fn health_check_response_unavailable() {
        let resp = AcpHealthCheckResponse {
            available: false,
            latency: None,
            error: Some("CLI not found".into()),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["available"], false);
        assert_eq!(json["error"], "CLI not found");
    }

    #[test]
    fn set_mode_request_serde() {
        let json = json!({ "mode": "code" });
        let req: SetModeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.mode, "code");
    }

    #[test]
    fn set_model_request_serde() {
        let json = json!({ "model_id": "claude-sonnet-4" });
        let req: SetModelRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.model_id, "claude-sonnet-4");
    }

    #[test]
    fn set_config_option_request_serde() {
        let json = json!({ "value": "dark" });
        let req: SetConfigOptionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.value, "dark");
    }

    #[test]
    fn test_custom_agent_request_serde() {
        let json = json!({
            "command": "/path/to/agent",
            "acp_args": ["--flag"],
            "env": { "KEY": "value" }
        });
        let req: TestCustomAgentRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.command, "/path/to/agent");
        assert_eq!(req.acp_args, vec!["--flag"]);
        assert_eq!(req.env.get("KEY"), Some(&"value".into()));
    }

    #[test]
    fn test_custom_agent_request_defaults() {
        let json = json!({ "command": "/bin/test" });
        let req: TestCustomAgentRequest = serde_json::from_value(json).unwrap();
        assert!(req.acp_args.is_empty());
        assert!(req.env.is_empty());
    }

    #[test]
    fn env_response_serde() {
        let resp = AcpEnvResponse {
            env: HashMap::from([
                ("PATH".into(), "/usr/bin".into()),
                ("HOME".into(), "/home/user".into()),
            ]),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["env"]["PATH"], "/usr/bin");
    }

    #[test]
    fn probe_model_request_serde() {
        let json = json!({ "backend": "claude" });
        let req: ProbeModelRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.backend, AcpBackend::Claude);
    }
}
