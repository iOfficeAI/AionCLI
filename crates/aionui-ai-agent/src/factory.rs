use aion_agent::session::SessionManager;
use aionui_common::{AcpBackend, AgentType, AppError, CommandSpec};
use aionui_db::{IProviderRepository, IRemoteAgentRepository};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::agent_manager::AgentManagerHandle;
use crate::agent_registry::AgentRegistry;
use crate::remote_agent::RemoteAgentConfig;
use crate::skill_manager::AcpSkillManager;
use crate::task_manager::AgentFactory;
use crate::types::{
    AcpBuildExtra, AionrsBuildExtra, AionrsCompatOverrides, AionrsResolvedConfig, BuildTaskOptions,
    OpenClawBuildExtra, RemoteBuildExtra,
};
use crate::{
    AcpAgentManager, AionrsAgentManager, NanobotAgentManager, OpenClawAgentManager,
    RemoteAgentManager,
};

/// Dependencies needed by the agent factory to construct agents.
pub struct AgentFactoryDeps {
    pub skill_manager: Arc<AcpSkillManager>,
    pub remote_agent_repo: Arc<dyn IRemoteAgentRepository>,
    pub provider_repo: Arc<dyn IProviderRepository>,
    pub encryption_key: [u8; 32],
    pub agent_registry: Arc<AgentRegistry>,
    pub data_dir: PathBuf,
    /// Absolute path to the backend binary, reused as the `command` of the
    /// stdio MCP bridge injected into ACP `session/new` for team sessions.
    /// Captured once at app startup (`std::env::current_exe()`).
    pub backend_binary_path: Arc<PathBuf>,
}

/// Build a production agent factory that dispatches to concrete agent types.
///
/// The factory bridges the synchronous `AgentFactory` signature to async agent
/// constructors. Uses a scoped thread + `Handle::block_on` so it works on both
/// multi-threaded and single-threaded (test) tokio runtimes.
pub fn build_agent_factory(deps: AgentFactoryDeps) -> AgentFactory {
    let deps = Arc::new(deps);

    Arc::new(move |options: BuildTaskOptions| {
        let deps = deps.clone();
        let handle = tokio::runtime::Handle::current();

        std::thread::scope(|s| {
            s.spawn(|| handle.block_on(build_agent(deps, options)))
                .join()
                .map_err(|_| AppError::Internal("Agent construction panicked".into()))?
        })
    })
}

