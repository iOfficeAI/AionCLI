use std::sync::Arc;

use crate::agent_task::AgentInstance;
use crate::factory::AgentFactoryDeps;
use crate::factory::acp_assembler::{WorkspaceInfo, assemble_acp_params};
use crate::factory::context::FactoryContext;
use crate::manager::acp::{AcpAgentManager, CatalogForwarder};
use crate::types::BuildTaskOptions;
use agent_client_protocol::schema::{EnvVariable, HttpHeader, McpServer, McpServerHttp, McpServerSse, McpServerStdio};
use aionui_api_types::AcpBuildExtra;
use aionui_common::{AppError, CommandSpec};
use aionui_db::IMcpServerRepository;
use aionui_db::models::McpServerRow;
use tracing::{debug, info, warn};

pub(super) async fn build(
    deps: Arc<AgentFactoryDeps>,
    options: BuildTaskOptions,
    ctx: FactoryContext,
) -> Result<AgentInstance, AppError> {
    let belongs_to_team = options
        .extra
        .get("teamId")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|s| !s.is_empty());

    let mut config: AcpBuildExtra = serde_json::from_value(options.extra)
        .map_err(|e| AppError::BadRequest(format!("Invalid ACP build options: {e}")))?;

    // Resolve the catalog row — prefer explicit agent_id, fall
    // back to a vendor-label match for legacy payloads.
    let meta = if let Some(ref agent_id) = config.agent_id {
        deps.agent_registry.get(agent_id).await
    } else if let Some(ref vendor) = config.backend {
        deps.agent_registry.find_builtin_by_backend(vendor).await
    } else {
        None
    }
    .ok_or_else(|| AppError::BadRequest("ACP agent requires either agent_id or backend in extra".into()))?;

    // Trust the catalog row over the client-supplied `backend` when an
    // `agent_id` was provided. The frontend collapses row-scoped rows
    // (custom ACP / remote) to a shared `custom`/`remote` slot string,
    // which downstream consumers (MCP injection, preset-context
    // composition) would mis-interpret. When the caller only supplied a
    // vendor label (builtin path), we preserve it as-is.
    if config.agent_id.is_some() || config.backend.is_none() {
        config.backend.clone_from(&meta.backend);
    }

    // Inject Guide MCP config for solo (non-team) sessions.
    // Team sessions already carry `team_mcp_stdio_config`; the
    // two are mutually exclusive per the build_new_session_request guard.
    if config.team_mcp_stdio_config.is_some() {
        debug!(ctx.conversation_id, "guide_mcp: skipped: has team_mcp");
    } else if belongs_to_team {
        debug!(
            ctx.conversation_id,
            "guide_mcp: skipped: conversation belongs to a team (extra.teamId)"
        );
    } else if config.guide_mcp_config.is_some() {
        debug!(
            ctx.conversation_id,
            "guide_mcp: skipped: caller already set guide_mcp_config"
        );
    } else if deps.guide_mcp_config.is_none() {
        debug!(ctx.conversation_id, "guide_mcp: skipped: guide server not running");
    } else {
        config.guide_mcp_config.clone_from(&deps.guide_mcp_config);
        info!(
            ctx.conversation_id,
            guide_mcp_port = deps.guide_mcp_config.as_ref().map(|c| c.port),
            "guide_mcp: injected into solo session"
        );
    }

    // Registry resolved the spawn command via `which()` at
    // hydrate time. A missing `resolved_command` means either the
    // CLI was uninstalled between hydrate and now, or the row
    // never had a command (e.g. remote-only). Either way the
    // caller needs to see a BadRequest, not a confusing
    // spawn-time error.
    let (command, args, mut env, cwd) = (
        meta.resolved_command
            .clone()
            .ok_or_else(|| AppError::BadRequest(format!("Agent '{}' CLI not found in PATH", meta.name)))?,
        meta.args.clone(),
        meta.env
            .iter()
            .map(|e| aionui_common::EnvVar {
                name: e.name.clone(),
                value: e.value.clone(),
            })
            .collect::<Vec<_>>(),
        Some(ctx.workspace.clone()),
    );
    if meta.backend.as_deref() == Some("claude") {
        let cc_switch_env = crate::cc_switch::read_claude_provider_env();
        if !cc_switch_env.is_empty() {
            let keys: Vec<&str> = cc_switch_env.keys().map(|k| k.as_str()).collect();
            for (name, value) in &cc_switch_env {
                env.push(aionui_common::EnvVar {
                    name: name.clone(),
                    value: value.clone(),
                });
            }
            tracing::info!(?keys, "cc-switch: env vars injected");
        }
    }

    let command_spec = CommandSpec {
        command,
        args,
        env,
        cwd,
    };
    let session_snapshot = deps.acp_agent_service.load_snapshot_state(&ctx.conversation_id).await;

    // Load user-configured MCP servers from the DB so they reach
    // ACP `session/new` mcpServers payload. Without this the agent
    // starts with zero MCP tools even when the user configured them
    // via Settings → MCP (ELECTRON-1JG).
    let user_mcp_servers = match deps.mcp_server_repo.as_ref() {
        Some(repo) => load_user_mcp_servers(repo.as_ref(), &ctx.conversation_id).await,
        None => Vec::new(),
    };

    let params = Arc::new(
        assemble_acp_params(
            ctx.conversation_id.clone(),
            WorkspaceInfo {
                path: ctx.workspace,
                is_custom: ctx.is_custom_workspace,
            },
            meta,
            command_spec,
            config,
            user_mcp_servers,
            session_snapshot,
            deps.data_dir.clone(),
        )
        .await,
    );

    let skill_mgr = deps.skill_manager.clone();
    let catalog_tx = deps.agent_registry.catalog_sender();

    let (agent, domain_rx, notification_rx) = AcpAgentManager::build(params, skill_mgr, &catalog_tx).await?;

    let arc = Arc::new(agent);
    arc.start_permission_handler();
    arc.start_session_event_tracker(notification_rx);
    CatalogForwarder::spawn(
        arc.agent_id().to_owned(),
        crate::IAgentTask::subscribe(arc.as_ref()),
        catalog_tx,
    );

    // Desired (mode/model/config) are seeded from `params.session_snapshot`
    // inside `AcpAgentManager::new`. The CLI-assigned session id is still
    // loaded here so the first turn after a task rebuild takes the resume
    // path.
    if let Some(sid) = deps.acp_agent_service.load_session_id(&ctx.conversation_id).await {
        arc.set_session_id(sid).await;
    }

    // Open the ACP session eagerly so `POST /warmup` returns only after
    // session/new (or claude-meta-resume / session/load) and the first
    // reconcile pass have completed. Matches aionrs factory behaviour:
    // the caller sees "warmed up" == "ready for PUT /mode | /model".
    arc.warmup_session().await?;

    let instance = AgentInstance::Acp(Arc::clone(&arc));

    // Hand the service the domain event receiver so it can
    // persist user intent changes without reverse-engineering
    // them from CLI observations.
    deps.acp_agent_service.attach(ctx.conversation_id, domain_rx).await;

    Ok(instance)
}

