use serde::{Deserialize, Serialize};

use crate::id::fnv1a_hex8;

/// Type of AI agent backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    Gemini,
    Acp,
    #[serde(rename = "openclaw-gateway")]
    OpenclawGateway,
    Nanobot,
    Remote,
    Aionrs,
}

/// ACP sub-backend identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcpBackend {
    Claude,
    Gemini,
    Qwen,
    #[serde(rename = "iFlow")]
    IFlow,
    Codex,
    Codebuddy,
    Droid,
    Goose,
    Auggie,
    Kimi,
    Opencode,
    Copilot,
    Qoder,
    #[serde(rename = "openclaw-gateway")]
    OpenclawGateway,
    Vibe,
    Nanobot,
    Cursor,
    Kiro,
    Hermes,
    Snow,
    Remote,
    Aionrs,
    Custom,
}

impl AcpBackend {
    /// All backends that have a detectable CLI binary.
    pub const CLI_BACKENDS: &[AcpBackend] = &[
        AcpBackend::Claude,
        AcpBackend::Qwen,
        AcpBackend::Codex,
        AcpBackend::Codebuddy,
        AcpBackend::Kiro,
        AcpBackend::Opencode,
        AcpBackend::Copilot,
        AcpBackend::Goose,
        AcpBackend::Cursor,
        AcpBackend::Droid,
        AcpBackend::Auggie,
        AcpBackend::Kimi,
        AcpBackend::Qoder,
        AcpBackend::Vibe,
        AcpBackend::Nanobot,
        AcpBackend::Hermes,
        AcpBackend::Snow,
    ];

    pub fn cli_binary_name(&self) -> Option<&'static str> {
        match self {
            AcpBackend::Claude => Some("claude"),
            AcpBackend::Qwen => Some("qwen"),
            AcpBackend::Codex => Some("codex"),
            AcpBackend::Codebuddy => Some("codebuddy"),
            AcpBackend::Kiro => Some("kiro"),
            AcpBackend::Opencode => Some("opencode"),
            AcpBackend::Copilot => Some("copilot"),
            AcpBackend::Goose => Some("goose"),
            AcpBackend::Cursor => Some("cursor"),
            AcpBackend::Droid => Some("droid"),
            AcpBackend::Auggie => Some("auggie"),
            AcpBackend::Kimi => Some("kimi"),
            AcpBackend::Qoder => Some("qoder"),
            AcpBackend::Vibe => Some("vibe"),
            AcpBackend::Nanobot => Some("nanobot"),
            AcpBackend::Hermes => Some("hermes"),
            AcpBackend::Snow => Some("snow"),
            AcpBackend::IFlow
            | AcpBackend::Gemini
            | AcpBackend::OpenclawGateway
            | AcpBackend::Remote
            | AcpBackend::Aionrs
            | AcpBackend::Custom => None,
        }
    }

    pub fn id(&self) -> String {
        let hash = fnv1a_hex8(self.display_name().as_bytes());
        // SAFETY: fnv1a_hex8 only produces ASCII hex digits
        unsafe { std::str::from_utf8_unchecked(&hash) }.into()
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            AcpBackend::Claude => "Claude",
            AcpBackend::Gemini => "Gemini",
            AcpBackend::Qwen => "Qwen",
            AcpBackend::IFlow => "iFlow",
            AcpBackend::Codex => "Codex",
            AcpBackend::Codebuddy => "CodeBuddy",
            AcpBackend::Droid => "Droid",
            AcpBackend::Goose => "Goose",
            AcpBackend::Auggie => "Auggie",
            AcpBackend::Kimi => "Kimi",
            AcpBackend::Opencode => "OpenCode",
            AcpBackend::Copilot => "Copilot",
            AcpBackend::Qoder => "Qoder",
            AcpBackend::OpenclawGateway => "OpenClaw Gateway",
            AcpBackend::Vibe => "Vibe",
            AcpBackend::Nanobot => "Nanobot",
            AcpBackend::Cursor => "Cursor",
            AcpBackend::Kiro => "Kiro",
            AcpBackend::Hermes => "Hermes",
            AcpBackend::Snow => "Snow",
            AcpBackend::Remote => "Remote",
            AcpBackend::Aionrs => "Aionrs",
            AcpBackend::Custom => "Custom",
        }
    }
}

/// Runtime status of a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationStatus {
    Pending,
    Running,
    Finished,
}