async fn build_agent(
    deps: Arc<AgentFactoryDeps>,
    options: BuildTaskOptions,
) -> Result<AgentManagerHandle, AppError> {
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

            // Resolve agent from registry — try agent_id first, then backend
            let detected = if let Some(ref agent_id) = config.agent_id {
                deps.agent_registry.get_by_id(agent_id).await
            } else if let Some(backend) = config.backend {
                deps.agent_registry.get_by_id(&backend.id()).await
            } else {
                None
            };

            // Fill in missing fields from detected agent
            if let Some(ref detected) = detected
                && config.backend.is_none()
            {
                config.backend = detected.backend;
            }

            let (spawn_command, spawn_args, spawn_env) = match detected {
                Some(ref d) if d.command.is_some() => {
                    (d.command.clone().unwrap(), d.args.clone(), d.env.clone())
                }
                _ => {
                    // Last resort fallback: direct CLI with default ACP args
                    let backend = config
                        .backend
                        .ok_or_else(|| AppError::BadRequest("ACP backend is required".into()))?;

                    let binary = backend.binary_name().ok_or_else(|| {
                        AppError::BadRequest(format!("Backend {backend:?} has no CLI binary"))
                    })?;
                    let path = which::which(binary)
                        .map(|p| p.to_string_lossy().into_owned())
                        .map_err(|_| {
                            AppError::BadRequest(format!("CLI '{binary}' not found in PATH"))
                        })?;
                    let args = backend
                        .args()
                        .unwrap_or(&["--experimental-acp"])
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect();
                    (path, args, vec![])
                }
            };

            let agent = AcpAgentManager::new(
                conversation_id,
                workspace.clone(),
                is_custom_workspace,
                CommandSpec {
                    command: PathBuf::from(spawn_command),
                    args: spawn_args,
                    env: spawn_env,
                    cwd: Some(workspace),
                },
                config,
                deps.skill_manager.clone(),
                deps.backend_binary_path.clone(),
            )
            .await?;
            let arc = Arc::new(agent);
            arc.start_permission_handler();
            arc.start_runtime_snapshot_tracker();
            Ok(arc as AgentManagerHandle)
        }
        AgentType::OpenclawGateway => {
            let mut config: OpenClawBuildExtra =
                serde_json::from_value(options.extra).map_err(|e| {
                    AppError::BadRequest(format!("Invalid OpenClaw build options: {e}"))
                })?;

            if config.gateway.cli_path.is_none()
                && let Some(detected) = deps
                    .agent_registry
                    .get_by_id(&AgentType::OpenclawGateway.id())
                    .await
            {
                config.gateway.cli_path = detected.command;
            }

            let resume_session_key = config.session_key.clone();
            let agent =
                OpenClawAgentManager::new(conversation_id, workspace, config, resume_session_key)
                    .await?;
            let arc = Arc::new(agent);
            arc.start_event_relay();
            Ok(arc as AgentManagerHandle)
        }
        AgentType::Nanobot => {
            let cli_path = deps
                .agent_registry
                .get_by_id(&AgentType::Nanobot.id())
                .await
                .and_then(|d| d.command)
                .ok_or_else(|| {
                    AppError::BadRequest("Nanobot CLI not found in agent registry".into())
                })?;
            let agent = NanobotAgentManager::new(conversation_id, workspace, cli_path).await?;
            Ok(Arc::new(agent) as AgentManagerHandle)
        }
        AgentType::Remote => {
            let extra: RemoteBuildExtra = serde_json::from_value(options.extra)
                .map_err(|e| AppError::BadRequest(format!("Invalid Remote build options: {e}")))?;
            let row = deps
                .remote_agent_repo
                .find_by_id(&extra.remote_agent_id)
                .await
                .map_err(|e| {
                    AppError::Internal(format!("Failed to load remote agent config: {e}"))
                })?
                .ok_or_else(|| {
                    AppError::NotFound(format!(
                        "Remote agent '{}' not found",
                        extra.remote_agent_id
                    ))
                })?;
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
            Ok(Arc::new(agent) as AgentManagerHandle)
        }
        AgentType::Aionrs => {
            let overrides: AionrsBuildExtra =
                serde_json::from_value(options.extra).unwrap_or_default();

            let provider_id = &options.model.provider_id;
            let row = deps
                .provider_repo
                .find_by_id(provider_id)
                .await
                .map_err(|e| AppError::Internal(format!("Failed to load provider config: {e}")))?
                .ok_or_else(|| {
                    AppError::BadRequest(format!("Provider '{provider_id}' not found"))
                })?;

            let api_key =
                aionui_common::decrypt_string(&row.api_key_encrypted, &deps.encryption_key)?;

            let model_id = options
                .model
                .use_model
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(&options.model.model)
                .to_owned();

            let provider =
                map_aionrs_provider(&row.platform, &model_id, row.model_protocols.as_deref());

            let (base_url, compat_overrides) =
                resolve_aionrs_url_and_compat(&row.platform, &row.base_url, &provider);

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

            let agent =
                AionrsAgentManager::new(conversation_id, workspace, config, resume_session).await?;
            Ok(Arc::new(agent) as AgentManagerHandle)
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
        && let Ok(map) =
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(protocols_json)
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
/// For ACP conversations the label is the sub-backend id
/// (e.g. `"claude"`, `"gemini"`); otherwise the agent type's serde name
/// (e.g. `"aionrs"`). Must stay in sync with
/// `ConversationService::create`'s `conversation_label`.
fn workspace_label(agent_type: &AgentType, backend: Option<&serde_json::Value>) -> String {
    if *agent_type == AgentType::Acp
        && let Some(v) = backend
        && let Ok(be) = serde_json::from_value::<AcpBackend>(v.clone())
        && let Ok(serde_json::Value::String(s)) = serde_json::to_value(be)
    {
        return s;
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
        assert_eq!(
            map_aionrs_provider("gemini", "gemini-2.5-pro", None),
            "openai"
        );
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
        assert_eq!(
            map_aionrs_provider("new-api", "gpt-4o", Some(protocols)),
            "openai"
        );
        assert_eq!(
            map_aionrs_provider("new-api", "unknown-model", Some(protocols)),
            "openai"
        );
    }

    #[test]
    fn map_aionrs_provider_new_api_with_invalid_json() {
        assert_eq!(
            map_aionrs_provider("new-api", "m", Some("not json")),
            "openai"
        );
    }

    #[test]
    fn map_aionrs_provider_non_new_api_ignores_protocols() {
        let protocols = r#"{"m":"anthropic"}"#;
        assert_eq!(
            map_aionrs_provider("custom", "m", Some(protocols)),
            "openai"
        );
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
        let (base_url, compat) =
            resolve_aionrs_url_and_compat("custom", "https://api.openai.com/v1", "openai");
        assert_eq!(base_url.as_deref(), Some("https://api.openai.com"));
        assert_eq!(
            compat.max_tokens_field.as_deref(),
            Some("max_completion_tokens")
        );
        assert!(compat.api_path.is_none());
    }

    #[test]
    fn resolve_non_openai_keeps_default_max_tokens() {
        let (base_url, compat) =
            resolve_aionrs_url_and_compat("custom", "https://api.deepseek.com/v1", "openai");
        assert_eq!(base_url.as_deref(), Some("https://api.deepseek.com"));
        assert!(compat.max_tokens_field.is_none());
    }

    #[test]
    fn resolve_gemini_prepends_path_and_sets_api_path() {
        let (base_url, compat) = resolve_aionrs_url_and_compat(
            "gemini",
            "https://generativelanguage.googleapis.com",
            "openai",
        );
        assert_eq!(
            base_url.as_deref(),
            Some("https://generativelanguage.googleapis.com/v1beta/openai")
        );
        assert_eq!(compat.api_path.as_deref(), Some("/chat/completions"));
        assert!(compat.max_tokens_field.is_none());
    }

    #[test]
    fn resolve_anthropic_no_compat_overrides() {
        let (base_url, compat) =
            resolve_aionrs_url_and_compat("anthropic", "https://api.anthropic.com", "anthropic");
        assert_eq!(base_url.as_deref(), Some("https://api.anthropic.com"));
        assert!(compat.max_tokens_field.is_none());
        assert!(compat.api_path.is_none());
    }
}