/// Load the operator's enabled MCP servers from the DB, log+skip any rows
/// whose `transport_config` JSON fails to parse (better to start without one
/// MCP tool than fail the whole session), and return them in SDK shape ready
/// for `NewSessionRequest::mcp_servers`.
///
/// Skips disabled and builtin rows: builtins are wired through other paths
/// (e.g. team/guide MCP) and the user can't manage them directly.
async fn load_user_mcp_servers(repo: &dyn IMcpServerRepository, conversation_id: &str) -> Vec<McpServer> {
    let rows = match repo.list().await {
        Ok(r) => r,
        Err(err) => {
            warn!(
                conversation_id,
                error = %err,
                "user_mcp: list() failed; skipping injection"
            );
            return Vec::new();
        }
    };

    let mut servers = Vec::with_capacity(rows.len());
    for row in rows {
        if !row.enabled || row.builtin {
            continue;
        }
        match row_to_sdk_mcp_server(&row) {
            Ok(server) => servers.push(server),
            Err(err) => {
                warn!(
                    conversation_id,
                    server_id = %row.id,
                    server_name = %row.name,
                    error = %err,
                    "user_mcp: failed to convert row; skipping"
                );
            }
        }
    }

    if !servers.is_empty() {
        info!(
            conversation_id,
            count = servers.len(),
            "user_mcp: injected into session/new"
        );
    }
    servers
}

