use serde::{Deserialize, Serialize};

use crate::id::fnv1a_hex8;

/// Type of AI agent backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    Acp,
    #[serde(rename = "openclaw-gateway")]
    OpenclawGateway,
    Nanobot,
    Remote,
    Aionrs,
    /// Legacy Gemini conversations. Kept solely so that historical rows
    /// with `type='gemini'` remain readable in the conversation list and
    /// message history. Any attempt to run the agent (send a message,
    /// resume a session) returns an error — this variant has no factory
    /// branch. New Gemini conversations use `AgentType::Acp` with
    /// `backend='gemini'`.
    Gemini,
}

impl AgentType {
    pub fn display_name(&self) -> &'static str {
        match self {
            AgentType::Acp => "ACP",
            AgentType::OpenclawGateway => "OpenClaw Gateway",
            AgentType::Nanobot => "Nanobot",
            AgentType::Remote => "Remote",
            AgentType::Aionrs => "Aion CLI",
            AgentType::Gemini => "Gemini (legacy)",
        }
    }

    pub fn serde_name(&self) -> &'static str {
        match self {
            AgentType::Acp => "acp",
            AgentType::OpenclawGateway => "openclaw-gateway",
            AgentType::Nanobot => "nanobot",
            AgentType::Remote => "remote",
            AgentType::Aionrs => "aionrs",
            AgentType::Gemini => "gemini",
        }
    }

    pub fn id(&self) -> String {
        let hash = fnv1a_hex8(self.serde_name().as_bytes());
        // SAFETY: fnv1a_hex8 only produces ASCII hex digits
        unsafe { std::str::from_utf8_unchecked(&hash) }.into()
    }

    /// Native skill-discovery directories for non-ACP agent types.
    ///
    /// ACP backends expose their own skill dirs via
    /// [`AcpBackend::native_skills_dirs`]; this method covers the few
    /// non-ACP agent types that still support native skill discovery.
    /// Returns `None` for agent types that require prompt-injection
    /// instead of workspace symlinks.
    ///
    /// `AgentType::Gemini` is intentionally absent: new Gemini
    /// conversations use `AgentType::Acp` with `backend = gemini`, so
    /// their skill dirs come from `AcpBackend::Gemini.native_skills_dirs()`.
    /// Historical `AgentType::Gemini` rows cannot start a new runtime
    /// (see the variant's doc comment) and therefore never reach this
    /// path during workspace provisioning.
    ///
    /// Mirrors the `NON_ACP_SKILLS_DIRS` table in
    /// `src/common/types/acpTypes.ts`.
    pub fn native_skills_dirs(&self) -> Option<&'static [&'static str]> {
        match self {
            AgentType::Aionrs => Some(&[".aionrs/skills"]),
            AgentType::Acp
            | AgentType::OpenclawGateway
            | AgentType::Nanobot
            | AgentType::Remote
            | AgentType::Gemini => None,
        }
    }

    /// Canonical full-auto session mode id for this agent type.
    ///
    /// ACP agents need backend-specific mode ids, while other agent types
    /// currently converge on the permissive `yolo` mode.
    pub fn full_auto_mode_id(&self, backend: Option<AcpBackend>) -> &'static str {
        match self {
            AgentType::Acp => backend
                .map(|backend| backend.full_auto_mode_id())
                .unwrap_or("yolo"),
            AgentType::Aionrs
            | AgentType::Gemini
            | AgentType::OpenclawGateway
            | AgentType::Nanobot
            | AgentType::Remote => "yolo",
        }
    }
}

/// ACP sub-backend identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcpBackend {
    Claude,
    Gemini,
    Qwen,
    Codex,
    Codebuddy,
    Droid,
    Goose,
    Auggie,
    Kimi,
    Opencode,
    Copilot,
    Qoder,
    Vibe,
    Cursor,
    Kiro,
    Hermes,
    Snow,
}

