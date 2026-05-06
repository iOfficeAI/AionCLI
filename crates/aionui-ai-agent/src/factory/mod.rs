pub mod acp_assembler;

use aion_agent::session::SessionManager;
use aionui_api_types::{AcpBuildExtra, AionrsBuildExtra, GuideMcpConfig, OpenClawBuildExtra, RemoteBuildExtra};
use aionui_common::{AgentType, AppError, CommandSpec};
use aionui_db::{IProviderRepository, IRemoteAgentRepository};
use futures_util::FutureExt;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::agent_registry::AgentRegistry;
use crate::agent_task::AgentInstance;
use crate::factory::acp_assembler::{WorkspaceInfo, assemble_acp_params};
use crate::manager::acp::{AcpSessionSyncService, CatalogForwarder};
use crate::manager::remote::RemoteAgentConfig;
use crate::skill_manager::AcpSkillManager;
use crate::task_manager::AgentFactory;
use crate::types::{AionrsCompatOverrides, AionrsResolvedConfig, BuildTaskOptions};
use crate::{AcpAgentManager, AionrsAgentManager, NanobotAgentManager, OpenClawAgentManager, RemoteAgentManager};

/// Dependencies needed by the agent factory to construct agents.
pub struct AgentFactoryDeps {
    pub skill_manager: Arc<AcpSkillManager>,
    pub remote_agent_repo: Arc<dyn IRemoteAgentRepository>,
    pub provider_repo: Arc<dyn IProviderRepository>,
    pub encryption_key: [u8; 32],
    pub agent_registry: Arc<AgentRegistry>,
    pub acp_agent_service: Arc<AcpSessionSyncService>,
    pub data_dir: PathBuf,
    /// Absolute path to the backend binary, reused as the `command` of the
    /// stdio MCP bridge injected into ACP `session/new` for team sessions.
    /// Captured once at app startup (`std::env::current_exe()`).
    pub backend_binary_path: Arc<PathBuf>,
    /// Guide MCP server config. When `Some`, injected into solo (non-team)
    /// ACP agent sessions so the agent gets the `aion_create_team` tool.
    /// `None` when the Guide server failed to start (graceful degradation).
    pub guide_mcp_config: Option<GuideMcpConfig>,
}

/// Build a production agent factory that dispatches to concrete agent types.
///
/// [`AgentFactory`] is async: the returned `BoxFuture` is driven by
/// [`crate::task_manager::IWorkerTaskManager::get_or_build_task`] on whatever
/// runtime is currently polling it. This lets us spawn CLI processes and
/// await ACP handshakes directly, without the scoped-thread + `block_on`
/// bridge the old sync-factory version needed.
pub fn build_agent_factory(deps: AgentFactoryDeps) -> AgentFactory {
    let deps = Arc::new(deps);

    Arc::new(move |options: BuildTaskOptions| {
        let deps = deps.clone();
        async move { build_agent(deps, options).await }.boxed()
    })
}