/// Convert an `McpServerRow` into the SDK `McpServer` shape used by
/// `NewSessionRequest::mcp_servers`. Returns an error string when
/// `transport_config` is malformed or required fields are missing.
fn row_to_sdk_mcp_server(row: &McpServerRow) -> Result<McpServer, String> {
    let value: serde_json::Value =
        serde_json::from_str(&row.transport_config).map_err(|e| format!("invalid transport_config JSON: {e}"))?;

    match row.transport_type.as_str() {
        "stdio" => {
            let command = value
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "stdio: missing command".to_owned())?;
            let args: Vec<String> = value
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let env: Vec<EnvVariable> = value
                .get("env")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    let mut entries: Vec<(String, String)> = obj
                        .iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                        .collect();
                    // Sort for deterministic ordering across runs.
                    entries.sort_by(|a, b| a.0.cmp(&b.0));
                    entries.into_iter().map(|(k, v)| EnvVariable::new(k, v)).collect()
                })
                .unwrap_or_default();

            let stdio = McpServerStdio::new(row.name.clone(), command).args(args).env(env);
            Ok(McpServer::Stdio(stdio))
        }
        "http" => {
            let url = value
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "http: missing url".to_owned())?;
            let headers = parse_headers(value.get("headers"));
            Ok(McpServer::Http(
                McpServerHttp::new(row.name.clone(), url).headers(headers),
            ))
        }
        "sse" => {
            let url = value
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "sse: missing url".to_owned())?;
            let headers = parse_headers(value.get("headers"));
            Ok(McpServer::Sse(
                McpServerSse::new(row.name.clone(), url).headers(headers),
            ))
        }
        other => Err(format!("unknown transport type: {other}")),
    }
}

