use serde::{Deserialize, Serialize};

use crate::{GuideMcpConfig, TeamMcpStdioConfig};

/// ACP-specific fields extracted from `extra` in build task options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpBuildExtra {
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub cli_path: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub custom_agent_id: Option<String>,
    #[serde(default)]
    pub preset_context: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub preset_assistant_id: Option<String>,
    #[serde(default)]
    pub session_mode: Option<String>,
    #[serde(default)]
    pub cron_job_id: Option<String>,
    #[serde(default)]
    pub team_mcp_stdio_config: Option<TeamMcpStdioConfig>,
    #[serde(default)]
    pub guide_mcp_config: Option<GuideMcpConfig>,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// OpenClaw gateway configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenClawGatewayConfig {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub token: Option<String>,
    pub password: Option<String>,
    #[serde(default)]
    pub use_external_gateway: bool,
    pub cli_path: Option<String>,
}

/// OpenClaw-specific fields extracted from `extra` in build task options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenClawBuildExtra {
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub gateway: OpenClawGatewayConfig,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub preset_assistant_id: Option<String>,
    #[serde(default)]
    pub cron_job_id: Option<String>,
    #[serde(default, rename = "sessionKey")]
    pub session_key: Option<String>,
}

/// Remote agent-specific fields extracted from `extra` in build task options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteBuildExtra {
    pub remote_agent_id: String,
}

/// Aionrs-specific fields extracted from `extra` in build task options.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AionrsBuildExtra {
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub preset_rules: Option<String>,
    #[serde(default = "default_aionrs_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub max_turns: Option<usize>,
    #[serde(default)]
    pub session_mode: Option<String>,
    #[serde(default)]
    pub team_mcp_stdio_config: Option<TeamMcpStdioConfig>,
    #[serde(default)]
    pub guide_mcp_config: Option<GuideMcpConfig>,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
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

/// A slash command item available in a conversation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashCommandItem {
    pub command: String,
    pub description: String,
}
