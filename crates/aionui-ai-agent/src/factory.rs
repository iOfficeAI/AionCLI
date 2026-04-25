use aionui_common::{AcpBackend, AgentType, AppError, CommandSpec, now_ms};
use aionui_db::IRemoteAgentRepository;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::warn;

use crate::agent_manager::AgentManagerHandle;
use crate::agent_registry::AgentRegistry;
use crate::remote_agent::RemoteAgentConfig;
use crate::skill_manager::AcpSkillManager;
use crate::task_manager::AgentFactory;
use crate::types::{
    AcpBuildExtra, AionrsBuildExtra, BuildTaskOptions, GeminiBuildExtra, OpenClawBuildExtra,
    RemoteBuildExtra,
};
use crate::{
    AcpAgentManager, AionrsAgentManager, GeminiAgentManager, NanobotAgentManager,
    OpenClawAgentManager, RemoteAgentManager,
};

/// Dependencies needed by the agent factory to construct agents.
pub struct AgentFactoryDeps {
    pub skill_manager: Arc<AcpSkillManager>,
    pub remote_agent_repo: Arc<dyn IRemoteAgentRepository>,
    pub encryption_key: [u8; 32],
    pub agent_registry: Arc<AgentRegistry>,
    pub data_dir: PathBuf,
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
    let workspace = if options.workspace.is_empty() {
        let label = match options.agent_type {
            AgentType::Acp => {
                let backend = options
                    .extra
                    .get("backend")
                    .and_then(|v| serde_json::from_value::<AcpBackend>(v.clone()).ok());
                match backend {
                    Some(b) => format!("acp-{}", b.display_name()).to_lowercase(),
                    None => "acp".to_string(),
                }
            }
            other => format!("{other:?}").to_lowercase(),
        };
        let dir = deps
            .data_dir
            .join("tmp")
            .join(format!("{label}-temp-{}", now_ms()));
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Internal(format!("Failed to create temp workspace: {e}")))?;
        dir.to_string_lossy().into_owned()
    } else {
        options.workspace.clone()
    };

    match options.agent_type {
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
                CommandSpec {
                    command: PathBuf::from(spawn_command),
                    args: spawn_args,
                    env: spawn_env,
                    cwd: Some(workspace),
                },
                config,
            )
            .await?;
            let arc = Arc::new(agent);
            arc.start_permission_handler();
            Ok(arc as AgentManagerHandle)
        }
        AgentType::Gemini => {
            let config: GeminiBuildExtra = serde_json::from_value(options.extra)
                .map_err(|e| AppError::BadRequest(format!("Invalid Gemini build options: {e}")))?;
            // Gemini CLI path detected via `which gemini`
            let cli_path = which::which("gemini")
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(|_| AppError::BadRequest("Gemini CLI not found in PATH".into()))?;
            let agent = GeminiAgentManager::new(
                conversation_id,
                workspace,
                cli_path,
                config,
                Some(deps.skill_manager.clone()),
            )
            .await?;
            Ok(Arc::new(agent) as AgentManagerHandle)
        }
        AgentType::OpenclawGateway => {
            let config: OpenClawBuildExtra =
                serde_json::from_value(options.extra).map_err(|e| {
                    AppError::BadRequest(format!("Invalid OpenClaw build options: {e}"))
                })?;
            let agent = OpenClawAgentManager::new(conversation_id, workspace, config).await?;
            Ok(Arc::new(agent) as AgentManagerHandle)
        }
        AgentType::Nanobot => {
            let cli_path = which::which("nanobot")
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(|_| AppError::BadRequest("Nanobot CLI not found in PATH".into()))?;
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
            let config: AionrsBuildExtra = serde_json::from_value(options.extra)
                .map_err(|e| AppError::BadRequest(format!("Invalid Aionrs build options: {e}")))?;
            let agent = AionrsAgentManager::new(conversation_id, workspace, config);
            Ok(Arc::new(agent) as AgentManagerHandle)
        }
    }
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
}
