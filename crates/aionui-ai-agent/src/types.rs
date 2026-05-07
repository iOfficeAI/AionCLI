use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use aionui_common::{AgentType, ProviderWithModel};

/// Data payload for sending a user message to an Agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageData {
    /// User message content.
    pub content: String,
    /// Client-generated message ID for correlation.
    pub msg_id: String,
    /// File paths attached to the message.
    #[serde(default)]
    pub files: Vec<String>,
    /// Skills to inject into this message turn.
    #[serde(default)]
    pub inject_skills: Vec<String>,
}

/// Options for building (creating or resuming) an Agent task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildTaskOptions {
    /// Type of agent to create.
    pub agent_type: AgentType,
    /// Working directory for the agent.
    pub workspace: String,
    /// Model selection config.
    pub model: ProviderWithModel,
    /// Conversation ID this task belongs to.
    pub conversation_id: String,
    /// Type-specific extra parameters (JSON object).
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// Provider-specific compat overrides resolved in the factory.
#[derive(Debug, Clone, Default)]
pub struct AionrsCompatOverrides {
    pub max_tokens_field: Option<String>,
    pub api_path: Option<String>,
}

/// Fully resolved Aionrs configuration passed to the agent manager.
#[derive(Debug, Clone)]
pub struct AionrsResolvedConfig {
    /// LLM provider name (anthropic, openai, bedrock, vertex).
    pub provider: String,
    /// Decrypted API key.
    pub api_key: String,
    /// Model identifier.
    pub model: String,
    /// Provider base URL.
    pub base_url: Option<String>,
    /// System prompt override.
    pub system_prompt: Option<String>,
    /// Max tokens per response.
    pub max_tokens: u32,
    /// Max agentic turns.
    pub max_turns: Option<usize>,
    /// Provider-specific compat overrides.
    pub compat_overrides: AionrsCompatOverrides,
    /// Directory for aionrs session persistence files.
    pub session_directory: PathBuf,
    /// Session mode (default, auto_edit, yolo).
    pub session_mode: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_api_types::{
        AcpBuildExtra, AcpModelInfo, AcpSessionConfigOption, AionrsBuildExtra, OpenClawGatewayConfig, SlashCommandItem,
    };
    use serde_json::json;

    #[test]
    fn acp_build_extra_accepts_payload_without_skills() {
        let legacy = r#"{"backend":"claude"}"#;
        let parsed: AcpBuildExtra = serde_json::from_str(legacy).unwrap();
        assert!(parsed.skills.is_empty());
    }

    #[test]
    fn acp_build_extra_accepts_skills() {
        let with_field = r#"{"backend":"claude","skills":["cron","pdf"]}"#;
        let parsed: AcpBuildExtra = serde_json::from_str(with_field).unwrap();
        assert_eq!(parsed.skills, vec!["cron".to_owned(), "pdf".to_owned()]);
    }

    #[test]
    fn acp_build_extra_missing_team_mcp_stdio_config_is_none() {
        let legacy = r#"{"backend":"claude","skills":["cron"]}"#;
        let parsed: AcpBuildExtra = serde_json::from_str(legacy).unwrap();
        assert!(parsed.team_mcp_stdio_config.is_none());
    }

