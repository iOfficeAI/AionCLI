//! Stdio bridge descriptors consumed by ACP `session/new.mcp_servers`.
//!
//! Flow: `TeamSessionService::ensure_session` builds a `TeamMcpStdioServerSpec`
//! per agent and writes its config triple into `conversation.extra`. When
//! the ACP session is created, the spec is converted via `into_sdk()` to the
//! wire-level `agent_client_protocol::schema::McpServer::Stdio` variant and
//! sent to the agent CLI, which then spawns `<backend> mcp-bridge` with the
//! three `TEAM_MCP_*` env keys so it can proxy stdio↔TCP to the in-process
//! team MCP server.

use std::path::PathBuf;

use agent_client_protocol::schema::{EnvVariable, McpServer, McpServerStdio};

pub use aionui_api_types::TeamMcpStdioConfig;

/// Stdio MCP server description ready to be handed to `session/new`.
///
/// Field shapes are fixed by [phase1 interface-contracts.md §3]:
/// - `name` = `"aionui-team-<team_id>"`
/// - `command` = absolute path to the backend binary (resolved via
///   `std::env::current_exe()` at app startup)
/// - `args` = `["mcp-bridge"]`
/// - `env` = three pairs built from `TeamMcpStdioConfig::ENV_*` constants
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamMcpStdioServerSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl TeamMcpStdioServerSpec {
    /// Build the spec from the persisted stdio config plus runtime context.
    ///
    /// `backend_binary_path` is the absolute path to the `aionui-backend`
    /// executable (phase1 single-binary constraint — no standalone bridge).
    /// `team_id` is read from `cfg.team_id` rather than taken as a separate
    /// parameter so the wire-level server name stays in sync with the
    /// persisted config across every consumer.
    pub fn from_config(backend_binary_path: &str, cfg: &TeamMcpStdioConfig) -> Self {
        Self {
            name: format!("aionui-team-{}", cfg.team_id),
            command: backend_binary_path.to_owned(),
            args: vec!["mcp-bridge".to_owned()],
            env: vec![
                (
                    TeamMcpStdioConfig::ENV_PORT.to_owned(),
                    cfg.port.to_string(),
                ),
                (TeamMcpStdioConfig::ENV_TOKEN.to_owned(), cfg.token.clone()),
                (
                    TeamMcpStdioConfig::ENV_SLOT_ID.to_owned(),
                    cfg.slot_id.clone(),
                ),
            ],
        }
    }

    /// Convert into the ACP SDK wire type expected by `NewSessionRequest::mcp_servers`.
    pub fn into_sdk(self) -> McpServer {
        // Both `McpServerStdio` and `EnvVariable` are `#[non_exhaustive]` in the
        // SDK, so construction goes through the `new(..)` / builder entry points.
        let env: Vec<EnvVariable> = self
            .env
            .into_iter()
            .map(|(name, value)| EnvVariable::new(name, value))
            .collect();

        let stdio = McpServerStdio::new(self.name, PathBuf::from(self.command))
            .args(self.args)
            .env(env);

        McpServer::Stdio(stdio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cfg() -> TeamMcpStdioConfig {
        TeamMcpStdioConfig {
            team_id: "team-42".into(),
            port: 12345,
            token: "tok-abc".into(),
            slot_id: "slot-1".into(),
        }
    }

    #[test]
    fn from_config_fills_all_fields() {
        let spec = TeamMcpStdioServerSpec::from_config("/usr/bin/aionui-backend", &sample_cfg());

        assert_eq!(spec.name, "aionui-team-team-42");
        assert_eq!(spec.command, "/usr/bin/aionui-backend");
        assert_eq!(spec.args, vec!["mcp-bridge".to_owned()]);
        assert_eq!(spec.env.len(), 3);
    }

    #[test]
    fn env_keys_match_api_type_constants() {
        let spec = TeamMcpStdioServerSpec::from_config("/p", &sample_cfg());
        let kv: std::collections::HashMap<_, _> = spec.env.iter().cloned().collect();

        assert_eq!(
            kv.get(TeamMcpStdioConfig::ENV_PORT).map(String::as_str),
            Some("12345")
        );
        assert_eq!(
            kv.get(TeamMcpStdioConfig::ENV_TOKEN).map(String::as_str),
            Some("tok-abc")
        );
        assert_eq!(
            kv.get(TeamMcpStdioConfig::ENV_SLOT_ID).map(String::as_str),
            Some("slot-1")
        );
    }

    #[test]
    fn into_sdk_serializes_as_stdio_variant() {
        let spec = TeamMcpStdioServerSpec::from_config("/bin/aionui-backend", &sample_cfg());
        let sdk = spec.into_sdk();

        let json = serde_json::to_value(&sdk).expect("serialize");

        // `Stdio` variant is `#[serde(untagged)]` inside `McpServer`, so the
        // JSON is the raw `McpServerStdio` shape — no `"type":"stdio"` tag.
        assert_eq!(json["name"], "aionui-team-team-42");
        assert_eq!(json["command"], "/bin/aionui-backend");
        assert_eq!(json["args"], serde_json::json!(["mcp-bridge"]));

        let env = json["env"].as_array().expect("env array");
        assert_eq!(env.len(), 3);
        let pairs: std::collections::HashMap<_, _> = env
            .iter()
            .map(|v| {
                (
                    v["name"].as_str().unwrap().to_owned(),
                    v["value"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(pairs[TeamMcpStdioConfig::ENV_PORT], "12345");
        assert_eq!(pairs[TeamMcpStdioConfig::ENV_TOKEN], "tok-abc");
        assert_eq!(pairs[TeamMcpStdioConfig::ENV_SLOT_ID], "slot-1");

        // The untagged variant must still round-trip back into the enum.
        let back: McpServer = serde_json::from_value(json).expect("roundtrip");
        assert!(matches!(back, McpServer::Stdio(_)));
    }
}