/// Origin of a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationSource {
    Aionui,
    Telegram,
    Lark,
    Dingtalk,
    Weixin,
}

/// Type discriminant for messages in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Text,
    Tips,
    ToolCall,
    ToolGroup,
    AgentStatus,
    AcpPermission,
    AcpToolCall,
    CodexPermission,
    CodexToolCall,
    Plan,
    Thinking,
    AvailableCommands,
    SkillSuggest,
    CronTrigger,
}

/// Display position of a message in the chat UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessagePosition {
    Right,
    Left,
    Center,
    Pop,
}

/// Processing status of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    Finish,
    Pending,
    Error,
    Work,
}

/// LLM API protocol type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolType {
    #[serde(rename = "openai")]
    OpenAI,
    Anthropic,
    Gemini,
    Unknown,
}

/// Remote Agent protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteAgentProtocol {
    OpenClaw,
    ZeroClaw,
    Acp,
}

/// Remote Agent authentication method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteAgentAuthType {
    Bearer,
    Password,
    None,
}

/// Remote Agent connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteAgentStatus {
    Unknown,
    Connected,
    Pending,
    Error,
}

/// Reason for terminating an Agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKillReason {
    IdleTimeout,
}

/// Preview content type for document preview history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewContentType {
    Markdown,
    Diff,
    Code,
    Html,
    Pdf,
    Ppt,
    Word,
    Excel,
    Image,
    Url,
}

/// File change operation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileChangeOperation {
    Create,
    Modify,
    Delete,
}

/// AI Agent CLI source identifier for MCP configuration sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpSource {
    Claude,
    Gemini,
    Qwen,
    #[serde(rename = "iflow")]
    IFlow,
    Codex,
    #[serde(rename = "codebuddy")]
    CodeBuddy,
    #[serde(rename = "opencode")]
    OpenCode,
    Aionrs,
    Nanobot,
    Aionui,
}

