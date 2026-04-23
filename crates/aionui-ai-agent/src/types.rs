use serde::{Deserialize, Serialize};

use aionui_common::{AcpBackend, AgentType, ProviderWithModel};

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

/// ACP-specific fields extracted from `extra` in [`BuildTaskOptions`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpBuildExtra {
    /// Agent registry ID. When provided, `backend`/`cli_path` are resolved
    /// from the registry and need not be supplied by the caller.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// ACP sub-backend identifier.
    #[serde(default)]
    pub backend: Option<AcpBackend>,
    /// Path to the CLI executable (resolved from registry when `agent_id` is set).
    #[serde(default)]
    pub cli_path: Option<String>,
    /// Whether the user picked a custom workspace path.
    #[serde(default)]
    pub custom_workspace: bool,
    /// Agent name within the ACP backend.
    #[serde(default)]
    pub agent_name: Option<String>,
    /// Custom agent ID (for user-defined agents).
    #[serde(default)]
    pub custom_agent_id: Option<String>,
    /// Preset context to inject.
    #[serde(default)]
    pub preset_context: Option<String>,
    /// Skills to enable for this session.
    #[serde(default)]
    pub enabled_skills: Vec<String>,
    /// Preset assistant ID.
    #[serde(default)]
    pub preset_assistant_id: Option<String>,
    /// Session mode override.
    #[serde(default)]
    pub session_mode: Option<String>,
    /// Associated cron job ID.
    #[serde(default)]
    pub cron_job_id: Option<String>,
}

/// Gemini-specific fields extracted from `extra` in [`BuildTaskOptions`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiBuildExtra {
    /// Whether the user picked a custom workspace path.
    #[serde(default)]
    pub custom_workspace: bool,
    /// Web search engine preference.
    #[serde(default)]
    pub web_search_engine: Option<String>,
    /// Context file name.
    #[serde(default)]
    pub context_file_name: Option<String>,
    /// Context content to inject.
    #[serde(default)]
    pub context_content: Option<String>,
    /// Preset rules.
    #[serde(default)]
    pub preset_rules: Option<String>,
    /// Skills to enable.
    #[serde(default)]
    pub enabled_skills: Vec<String>,
    /// Extra skill paths.
    #[serde(default)]
    pub extra_skill_paths: Vec<String>,
    /// Built-in skills to exclude.
    #[serde(default)]
    pub exclude_builtin_skills: Vec<String>,
    /// Preset assistant ID.
    #[serde(default)]
    pub preset_assistant_id: Option<String>,
    /// Session mode override.
    #[serde(default)]
    pub session_mode: Option<String>,
    /// Associated cron job ID.
    #[serde(default)]
    pub cron_job_id: Option<String>,
}

/// OpenClaw gateway configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenClawGatewayConfig {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub token: Option<String>,
    pub password: Option<String>,
    #[serde(default)]
    pub use_external_gateway: bool,
    pub cli_path: Option<String>,
}

/// OpenClaw-specific fields extracted from `extra` in [`BuildTaskOptions`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenClawBuildExtra {
    /// ACP sub-backend identifier.
    pub backend: AcpBackend,
    /// Agent name.
    #[serde(default)]
    pub agent_name: Option<String>,
    /// Whether the user picked a custom workspace path.
    #[serde(default)]
    pub custom_workspace: bool,
    /// OpenClaw gateway configuration.
    pub gateway: OpenClawGatewayConfig,
    /// Skills to enable.
    #[serde(default)]
    pub enabled_skills: Vec<String>,
    /// Preset assistant ID.
    #[serde(default)]
    pub preset_assistant_id: Option<String>,
    /// Associated cron job ID.
    #[serde(default)]
    pub cron_job_id: Option<String>,
}

/// Remote agent-specific fields extracted from `extra` in [`BuildTaskOptions`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteBuildExtra {
    /// Remote agent configuration ID.
    pub remote_agent_id: String,
}

/// Aionrs-specific fields extracted from `extra` in [`BuildTaskOptions`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AionrsBuildExtra {
    /// LLM provider name (anthropic, openai, bedrock, vertex).
    pub provider: String,
    /// API key for the provider.
    pub api_key: String,
    /// Model identifier.
    pub model: String,
    /// Provider base URL override.
    #[serde(default)]
    pub base_url: Option<String>,
    /// System prompt override.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Max tokens per response.
    #[serde(default = "default_aionrs_max_tokens")]
    pub max_tokens: u32,
    /// Max agentic turns.
    #[serde(default)]
    pub max_turns: Option<usize>,
}

fn default_aionrs_max_tokens() -> u32 {
    8192
}

/// ACP model information returned by the ACP backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpModelInfo {
    pub model_id: String,
    pub model_name: Option<String>,
    pub provider: Option<String>,
}

/// ACP session configuration option.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpSessionConfigOption {
    pub config_id: String,
    pub label: String,
    pub value: String,
    /// Possible values; `None` means free-form input.
    pub options: Option<Vec<String>>,
}

/// A slash command item available in a conversation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashCommandItem {
    pub command: String,
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn aionrs_build_extra_serde_minimal() {
        let json = json!({
            "provider": "anthropic",
            "api_key": "sk-test",
            "model": "claude-sonnet-4-20250514"
        });
        let extra: AionrsBuildExtra = serde_json::from_value(json).unwrap();
        assert_eq!(extra.provider, "anthropic");
        assert_eq!(extra.api_key, "sk-test");
        assert_eq!(extra.model, "claude-sonnet-4-20250514");
        assert!(extra.base_url.is_none());
        assert!(extra.system_prompt.is_none());
        assert_eq!(extra.max_tokens, 8192);
        assert!(extra.max_turns.is_none());
    }

    #[test]
    fn aionrs_build_extra_serde_full() {
        let json = json!({
            "provider": "openai",
            "api_key": "sk-openai",
            "model": "gpt-4o",
            "base_url": "https://api.openai.com/v1",
            "system_prompt": "You are a helpful assistant.",
            "max_tokens": 4096,
            "max_turns": 10
        });
        let extra: AionrsBuildExtra = serde_json::from_value(json).unwrap();
        assert_eq!(extra.provider, "openai");
        assert_eq!(extra.base_url.unwrap(), "https://api.openai.com/v1");
        assert_eq!(extra.max_tokens, 4096);
        assert_eq!(extra.max_turns.unwrap(), 10);
    }
}
