pub use aionui_common::EnvVar;
use aionui_common::{AcpBackend, AgentType};
use serde::{Deserialize, Serialize};

/// How an agent was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentSource {
    Internal,
    Builtin,
    Extension,
    Custom,
}

/// A discovered agent from any source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedAgent {
    pub id: String,
    pub name: String,
    pub agent_type: AgentType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<AcpBackend>,
    pub available: bool,
    pub source: AgentSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvVar>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_source_serde_roundtrip() {
        let cases = [
            (AgentSource::Internal, "internal"),
            (AgentSource::Builtin, "builtin"),
            (AgentSource::Extension, "extension"),
            (AgentSource::Custom, "custom"),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
            let parsed: AgentSource = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn detected_agent_serialization_skips_empty_fields() {
        let agent = DetectedAgent {
            id: "abc12345".into(),
            name: "Claude".into(),
            agent_type: AgentType::Acp,
            backend: Some(AcpBackend::Claude),
            available: true,
            source: AgentSource::Builtin,
            command: None,
            args: vec![],
            env: vec![],
        };
        let json = serde_json::to_value(&agent).unwrap();
        assert_eq!(json["id"], "abc12345");
        assert_eq!(json["agent_type"], "acp");
        assert_eq!(json["backend"], "claude");
        assert_eq!(json["source"], "builtin");
        assert!(json.get("command").is_none());
        assert!(json.get("args").is_none());
        assert!(json.get("env").is_none());
    }

    #[test]
    fn detected_agent_deserialization() {
        let json = json!({
            "id": "ext123",
            "name": "MyAgent",
            "agent_type": "acp",
            "backend": "claude",
            "available": true,
            "source": "extension",
            "command": "/usr/bin/my-cli",
            "env": [{"name": "FOO", "value": "bar"}]
        });
        let agent: DetectedAgent = serde_json::from_value(json).unwrap();
        assert_eq!(agent.agent_type, AgentType::Acp);
        assert_eq!(agent.backend, Some(AcpBackend::Claude));
        assert_eq!(agent.source, AgentSource::Extension);
        assert_eq!(agent.command.as_deref(), Some("/usr/bin/my-cli"));
        assert_eq!(agent.env.len(), 1);
        assert_eq!(agent.env[0].name, "FOO");
    }

    #[test]
    fn detected_agent_without_backend() {
        let agent = DetectedAgent {
            id: "oc123".into(),
            name: "OpenClaw Gateway".into(),
            agent_type: AgentType::OpenclawGateway,
            backend: None,
            available: true,
            source: AgentSource::Builtin,
            command: Some("/usr/bin/openclaw".into()),
            args: vec![],
            env: vec![],
        };
        let json = serde_json::to_value(&agent).unwrap();
        assert_eq!(json["agent_type"], "openclaw-gateway");
        assert!(json.get("backend").is_none());
    }
}