/// MCP server connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpServerStatus {
    Connected,
    Disconnected,
    Error,
    Testing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_type_serde_roundtrip() {
        let val = AgentType::OpenclawGateway;
        let json = serde_json::to_string(&val).unwrap();
        assert_eq!(json, r#""openclaw-gateway""#);
        let parsed: AgentType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, val);
    }

    #[test]
    fn test_agent_type_all_variants() {
        let cases = [
            (AgentType::Gemini, "gemini"),
            (AgentType::Acp, "acp"),
            (AgentType::OpenclawGateway, "openclaw-gateway"),
            (AgentType::Nanobot, "nanobot"),
            (AgentType::Remote, "remote"),
            (AgentType::Aionrs, "aionrs"),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, format!("\"{expected}\""), "serialize {variant:?}");
            let parsed: AgentType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant, "deserialize {expected}");
        }
    }

    #[test]
    fn test_acp_backend_iflow() {
        let val = AcpBackend::IFlow;
        let json = serde_json::to_string(&val).unwrap();
        assert_eq!(json, r#""iFlow""#);
    }

    #[test]
    fn test_acp_backend_lowercase_variants() {
        let cases = [
            (AcpBackend::Claude, "claude"),
            (AcpBackend::Codebuddy, "codebuddy"),
            (AcpBackend::Opencode, "opencode"),
            (AcpBackend::OpenclawGateway, "openclaw-gateway"),
            (AcpBackend::Hermes, "hermes"),
            (AcpBackend::Snow, "snow"),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, format!("\"{expected}\""), "serialize {variant:?}");
            let parsed: AcpBackend = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant, "deserialize {expected}");
        }
    }

    #[test]
    fn test_protocol_type_openai() {
        let val = ProtocolType::OpenAI;
        let json = serde_json::to_string(&val).unwrap();
        assert_eq!(json, r#""openai""#);
        let parsed: ProtocolType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ProtocolType::OpenAI);
    }

    #[test]
    fn test_conversation_status_lowercase() {
        let val = ConversationStatus::Pending;
        let json = serde_json::to_string(&val).unwrap();
        assert_eq!(json, r#""pending""#);
    }

    #[test]
    fn test_message_type_snake_case() {
        let val = MessageType::ToolCall;
        let json = serde_json::to_string(&val).unwrap();
        assert_eq!(json, r#""tool_call""#);

        let val = MessageType::AcpToolCall;
        let json = serde_json::to_string(&val).unwrap();
        assert_eq!(json, r#""acp_tool_call""#);

        let val = MessageType::AgentStatus;
        let json = serde_json::to_string(&val).unwrap();
        assert_eq!(json, r#""agent_status""#);
    }

    #[test]
    fn test_file_change_operation_roundtrip() {
        for op in [
            FileChangeOperation::Create,
            FileChangeOperation::Modify,
            FileChangeOperation::Delete,
        ] {
            let json = serde_json::to_string(&op).unwrap();
            let parsed: FileChangeOperation = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, op);
        }
    }

    #[test]
    fn test_mcp_source_serde_roundtrip() {
        let cases = [
            (McpSource::Claude, r#""claude""#),
            (McpSource::Gemini, r#""gemini""#),
            (McpSource::Qwen, r#""qwen""#),
            (McpSource::IFlow, r#""iflow""#),
            (McpSource::Codex, r#""codex""#),
            (McpSource::CodeBuddy, r#""codebuddy""#),
            (McpSource::OpenCode, r#""opencode""#),
            (McpSource::Aionrs, r#""aionrs""#),
            (McpSource::Nanobot, r#""nanobot""#),
            (McpSource::Aionui, r#""aionui""#),
        ];
        for (variant, expected_json) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json, "serialize {variant:?}");
            let parsed: McpSource = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant, "deserialize {expected_json}");
        }
    }

    #[test]
    fn test_mcp_server_status_serde_roundtrip() {
        let cases = [
            (McpServerStatus::Connected, r#""connected""#),
            (McpServerStatus::Disconnected, r#""disconnected""#),
            (McpServerStatus::Error, r#""error""#),
            (McpServerStatus::Testing, r#""testing""#),
        ];
        for (variant, expected_json) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json, "serialize {variant:?}");
            let parsed: McpServerStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant, "deserialize {expected_json}");
        }
    }

    #[test]
    fn test_acp_backend_cli_binary_name_known() {
        assert_eq!(AcpBackend::Claude.cli_binary_name(), Some("claude"));
        assert_eq!(AcpBackend::Qwen.cli_binary_name(), Some("qwen"));
        assert_eq!(AcpBackend::Codex.cli_binary_name(), Some("codex"));
        assert_eq!(AcpBackend::Kiro.cli_binary_name(), Some("kiro"));
        assert_eq!(AcpBackend::Goose.cli_binary_name(), Some("goose"));
        assert_eq!(AcpBackend::Cursor.cli_binary_name(), Some("cursor"));
        assert_eq!(AcpBackend::Snow.cli_binary_name(), Some("snow"));
    }

    #[test]
    fn test_acp_backend_cli_binary_name_none() {
        assert_eq!(AcpBackend::IFlow.cli_binary_name(), None);
        assert_eq!(AcpBackend::Gemini.cli_binary_name(), None);
        assert_eq!(AcpBackend::OpenclawGateway.cli_binary_name(), None);
        assert_eq!(AcpBackend::Remote.cli_binary_name(), None);
        assert_eq!(AcpBackend::Aionrs.cli_binary_name(), None);
        assert_eq!(AcpBackend::Custom.cli_binary_name(), None);
    }

    #[test]
    fn test_acp_backend_display_name() {
        assert_eq!(AcpBackend::Claude.display_name(), "Claude");
        assert_eq!(AcpBackend::IFlow.display_name(), "iFlow");
        assert_eq!(AcpBackend::Codebuddy.display_name(), "CodeBuddy");
        assert_eq!(AcpBackend::Opencode.display_name(), "OpenCode");
        assert_eq!(
            AcpBackend::OpenclawGateway.display_name(),
            "OpenClaw Gateway"
        );
    }

    #[test]
    fn test_acp_backend_cli_backends_only_contains_some() {
        for backend in AcpBackend::CLI_BACKENDS {
            assert!(
                backend.cli_binary_name().is_some(),
                "{backend:?} is in CLI_BACKENDS but cli_binary_name() returns None"
            );
        }
    }

    #[test]
    fn test_acp_backend_id_deterministic() {
        let a = AcpBackend::Claude.id();
        let b = AcpBackend::Claude.id();
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
    }

    #[test]
    fn test_acp_backend_id_unique_per_variant() {
        let claude_id = AcpBackend::Claude.id();
        let codex_id = AcpBackend::Codex.id();
        assert_ne!(claude_id, codex_id);
    }
}
