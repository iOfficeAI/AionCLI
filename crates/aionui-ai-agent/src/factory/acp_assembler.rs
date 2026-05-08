use crate::capability::team_guide_prompt;
use crate::shared_kernel::PersistedSessionState;
use agent_client_protocol::schema::{EnvVariable, McpServer, McpServerStdio, NewSessionRequest};
use aionui_api_types::AgentMetadata;
use aionui_api_types::{AcpBuildExtra, GuideMcpConfig, TeamMcpStdioConfig};
use aionui_common::CommandSpec;

use aionui_common::constants::TEAM_CAPABLE_BACKENDS;

/// Pre-computed workspace information.
#[derive(Debug, Clone)]
pub struct WorkspaceInfo {
    pub path: String,
    pub is_custom: bool,
}

/// All pre-computed parameters needed to create and drive an ACP session.
///
/// Assembled once by `assemble_acp_params` in the factory layer; the
/// `AcpAgentManager` reads from this but never mutates it. By front-loading
/// the decision logic (which MCP servers to inject, what preset context to
/// compose) we keep the manager focused on execution + state.
#[derive(Debug, Clone)]
pub struct AcpSessionParams {
    pub conversation_id: String,
    pub workspace: WorkspaceInfo,
    pub metadata: AgentMetadata,
    pub command_spec: CommandSpec,
    pub config: AcpBuildExtra,
    pub mcp_servers: Vec<McpServer>,
    pub preset_context: Option<String>,
    pub session_snapshot: Option<PersistedSessionState>,
}

impl AcpSessionParams {
    /// Build a `NewSessionRequest` using the pre-computed MCP servers.
    pub fn new_session_request(&self) -> NewSessionRequest {
        let req = NewSessionRequest::new(&self.workspace.path);
        if self.mcp_servers.is_empty() {
            req
        } else {
            req.mcp_servers(self.mcp_servers.clone())
        }
    }
}

/// Assemble fully-resolved ACP session params from factory inputs.
///
/// This front-loads all decision logic that was previously scattered across
/// `build_new_session_request`, `compose_preset_context_with_team_guide`,
/// and the factory's ACP match arm.
pub async fn assemble_acp_params(
    conversation_id: String,
    workspace: WorkspaceInfo,
    metadata: AgentMetadata,
    command_spec: CommandSpec,
    config: AcpBuildExtra,
    session_snapshot: Option<PersistedSessionState>,
) -> AcpSessionParams {
    let mcp_servers = resolve_mcp_servers(&config, &conversation_id);
    let preset_context = compose_preset_context(
        config.preset_context.as_deref(),
        config.backend.as_deref(),
        config.team_mcp_stdio_config.is_some(),
    );

    AcpSessionParams {
        conversation_id,
        workspace,
        metadata,
        command_spec,
        config,
        mcp_servers,
        preset_context,
        session_snapshot,
    }
}

/// Determine which MCP servers to inject into `session/new`.
///
/// Priority: team session > solo guide > none. The two injections are
/// mutually exclusive.
fn resolve_mcp_servers(config: &AcpBuildExtra, conversation_id: &str) -> Vec<McpServer> {
    if let Some(cfg) = config.team_mcp_stdio_config.as_ref() {
        return vec![team_mcp_server(cfg)];
    }
    if let Some(guide_cfg) = config.guide_mcp_config.as_ref()
        && config
            .backend
            .as_deref()
            .is_some_and(|b| TEAM_CAPABLE_BACKENDS.contains(&b))
    {
        return vec![guide_mcp_server(guide_cfg, config, conversation_id)];
    }
    Vec::new()
}

/// Compose first-message preset context, optionally appending the Team Guide
/// prompt for solo sessions on team-capable backends.
fn compose_preset_context(
    base_preset_context: Option<&str>,
    backend: Option<&str>,
    has_team_session: bool,
) -> Option<String> {
    let base = base_preset_context
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    if has_team_session {
        return base;
    }
    let backend_key = backend.unwrap_or_default();
    if !team_guide_prompt::is_solo_team_guide_backend(backend_key) {
        return base;
    }

    let guide = team_guide_prompt::build_solo_team_guide_prompt(backend_key);
    match base {
        Some(ctx) => Some(format!("{ctx}\n\n{guide}")),
        None => Some(guide),
    }
}

fn team_mcp_server(cfg: &TeamMcpStdioConfig) -> McpServer {
    let env = vec![
        EnvVariable::new(TeamMcpStdioConfig::ENV_PORT.to_owned(), cfg.port.to_string()),
        EnvVariable::new(TeamMcpStdioConfig::ENV_TOKEN.to_owned(), cfg.token.clone()),
        EnvVariable::new(TeamMcpStdioConfig::ENV_SLOT_ID.to_owned(), cfg.slot_id.clone()),
    ];
    let stdio = McpServerStdio::new(format!("aionui-team-{}", cfg.team_id), &cfg.binary_path)
        .args(vec!["mcp-team-stdio".to_owned()])
        .env(env);
    McpServer::Stdio(stdio)
}