async fn build_agent(deps: Arc<AgentFactoryDeps>, options: BuildTaskOptions) -> Result<AgentInstance, AppError> {
    let conversation_id = options.conversation_id.clone();
    // `is_custom_workspace` is the authoritative signal for "user chose
    // this path" — determined here and plumbed down to the managers
    // that care (currently `AcpAgentManager`, for first-message
    // injection). Do NOT re-derive it from the workspace string later:
    // user paths may incidentally contain "conversations" or "-temp-".
    let (workspace, is_custom_workspace) = if options.workspace.is_empty() {
        // Fallback workspace path: kept in sync with
        // `ConversationService::create`, which places auto-provisioned
        // workspaces under `{data_dir}/conversations/{label}-temp-{id}/`.
        // Reaching this branch means the caller did not supply an
        // `extra.workspace` — construct the same `{label}-temp-{id}`
        // layout so logs, cleanup scripts, and the frontend's "is this a
        // managed temp dir?" heuristic all see a single naming scheme.
        let label = workspace_label(&options.agent_type, options.extra.get("backend"));
        let dir = deps
            .data_dir
            .join("conversations")
            .join(format!("{label}-temp-{conversation_id}"));
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Internal(format!("Failed to create temp workspace: {e}")))?;
        (dir.to_string_lossy().into_owned(), false)
    } else {
        (options.workspace.clone(), true)
    };

    match options.agent_type {
        AgentType::Gemini => Err(AppError::ConversationArchived(
            "This conversation was created with the legacy Gemini runtime, which has been \
             removed. Please start a new conversation with the Gemini ACP backend to continue."
                .into(),
        )),
        AgentType::Acp => {
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

            if config.backend.is_none() {
                config.backend.clone_from(&meta.backend);
            }

            // Inject Guide MCP config for solo (non-team) sessions.
            // Team sessions already carry `team_mcp_stdio_config`; the
            // two are mutually exclusive per the build_new_session_request guard.
            if config.team_mcp_stdio_config.is_some() {
                debug!(conversation_id, "guide_mcp: skipped: has team_mcp");
            } else if config.guide_mcp_config.is_some() {
                debug!(
                    conversation_id,
                    "guide_mcp: skipped: caller already set guide_mcp_config"
                );
            } else if deps.guide_mcp_config.is_none() {
                debug!(conversation_id, "guide_mcp: skipped: guide server not running");
            } else {
                config.guide_mcp_config.clone_from(&deps.guide_mcp_config);
                info!(
                    conversation_id,
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
            let command = meta
                .resolved_command
                .clone()
                .ok_or_else(|| AppError::BadRequest(format!("Agent '{}' CLI not found in PATH", meta.name)))?;
            let args = meta.args.clone();
            let env = meta
                .env
                .iter()
                .map(|e| aionui_common::EnvVar {
                    name: e.name.clone(),
                    value: e.value.clone(),
                })
                .collect();
            let command_spec = CommandSpec {
                command,
                args,
                env,
                cwd: Some(workspace.clone()),
            };

            let params = Arc::new(assemble_acp_params(
                conversation_id.clone(),
                WorkspaceInfo {
                    path: workspace,
                    is_custom: is_custom_workspace,
                },
                meta,
                command_spec,
                config,
            ));

            let skill_mgr = deps.skill_manager.clone();
            let catalog_tx = deps.agent_registry.catalog_sender();

            let (agent, domain_rx) = AcpAgentManager::new(params, skill_mgr, &catalog_tx).await?;

            let arc = Arc::new(agent);
            arc.start_permission_handler();
            arc.start_session_event_tracker();
            CatalogForwarder::spawn(
                arc.agent_metadata_id().to_owned(),
                crate::IAgentTask::subscribe(arc.as_ref()),
                catalog_tx,
            );

            // Seed the aggregate with persisted runtime choices and
            // (if present) the CLI-assigned session id, so the first
            // turn after a task rebuild takes the resume path.
            if let Some(state) = deps.acp_agent_service.load_snapshot_state(&conversation_id).await {
                arc.preload_snapshot(state).await;
            }
            if let Some(sid) = deps.acp_agent_service.load_session_id(&conversation_id).await {
                arc.restore_session_id(sid).await;
            }

            let instance = AgentInstance::Acp(Arc::clone(&arc));

            // Hand the service the domain event receiver so it can
            // persist user intent changes without reverse-engineering
            // them from CLI observations.
            deps.acp_agent_service.attach(conversation_id, domain_rx).await;

            Ok(instance)
        }
        AgentType::OpenclawGateway => {
            let mut config: OpenClawBuildExtra = serde_json::from_value(options.extra)
                .map_err(|e| AppError::BadRequest(format!("Invalid OpenClaw build options: {e}")))?;

            // OpenClaw lives in the catalog as an internal row; reuse
            // the registry-resolved path instead of re-running `which()`.
            if config.gateway.cli_path.is_none()
                && let Some(cli) = deps
                    .agent_registry
                    .list_by_agent_type(AgentType::OpenclawGateway)
                    .await
                    .into_iter()
                    .find_map(|m| m.resolved_command)
                    .map(|p| p.to_string_lossy().into_owned())
            {
                config.gateway.cli_path = Some(cli);
            }

            let resume_session_key = config.session_key.clone();
            let agent = OpenClawAgentManager::new(conversation_id, workspace, config, resume_session_key).await?;
            let arc = Arc::new(agent);
            arc.start_event_relay();
            Ok(AgentInstance::OpenClaw(arc))
        }
        AgentType::Nanobot => {
            // Nanobot lives in the catalog as an internal row; reuse the
            // registry-resolved path instead of re-running `which()`.
            let cli_path = deps
                .agent_registry
                .list_by_agent_type(AgentType::Nanobot)
                .await
                .into_iter()
                .find_map(|m| m.resolved_command)
                .ok_or_else(|| AppError::BadRequest("Nanobot CLI not found in PATH".into()))?;
            let agent = NanobotAgentManager::new(conversation_id, workspace, cli_path).await?;
            Ok(AgentInstance::Nanobot(Arc::new(agent)))
        }
        AgentType::Remote => {
            let extra: RemoteBuildExtra = serde_json::from_value(options.extra)
                .map_err(|e| AppError::BadRequest(format!("Invalid Remote build options: {e}")))?;
            let row = deps
                .remote_agent_repo
                .find_by_id(&extra.remote_agent_id)
                .await
                .map_err(|e| AppError::Internal(format!("Failed to load remote agent config: {e}")))?
                .ok_or_else(|| AppError::NotFound(format!("Remote agent '{}' not found", extra.remote_agent_id)))?;
            let auth_token = row
                .auth_token
                .as_deref()
                .filter(|t| !t.is_empty())
                .and_then(|encrypted| {
                    aionui_common::decrypt_string(encrypted, &deps.encryption_key)
                        .map_err(|e| {
                            warn!(error = %e, "Failed to decrypt remote agent auth_token");
                        })
                        .ok()
                });
            let config = RemoteAgentConfig {
                remote_agent_id: row.id.clone(),
                url: row.url.clone(),
                auth_type: row.auth_type.clone(),
                auth_token,
                allow_insecure: row.allow_insecure,
            };
            let agent = RemoteAgentManager::new(conversation_id, workspace, config).await?;
            Ok(AgentInstance::Remote(Arc::new(agent)))
        }
        AgentType::Aionrs => {
            let overrides: AionrsBuildExtra = serde_json::from_value(options.extra).unwrap_or_default();

            let provider_id = &options.model.provider_id;
            let row = deps
                .provider_repo
                .find_by_id(provider_id)
                .await
                .map_err(|e| AppError::Internal(format!("Failed to load provider config: {e}")))?
                .ok_or_else(|| AppError::BadRequest(format!("Provider '{provider_id}' not found")))?;

            let api_key = aionui_common::decrypt_string(&row.api_key_encrypted, &deps.encryption_key)?;

            let model_id = options
                .model
                .use_model
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(&options.model.model)
                .to_owned();

            let provider = map_aionrs_provider(&row.platform, &model_id, row.model_protocols.as_deref());

            let (base_url, compat_overrides) = resolve_aionrs_url_and_compat(&row.platform, &row.base_url, &provider);

            let session_directory = deps.data_dir.join("aionrs-sessions");

            let resume_session = {
                let session_mgr = SessionManager::new(session_directory.clone(), 100);
                match session_mgr.load(&conversation_id) {
                    Ok(session) => {
                        info!(
                            conversation_id = %conversation_id,
                            session_id = %session.id,
                            message_count = session.messages.len(),
                            "Loaded existing aionrs session for resume"
                        );
                        Some(session)
                    }
                    Err(e) => {
                        debug!(
                            conversation_id = %conversation_id,
                            error = %e,
                            "No existing aionrs session found, starting fresh"
                        );
                        None
                    }
                }
            };

            let config = AionrsResolvedConfig {
                provider,
                api_key,
                model: model_id,
                base_url,
                system_prompt: overrides.system_prompt,
                max_tokens: overrides.max_tokens,
                max_turns: overrides.max_turns,
                compat_overrides,
                session_directory,
                session_mode: overrides.session_mode,
            };

            let agent = AionrsAgentManager::new(conversation_id, workspace, config, resume_session).await?;
            Ok(AgentInstance::Aionrs(Arc::new(agent)))
        }
    }
}

/// Map AionUi DB platform name to the aionrs provider identifier.
///
/// Mirrors the frontend `src/process/agent/aionrs/envBuilder.ts` mapping.
/// For `new-api` platform, per-model protocol overrides from `model_protocols`
/// JSON take precedence.
fn map_aionrs_provider(platform: &str, model_id: &str, model_protocols: Option<&str>) -> String {
    if platform == "new-api"
        && let Some(protocols_json) = model_protocols
        && let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(protocols_json)
        && let Some(serde_json::Value::String(protocol)) = map.get(model_id)
        && protocol == "anthropic"
    {
        return "anthropic".to_owned();
    }

    match platform {
        "anthropic" => "anthropic",
        "bedrock" => "bedrock",
        "gemini-vertex-ai" => "vertex",
        _ => "openai",
    }
    .to_owned()
}

/// Label used in auto-provisioned temp workspace directory names.
///
/// For ACP conversations the label is the vendor string from
/// `extra.backend` (e.g. `"claude"`); otherwise the agent type's serde
/// name (e.g. `"aionrs"`). Must stay in sync with
/// `ConversationService::create`'s `conversation_label`.
fn workspace_label(agent_type: &AgentType, backend: Option<&serde_json::Value>) -> String {
    if *agent_type == AgentType::Acp
        && let Some(serde_json::Value::String(s)) = backend
        && !s.is_empty()
    {
        return s.clone();
    }
    agent_type.serde_name().to_owned()
}

/// Resolve base_url and compat overrides for the aionrs provider.
///
/// Mirrors the frontend `envBuilder.ts` logic:
/// - Strips trailing `/v1` from base_url (aionrs appends its own path)
/// - Gemini: prepends `/v1beta/openai` and overrides `api_path`
/// - OpenAI official (`api.openai.com`): sets `max_completion_tokens`
fn resolve_aionrs_url_and_compat(
    platform: &str,
    raw_base_url: &str,
    mapped_provider: &str,
) -> (Option<String>, AionrsCompatOverrides) {
    let mut compat = AionrsCompatOverrides::default();

    if platform == "gemini" {
        let trimmed = raw_base_url.trim_end_matches('/');
        let base = format!("{trimmed}/v1beta/openai");
        compat.api_path = Some("/chat/completions".to_owned());
        return (Some(base), compat);
    }

    let normalized = normalize_aionrs_base_url(raw_base_url);
    let base_url = Some(normalized).filter(|u| !u.is_empty());

    if mapped_provider == "openai" && is_openai_host(raw_base_url) {
        compat.max_tokens_field = Some("max_completion_tokens".to_owned());
    }

    (base_url, compat)
}

fn is_openai_host(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .map(|rest| rest == "api.openai.com" || rest.starts_with("api.openai.com/"))
        .unwrap_or(false)
}

/// Strip trailing `/v1`, `/v1/`, or lone `/` from a base URL so that
/// aionrs can append its own path suffix (`/v1/messages`, `/v1/chat/completions`).
fn normalize_aionrs_base_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    trimmed.strip_suffix("/v1").unwrap_or(trimmed).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_deps_can_be_constructed() {
        // Verify types compile — actual construction requires DB
        let _: fn() -> AgentFactoryDeps = || {
            panic!("compile-time check only");
        };
    }

    #[test]
    fn normalize_aionrs_base_url_strips_v1() {
        assert_eq!(
            normalize_aionrs_base_url("https://api.openai.com/v1"),
            "https://api.openai.com"
        );
        assert_eq!(
            normalize_aionrs_base_url("https://api.openai.com/v1/"),
            "https://api.openai.com"
        );
        assert_eq!(
            normalize_aionrs_base_url("https://api.anthropic.com"),
            "https://api.anthropic.com"
        );
        assert_eq!(
            normalize_aionrs_base_url("https://api.deepseek.com/"),
            "https://api.deepseek.com"
        );
        assert_eq!(
            normalize_aionrs_base_url("http://localhost:11434"),
            "http://localhost:11434"
        );
        assert_eq!(normalize_aionrs_base_url(""), "");
    }

    #[test]
    fn map_aionrs_provider_known_platforms() {
        assert_eq!(map_aionrs_provider("anthropic", "m", None), "anthropic");
        assert_eq!(map_aionrs_provider("bedrock", "m", None), "bedrock");
        assert_eq!(map_aionrs_provider("gemini-vertex-ai", "m", None), "vertex");
    }

    #[test]
    fn map_aionrs_provider_custom_and_others_default_to_openai() {
        assert_eq!(map_aionrs_provider("custom", "gpt-4o", None), "openai");
        assert_eq!(map_aionrs_provider("gemini", "gemini-2.5-pro", None), "openai");
        assert_eq!(map_aionrs_provider("new-api", "m", None), "openai");
        assert_eq!(map_aionrs_provider("unknown", "m", None), "openai");
    }

    #[test]
    fn map_aionrs_provider_new_api_with_anthropic_protocol() {
        let protocols = r#"{"claude-sonnet":"anthropic","gpt-4o":"openai"}"#;
        assert_eq!(
            map_aionrs_provider("new-api", "claude-sonnet", Some(protocols)),
            "anthropic"
        );
        assert_eq!(map_aionrs_provider("new-api", "gpt-4o", Some(protocols)), "openai");
        assert_eq!(
            map_aionrs_provider("new-api", "unknown-model", Some(protocols)),
            "openai"
        );
    }

    #[test]
    fn map_aionrs_provider_new_api_with_invalid_json() {
        assert_eq!(map_aionrs_provider("new-api", "m", Some("not json")), "openai");
    }

    #[test]
    fn map_aionrs_provider_non_new_api_ignores_protocols() {
        let protocols = r#"{"m":"anthropic"}"#;
        assert_eq!(map_aionrs_provider("custom", "m", Some(protocols)), "openai");
    }

    #[test]
    fn is_openai_host_detects_official_api() {
        assert!(is_openai_host("https://api.openai.com/v1"));
        assert!(is_openai_host("https://api.openai.com"));
        assert!(is_openai_host("https://API.OPENAI.COM/v1"));
        assert!(!is_openai_host("https://api.deepseek.com/v1"));
        assert!(!is_openai_host("https://openai.example.com/v1"));
        assert!(!is_openai_host(""));
        assert!(!is_openai_host("not-a-url"));
    }

    #[test]
    fn resolve_openai_official_sets_max_completion_tokens() {
        let (base_url, compat) = resolve_aionrs_url_and_compat("custom", "https://api.openai.com/v1", "openai");
        assert_eq!(base_url.as_deref(), Some("https://api.openai.com"));
        assert_eq!(compat.max_tokens_field.as_deref(), Some("max_completion_tokens"));
        assert!(compat.api_path.is_none());
    }

    #[test]
    fn resolve_non_openai_keeps_default_max_tokens() {
        let (base_url, compat) = resolve_aionrs_url_and_compat("custom", "https://api.deepseek.com/v1", "openai");
        assert_eq!(base_url.as_deref(), Some("https://api.deepseek.com"));
        assert!(compat.max_tokens_field.is_none());
    }

    #[test]
    fn resolve_gemini_prepends_path_and_sets_api_path() {
        let (base_url, compat) =
            resolve_aionrs_url_and_compat("gemini", "https://generativelanguage.googleapis.com", "openai");
        assert_eq!(
            base_url.as_deref(),
            Some("https://generativelanguage.googleapis.com/v1beta/openai")
        );
        assert_eq!(compat.api_path.as_deref(), Some("/chat/completions"));
        assert!(compat.max_tokens_field.is_none());
    }

    #[test]
    fn resolve_anthropic_no_compat_overrides() {
        let (base_url, compat) = resolve_aionrs_url_and_compat("anthropic", "https://api.anthropic.com", "anthropic");
        assert_eq!(base_url.as_deref(), Some("https://api.anthropic.com"));
        assert!(compat.max_tokens_field.is_none());
        assert!(compat.api_path.is_none());
    }
}