impl AcpBackend {
    /// All backends that have a detectable CLI binary.
    pub const CLI_BACKENDS: &[AcpBackend] = &[
        AcpBackend::Claude,
        AcpBackend::Gemini,
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
        AcpBackend::Hermes,
        AcpBackend::Snow,
    ];

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
            AcpBackend::Codex => "Codex",
            AcpBackend::Codebuddy => "CodeBuddy",
            AcpBackend::Droid => "Droid",
            AcpBackend::Goose => "Goose",
            AcpBackend::Auggie => "Auggie",
            AcpBackend::Kimi => "Kimi",
            AcpBackend::Opencode => "OpenCode",
            AcpBackend::Copilot => "Copilot",
            AcpBackend::Qoder => "Qoder",
            AcpBackend::Vibe => "Vibe",
            AcpBackend::Cursor => "Cursor",
            AcpBackend::Kiro => "Kiro",
            AcpBackend::Hermes => "Hermes",
            AcpBackend::Snow => "Snow",
        }
    }

    /// Returns the name of the CLI binary for this backend, if it has one.
    pub fn binary_name(&self) -> Option<&'static str> {
        match self {
            AcpBackend::Claude => Some("claude"),
            AcpBackend::Gemini => Some("gemini"),
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
            AcpBackend::Hermes => Some("hermes"),
            AcpBackend::Snow => Some("snow"),
        }
    }

    /// CLI arguments for direct-CLI backends (no bridge).
    ///
    /// These are appended after the CLI command itself.
    /// Returns `None` for bridge-based backends (use [`bridge_package`] instead)
    /// and for backends that don't have a standalone CLI.
    pub fn args(&self) -> Option<&'static [&'static str]> {
        match self {
            // Bridge-based — args handled by bridge_package + bridge_extra_args
            AcpBackend::Claude | AcpBackend::Codex | AcpBackend::Codebuddy => None,
            // Direct CLI with specific ACP args
            AcpBackend::Gemini => Some(&["--experimental-acp"]),
            AcpBackend::Goose => Some(&["acp"]),
            AcpBackend::Droid => Some(&["exec", "--output-format", "acp"]),
            AcpBackend::Auggie => Some(&["--acp"]),
            AcpBackend::Kimi => Some(&["acp"]),
            AcpBackend::Opencode => Some(&["acp"]),
            AcpBackend::Copilot => Some(&["--acp", "--stdio"]),
            AcpBackend::Qoder => Some(&["--acp"]),
            AcpBackend::Vibe => Some(&[]),
            AcpBackend::Cursor => Some(&["acp"]),
            AcpBackend::Kiro => Some(&["acp"]),
            AcpBackend::Hermes => Some(&["acp"]),
            AcpBackend::Snow => Some(&["--acp"]),
            AcpBackend::Qwen => Some(&["--acp"]),
        }
    }

    /// ACP bridge package for backends that require an NPX/bun bridge.
    ///
    /// Returns `None` for backends whose native CLI speaks ACP directly.
    pub fn bridge_package(&self) -> Option<&'static str> {
        match self {
            AcpBackend::Claude => Some("@agentclientprotocol/claude-agent-acp@0.29.2"),
            AcpBackend::Codex => Some("@zed-industries/codex-acp@0.9.5"),
            AcpBackend::Codebuddy => Some("@tencent-ai/codebuddy-code@2.73.0"),
            _ => None,
        }
    }

    /// Extra arguments appended when spawning via bridge package.
    ///
    /// Only relevant when [`bridge_package`](Self::bridge_package) returns `Some`.
    pub fn bridge_extra_args(&self) -> &'static [&'static str] {
        match self {
            AcpBackend::Codebuddy => &["--acp"],
            _ => &[],
        }
    }

    /// Native workspace skill directories the CLI reads on its own,
    /// without backend prompt-side injection.
    ///
    /// Returns `Some(&[".xxx/skills"])` for backends that support native
    /// skill discovery via workspace symlinks, and `None` for backends
    /// that must receive skills through prompt injection.
    ///
    /// Mirrors `src/common/types/acpTypes.ts` `ACP_BACKENDS_ALL` `skillsDirs`
    /// entries. Keep in sync when adding or modifying backends.
    pub fn native_skills_dirs(&self) -> Option<&'static [&'static str]> {
        match self {
            AcpBackend::Claude => Some(&[".claude/skills"]),
            AcpBackend::Gemini => Some(&[".gemini/skills"]),
            AcpBackend::Qwen => Some(&[".qwen/skills"]),
            AcpBackend::Codex => Some(&[".codex/skills"]),
            AcpBackend::Codebuddy => Some(&[".codebuddy/skills"]),
            AcpBackend::Goose => Some(&[".goose/skills"]),
            AcpBackend::Kimi => Some(&[".kimi/skills"]),
            AcpBackend::Opencode => Some(&[".opencode/skills"]),
            AcpBackend::Droid => Some(&[".factory/skills"]),
            AcpBackend::Vibe => Some(&[".vibe/skills"]),
            AcpBackend::Cursor => Some(&[".cursor/skills"]),
            // Explicitly no native skill discovery:
            AcpBackend::Copilot
            | AcpBackend::Qoder
            | AcpBackend::Kiro
            | AcpBackend::Hermes
            | AcpBackend::Snow
            | AcpBackend::Auggie => None,
        }
    }

    /// Canonical full-auto session mode id for this ACP backend.
    ///
    /// This mirrors the frontend `getFullAutoMode()` mapping and is used by
    /// backend flows that need to materialize a new cron job from an existing
    /// conversation without depending on a live ACP runtime snapshot.
    pub fn full_auto_mode_id(&self) -> &'static str {
        match self {
            AcpBackend::Claude => "bypassPermissions",
            AcpBackend::Codex => "full-access",
            AcpBackend::Opencode => "build",
            AcpBackend::Cursor => "agent",
            _ => "yolo",
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
    Permission,
    AcpToolCall,
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
    /// Team session is rebuilding the agent process to inject a fresh
    /// `team_mcp_stdio_config`. The conversation is preserved; only the
    /// in-memory ACP CLI is recycled.
    TeamMcpRebuild,
    /// Team is being deleted; every agent process under it must be torn
    /// down before the team's conversations / rows are removed.
    TeamDeleted,
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
    fn claude_supports_native_skills() {
        assert_eq!(
            AcpBackend::Claude.native_skills_dirs(),
            Some(&[".claude/skills"][..])
        );
    }

    #[test]
    fn copilot_has_no_native_skills() {
        assert_eq!(AcpBackend::Copilot.native_skills_dirs(), None);
    }

    #[test]
    fn qoder_kiro_hermes_snow_have_no_native_skills() {
        assert_eq!(AcpBackend::Qoder.native_skills_dirs(), None);
        assert_eq!(AcpBackend::Kiro.native_skills_dirs(), None);
        assert_eq!(AcpBackend::Hermes.native_skills_dirs(), None);
        assert_eq!(AcpBackend::Snow.native_skills_dirs(), None);
    }

    #[test]
    fn all_backends_with_skills_dirs() {
        // Mirrors src/common/types/acpTypes.ts ACP_BACKENDS_ALL skillsDirs entries.
        let expected_with_skills: &[AcpBackend] = &[
            AcpBackend::Claude,
            AcpBackend::Qwen,
            AcpBackend::Codex,
            AcpBackend::Codebuddy,
            AcpBackend::Goose,
            AcpBackend::Kimi,
            AcpBackend::Opencode,
            AcpBackend::Droid,
            AcpBackend::Vibe,
            AcpBackend::Cursor,
        ];
        for b in expected_with_skills {
            assert!(
                b.native_skills_dirs().is_some(),
                "{b:?} should expose native_skills_dirs"
            );
        }
    }

    #[test]
    fn test_agent_type_display_names() {
        assert_eq!(
            AgentType::OpenclawGateway.display_name(),
            "OpenClaw Gateway"
        );
        assert_eq!(AgentType::Aionrs.display_name(), "Aion CLI");
        assert_eq!(AgentType::Nanobot.display_name(), "Nanobot");
        assert_eq!(AgentType::Remote.display_name(), "Remote");
        assert_eq!(AgentType::Acp.display_name(), "ACP");
    }

    #[test]
    fn test_agent_type_id_stability() {
        let id = AgentType::Aionrs.id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(AgentType::Aionrs.id(), AgentType::Aionrs.id());
    }

    #[test]
    fn test_agent_type_id_unique_per_variant() {
        let ids: Vec<String> = [
            AgentType::Acp,
            AgentType::OpenclawGateway,
            AgentType::Nanobot,
            AgentType::Remote,
            AgentType::Aionrs,
        ]
        .iter()
        .map(|t| t.id())
        .collect();
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }

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
    fn test_acp_backend_lowercase_variants() {
        let cases = [
            (AcpBackend::Claude, "claude"),
            (AcpBackend::Codebuddy, "codebuddy"),
            (AcpBackend::Opencode, "opencode"),
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
    fn acp_backend_rejects_non_acp_engine_names() {
        // Non-ACP execution engines are dispatched via AgentType, not AcpBackend.
        // Rejecting them at the HTTP deserialization boundary prevents accidental
        // regression where a future change re-adds one of these variants.
        //
        // Note: "gemini" is intentionally NOT in this list — it is a valid
        // AcpBackend variant (spawned via `gemini --experimental-acp`).
        for name in ["nanobot", "remote", "aionrs", "openclaw-gateway"] {
            let json = format!("\"{name}\"");
            let result: Result<AcpBackend, _> = serde_json::from_str(&json);
            assert!(result.is_err(), "AcpBackend should not accept {name:?}");
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
        assert_eq!(AcpBackend::Claude.binary_name(), Some("claude"));
        assert_eq!(AcpBackend::Qwen.binary_name(), Some("qwen"));
        assert_eq!(AcpBackend::Codex.binary_name(), Some("codex"));
        assert_eq!(AcpBackend::Kiro.binary_name(), Some("kiro"));
        assert_eq!(AcpBackend::Goose.binary_name(), Some("goose"));
        assert_eq!(AcpBackend::Cursor.binary_name(), Some("cursor"));
        assert_eq!(AcpBackend::Snow.binary_name(), Some("snow"));
    }

    #[test]
    fn test_acp_backend_display_name() {
        assert_eq!(AcpBackend::Claude.display_name(), "Claude");
        assert_eq!(AcpBackend::Codebuddy.display_name(), "CodeBuddy");
        assert_eq!(AcpBackend::Opencode.display_name(), "OpenCode");
    }

    #[test]
    fn test_acp_backend_cli_backends_only_contains_some() {
        for backend in AcpBackend::CLI_BACKENDS {
            assert!(
                backend.binary_name().is_some(),
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

    #[test]
    fn acp_gemini_is_registered_as_cli_backend() {
        assert!(AcpBackend::CLI_BACKENDS.contains(&AcpBackend::Gemini));
        assert_eq!(AcpBackend::Gemini.binary_name(), Some("gemini"));
        assert_eq!(AcpBackend::Gemini.args(), Some(&["--experimental-acp"][..]));
        // Gemini is a direct-CLI backend, no bridge
        assert_eq!(AcpBackend::Gemini.bridge_package(), None);
    }

    #[test]
    fn acp_backend_full_auto_mode_id_matches_backend_capabilities() {
        assert_eq!(AcpBackend::Claude.full_auto_mode_id(), "bypassPermissions");
        assert_eq!(AcpBackend::Codex.full_auto_mode_id(), "full-access");
        assert_eq!(AcpBackend::Opencode.full_auto_mode_id(), "build");
        assert_eq!(AcpBackend::Cursor.full_auto_mode_id(), "agent");
        assert_eq!(AcpBackend::Gemini.full_auto_mode_id(), "yolo");
        assert_eq!(AcpBackend::Qwen.full_auto_mode_id(), "yolo");
    }

    #[test]
    fn agent_type_full_auto_mode_id_supports_non_acp_agents() {
        assert_eq!(
            AgentType::Acp.full_auto_mode_id(Some(AcpBackend::Codex)),
            "full-access"
        );
        assert_eq!(AgentType::Aionrs.full_auto_mode_id(None), "yolo");
        assert_eq!(AgentType::Remote.full_auto_mode_id(None), "yolo");
    }
}