fn guide_mcp_server(cfg: &GuideMcpConfig, extra: &AcpBuildExtra, conversation_id: &str) -> McpServer {
    let env = vec![
        EnvVariable::new("AION_MCP_PORT".to_owned(), cfg.port.to_string()),
        EnvVariable::new("AION_MCP_TOKEN".to_owned(), cfg.token.clone()),
        EnvVariable::new("AION_MCP_BACKEND".to_owned(), extra.backend.clone().unwrap_or_default()),
        EnvVariable::new("AION_MCP_CONVERSATION_ID".to_owned(), conversation_id.to_owned()),
        EnvVariable::new("AION_MCP_USER_ID".to_owned(), extra.user_id.clone().unwrap_or_default()),
    ];
    let stdio = McpServerStdio::new("aionui-team-guide", &cfg.binary_path)
        .args(vec!["mcp-guide-stdio".to_owned()])
        .env(env);
    McpServer::Stdio(stdio)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_preset_context_no_team_no_backend() {
        let result = compose_preset_context(Some("hello"), None, false);
        assert_eq!(result, Some("hello".to_owned()));
    }

    #[test]
    fn compose_preset_context_team_session_skips_guide() {
        let result = compose_preset_context(Some("hello"), Some("claude"), true);
        assert_eq!(result, Some("hello".to_owned()));
    }

    #[test]
    fn compose_preset_context_non_team_capable_backend() {
        let result = compose_preset_context(Some("hello"), Some("unknown"), false);
        assert_eq!(result, Some("hello".to_owned()));
    }

    #[test]
    fn compose_preset_context_team_capable_backend_appends_guide() {
        let result = compose_preset_context(None, Some("claude"), false);
        assert!(result.is_some());
        assert!(result.unwrap().contains("team"));
    }

    #[test]
    fn compose_preset_context_empty_string_treated_as_none() {
        let result = compose_preset_context(Some("  "), Some("unknown"), false);
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_mcp_servers_prefers_team_over_guide() {
        let config = AcpBuildExtra {
            agent_id: None,
            backend: Some("claude".into()),
            cli_path: None,
            agent_name: None,
            custom_agent_id: None,
            preset_context: None,
            skills: vec![],
            preset_assistant_id: None,
            session_mode: None,
            cron_job_id: None,
            team_mcp_stdio_config: Some(TeamMcpStdioConfig {
                team_id: "team-1".into(),
                port: 9999,
                token: "tok".into(),
                slot_id: "slot-lead".into(),
                binary_path: "/bin/backend".into(),
            }),
            guide_mcp_config: Some(GuideMcpConfig {
                port: 8888,
                token: "guide-tok".into(),
                binary_path: "/bin/backend".into(),
            }),
            user_id: None,
        };
        let servers = resolve_mcp_servers(&config, "conv-1");
        assert_eq!(servers.len(), 1);
        match &servers[0] {
            McpServer::Stdio(s) => assert!(s.name.contains("team-1")),
            _ => panic!("expected stdio server"),
        }
    }

    #[test]
    fn resolve_mcp_servers_guide_for_team_capable_solo() {
        let config = AcpBuildExtra {
            agent_id: None,
            backend: Some("claude".into()),
            cli_path: None,
            agent_name: None,
            custom_agent_id: None,
            preset_context: None,
            skills: vec![],
            preset_assistant_id: None,
            session_mode: None,
            cron_job_id: None,
            team_mcp_stdio_config: None,
            guide_mcp_config: Some(GuideMcpConfig {
                port: 8888,
                token: "guide-tok".into(),
                binary_path: "/bin/backend".into(),
            }),
            user_id: None,
        };
        let servers = resolve_mcp_servers(&config, "conv-1");
        assert_eq!(servers.len(), 1);
        match &servers[0] {
            McpServer::Stdio(s) => assert_eq!(s.name, "aionui-team-guide"),
            _ => panic!("expected stdio server"),
        }
    }

    #[test]
    fn resolve_mcp_servers_non_team_capable_backend_gets_none() {
        let config = AcpBuildExtra {
            agent_id: None,
            backend: Some("unknown-backend".into()),
            cli_path: None,
            agent_name: None,
            custom_agent_id: None,
            preset_context: None,
            skills: vec![],
            preset_assistant_id: None,
            session_mode: None,
            cron_job_id: None,
            team_mcp_stdio_config: None,
            guide_mcp_config: Some(GuideMcpConfig {
                port: 8888,
                token: "guide-tok".into(),
                binary_path: "/bin/backend".into(),
            }),
            user_id: None,
        };
        let servers = resolve_mcp_servers(&config, "conv-1");
        assert!(servers.is_empty());
    }
}