fn parse_headers(value: Option<&serde_json::Value>) -> Vec<HttpHeader> {
    let Some(obj) = value.and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut entries: Vec<(String, String)> = obj
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.into_iter().map(|(k, v)| HttpHeader::new(k, v)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(
        name: &str,
        transport_type: &str,
        transport_config: &str,
        enabled: bool,
        builtin: bool,
    ) -> McpServerRow {
        McpServerRow {
            id: format!("mcp_{name}"),
            name: name.to_owned(),
            description: None,
            enabled,
            transport_type: transport_type.into(),
            transport_config: transport_config.into(),
            tools: None,
            status: "disconnected".into(),
            last_connected: None,
            original_json: None,
            builtin,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn row_to_sdk_stdio_roundtrip() {
        let row = make_row(
            "ctx7",
            "stdio",
            r#"{"command":"npx","args":["-y","@upstash/context7-mcp"],"env":{"K":"V"}}"#,
            true,
            false,
        );
        let server = row_to_sdk_mcp_server(&row).expect("convert");
        match server {
            McpServer::Stdio(s) => {
                assert_eq!(s.name, "ctx7");
                assert_eq!(s.command.to_string_lossy(), "npx");
                assert_eq!(s.args, vec!["-y".to_owned(), "@upstash/context7-mcp".to_owned()]);
                assert_eq!(s.env.len(), 1);
                assert_eq!(s.env[0].name, "K");
                assert_eq!(s.env[0].value, "V");
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[test]
    fn row_to_sdk_http_with_headers() {
        let row = make_row(
            "remote",
            "http",
            r#"{"url":"https://example.com/mcp","headers":{"Authorization":"Bearer tok"}}"#,
            true,
            false,
        );
        let server = row_to_sdk_mcp_server(&row).expect("convert");
        match server {
            McpServer::Http(h) => {
                assert_eq!(h.name, "remote");
                assert_eq!(h.url, "https://example.com/mcp");
                assert_eq!(h.headers.len(), 1);
                assert_eq!(h.headers[0].name, "Authorization");
                assert_eq!(h.headers[0].value, "Bearer tok");
            }
            _ => panic!("expected Http"),
        }
    }

    #[test]
    fn row_to_sdk_unknown_transport_type_errors() {
        let row = make_row("bad", "websocket", "{}", true, false);
        assert!(row_to_sdk_mcp_server(&row).is_err());
    }

    #[test]
    fn row_to_sdk_invalid_json_errors() {
        let row = make_row("bad", "stdio", "not-json", true, false);
        assert!(row_to_sdk_mcp_server(&row).is_err());
    }

    #[test]
    fn row_to_sdk_stdio_missing_command_errors() {
        let row = make_row("bad", "stdio", r#"{"args":[]}"#, true, false);
        assert!(row_to_sdk_mcp_server(&row).is_err());
    }

    // -- load_user_mcp_servers integration -----------------------------------

    use async_trait::async_trait;
    use std::sync::Arc;

    struct MockRepo {
        rows: Vec<McpServerRow>,
        fail: bool,
    }

    #[async_trait]
    impl IMcpServerRepository for MockRepo {
        async fn list(&self) -> Result<Vec<McpServerRow>, aionui_db::DbError> {
            if self.fail {
                Err(aionui_db::DbError::Init("simulated".into()))
            } else {
                Ok(self.rows.clone())
            }
        }
        async fn find_by_id(&self, _id: &str) -> Result<Option<McpServerRow>, aionui_db::DbError> {
            unimplemented!()
        }
        async fn find_by_name(&self, _name: &str) -> Result<Option<McpServerRow>, aionui_db::DbError> {
            unimplemented!()
        }
        async fn create(
            &self,
            _params: aionui_db::CreateMcpServerParams<'_>,
        ) -> Result<McpServerRow, aionui_db::DbError> {
            unimplemented!()
        }
        async fn update(
            &self,
            _id: &str,
            _params: aionui_db::UpdateMcpServerParams<'_>,
        ) -> Result<McpServerRow, aionui_db::DbError> {
            unimplemented!()
        }
        async fn delete(&self, _id: &str) -> Result<(), aionui_db::DbError> {
            unimplemented!()
        }
        async fn batch_upsert(
            &self,
            _servers: &[aionui_db::CreateMcpServerParams<'_>],
        ) -> Result<Vec<McpServerRow>, aionui_db::DbError> {
            unimplemented!()
        }
        async fn update_status(
            &self,
            _id: &str,
            _status: &str,
            _last_connected: Option<aionui_common::TimestampMs>,
        ) -> Result<(), aionui_db::DbError> {
            unimplemented!()
        }
        async fn update_tools(&self, _id: &str, _tools: Option<&str>) -> Result<(), aionui_db::DbError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn load_user_mcp_servers_skips_disabled_and_builtin() {
        let repo: Arc<dyn IMcpServerRepository> = Arc::new(MockRepo {
            rows: vec![
                make_row(
                    "user-enabled",
                    "stdio",
                    r#"{"command":"npx","args":[],"env":{}}"#,
                    true,
                    false,
                ),
                make_row(
                    "user-disabled",
                    "stdio",
                    r#"{"command":"npx","args":[],"env":{}}"#,
                    false,
                    false,
                ),
                make_row(
                    "builtin",
                    "stdio",
                    r#"{"command":"img-gen","args":[],"env":{}}"#,
                    true,
                    true,
                ),
            ],
            fail: false,
        });
        let servers = load_user_mcp_servers(repo.as_ref(), "conv-1").await;
        assert_eq!(servers.len(), 1);
        match &servers[0] {
            McpServer::Stdio(s) => assert_eq!(s.name, "user-enabled"),
            _ => panic!("expected stdio"),
        }
    }

    #[tokio::test]
    async fn load_user_mcp_servers_returns_empty_on_repo_failure() {
        let repo: Arc<dyn IMcpServerRepository> = Arc::new(MockRepo {
            rows: vec![],
            fail: true,
        });
        let servers = load_user_mcp_servers(repo.as_ref(), "conv-1").await;
        assert!(servers.is_empty());
    }

    #[tokio::test]
    async fn load_user_mcp_servers_skips_malformed_rows_but_keeps_others() {
        let repo: Arc<dyn IMcpServerRepository> = Arc::new(MockRepo {
            rows: vec![
                make_row("good", "stdio", r#"{"command":"npx","args":[],"env":{}}"#, true, false),
                make_row("bad", "stdio", "not-json", true, false),
            ],
            fail: false,
        });
        let servers = load_user_mcp_servers(repo.as_ref(), "conv-1").await;
        assert_eq!(servers.len(), 1);
        match &servers[0] {
            McpServer::Stdio(s) => assert_eq!(s.name, "good"),
            _ => panic!("expected stdio"),
        }
    }
}