    #[test]
    fn acp_build_extra_parses_team_mcp_stdio_config() {
        let with_cfg = r#"{
            "backend":"claude",
            "team_mcp_stdio_config":{
                "team_id":"team-42",
                "port":54321,
                "token":"tok-abc",
                "slot_id":"slot-lead",
                "binary_path":"/bin/backend"
            }
        }"#;
        let parsed: AcpBuildExtra = serde_json::from_str(with_cfg).unwrap();
        let cfg = parsed.team_mcp_stdio_config.expect("config present");
        assert_eq!(cfg.team_id, "team-42");
        assert_eq!(cfg.port, 54321);
        assert_eq!(cfg.token, "tok-abc");
        assert_eq!(cfg.slot_id, "slot-lead");
    }

    #[test]
    fn send_message_data_serde_roundtrip() {
        let data = SendMessageData {
            content: "Hello".into(),
            msg_id: "msg-001".into(),
            files: vec!["/tmp/a.txt".into()],
            inject_skills: vec!["review".into()],
        };
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(json["content"], "Hello");
        assert_eq!(json["msg_id"], "msg-001");
        assert_eq!(json["files"], json!(["/tmp/a.txt"]));
        assert_eq!(json["inject_skills"], json!(["review"]));

        let parsed: SendMessageData = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.content, "Hello");
        assert_eq!(parsed.msg_id, "msg-001");
    }

    #[test]
    fn send_message_data_defaults_optional_fields() {
        let json = json!({ "content": "Hi", "msg_id": "m1" });
        let data: SendMessageData = serde_json::from_value(json).unwrap();
        assert!(data.files.is_empty());
        assert!(data.inject_skills.is_empty());
    }

    #[test]
    fn build_task_options_serde() {
        let opts = BuildTaskOptions {
            agent_type: AgentType::Acp,
            workspace: "/project".into(),
            model: ProviderWithModel {
                provider_id: "p1".into(),
                model: "claude-sonnet".into(),
                use_model: None,
            },
            conversation_id: "conv-1".into(),
            extra: json!({ "backend": "claude" }),
        };
        let json = serde_json::to_value(&opts).unwrap();
        assert_eq!(json["agent_type"], "acp");
        assert_eq!(json["workspace"], "/project");
        assert_eq!(json["conversation_id"], "conv-1");
    }

    #[test]
    fn acp_model_info_serde() {
        let info = AcpModelInfo {
            model_id: "claude-sonnet-4".into(),
            model_name: Some("Claude Sonnet 4".into()),
            provider: Some("anthropic".into()),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["model_id"], "claude-sonnet-4");
        assert_eq!(json["model_name"], "Claude Sonnet 4");
    }

    #[test]
    fn acp_session_config_option_serde() {
        let opt = AcpSessionConfigOption {
            config_id: "theme".into(),
            label: "Theme".into(),
            value: "dark".into(),
            options: Some(vec!["light".into(), "dark".into()]),
        };
        let json = serde_json::to_value(&opt).unwrap();
        assert_eq!(json["config_id"], "theme");
        assert_eq!(json["options"], json!(["light", "dark"]));
    }

    #[test]
    fn slash_command_item_serde() {
        let cmd = SlashCommandItem {
            command: "/review".into(),
            description: "Code review".into(),
        };
        let json = serde_json::to_value(&cmd).unwrap();
        assert_eq!(json["command"], "/review");
    }

    #[test]
    fn openclaw_gateway_config_defaults() {
        let json = json!({});
        let config: OpenClawGatewayConfig = serde_json::from_value(json).unwrap();
        assert!(!config.use_external_gateway);
        assert!(config.host.is_none());
        assert!(config.port.is_none());
    }

    #[test]
    fn aionrs_build_extra_serde_defaults() {
        let json = json!({});
        let extra: AionrsBuildExtra = serde_json::from_value(json).unwrap();
        assert!(extra.system_prompt.is_none());
        assert_eq!(extra.max_tokens, 8192);
        assert!(extra.max_turns.is_none());
    }

    #[test]
    fn aionrs_build_extra_serde_with_overrides() {
        let json = json!({
            "system_prompt": "You are a helpful assistant.",
            "max_tokens": 4096,
            "max_turns": 10
        });
        let extra: AionrsBuildExtra = serde_json::from_value(json).unwrap();
        assert_eq!(extra.system_prompt.unwrap(), "You are a helpful assistant.");
        assert_eq!(extra.max_tokens, 4096);
        assert_eq!(extra.max_turns.unwrap(), 10);
    }
}
