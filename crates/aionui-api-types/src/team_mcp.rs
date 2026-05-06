//! Team session MCP stdio connection types.
//!
//! These are promoted from `aionui-team::mcp::bridge` so that downstream
//! crates (`aionui-ai-agent` deserializing `AcpBuildExtra`, etc.) can reference
//! the same shape without depending on `aionui-team`.

use serde::{Deserialize, Serialize};

/// Connection config for the Guide MCP stdio server in solo conversations.
///
/// Passed through `AcpBuildExtra::guide_mcp_config` by the factory so that
/// `build_new_session_request` can inject the Guide as a stdio MCP server
/// when the backend is team-capable and this is not a team session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuideMcpConfig {
    pub port: u16,
    pub token: String,
    pub binary_path: String,
}

/// Stdio connection config for the team session MCP server.
///
/// `team_id` is persisted alongside the connection triple so every
/// consumer (D3 spec builder, D10 ACP injector, D7 bridge subcommand)
/// can derive the wire-level MCP server name `aionui-team-<team_id>`
/// without threading a second parameter through unrelated call sites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMcpStdioConfig {
    pub team_id: String,
    pub port: u16,
    pub token: String,
    pub slot_id: String,
    pub binary_path: String,
}

impl TeamMcpStdioConfig {
    /// env key the stdio bridge reads to learn the backend TCP port.
    pub const ENV_PORT: &'static str = "TEAM_MCP_PORT";
    /// env key the stdio bridge reads to learn the auth token.
    pub const ENV_TOKEN: &'static str = "TEAM_MCP_TOKEN";
    /// env key the stdio bridge reads to learn which agent slot it represents.
    pub const ENV_SLOT_ID: &'static str = "TEAM_AGENT_SLOT_ID";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_preserves_all_fields() {
        let cfg = TeamMcpStdioConfig {
            team_id: "team-42".into(),
            port: 54321,
            token: "tok-abc".into(),
            slot_id: "slot-1".into(),
            binary_path: "/usr/bin/aionui-backend".into(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: TeamMcpStdioConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn deserialization_tolerates_unknown_fields() {
        // Forward-compat: extra fields in persisted `conversation.extra.team_mcp_stdio_config`
        // JSON (e.g. added by a later backend version) must still round-trip through
        // older binaries without error.
        let json = r#"{"team_id":"t-1","port":1,"token":"t","slot_id":"s","binary_path":"/usr/bin/aionui-backend","future_field":42}"#;
        let parsed: TeamMcpStdioConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.team_id, "t-1");
        assert_eq!(parsed.port, 1);
        assert_eq!(parsed.token, "t");
        assert_eq!(parsed.slot_id, "s");
    }
}
