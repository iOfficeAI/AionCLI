//! Business-logic layer for the ai-agent crate.
//!
//! Per `AGENTS.md` "Domain Crate Structure", this is the sole location
//! for agent-related business logic. HTTP handlers in `routes/` should
//! only extract inputs, call methods on this service, and wrap the
//! result in `ApiResponse`.
//!
//! Session-scoped operations (mode/model/config/usage/capabilities/
//! slash-commands/side-question/workspace/openclaw-runtime) now live in
//! `aionui-conversation::ConversationService`, which dispatches through
//! `AgentInstance`. This service retains only agent-catalog and
//! ACP health-check responsibilities, plus support for the custom-agent
//! CRUD endpoints (see `services::custom`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use aionui_api_types::{
    AcpHealthCheckRequest, AcpHealthCheckResponse, AgentMetadata, AgentSource, AgentWarmupRequest, AgentWarmupResponse,
    AgentWarmupResult, AgentWarmupStatus,
};
use aionui_common::{AgentType, AppError};

use crate::registry::AgentRegistry;

pub struct AgentService {
    registry: Arc<AgentRegistry>,
    data_dir: PathBuf,
}

impl AgentService {
    pub fn new(registry: Arc<AgentRegistry>, data_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self { registry, data_dir })
    }

    /// Data directory used by the custom-agent probe to spawn CLI
    /// processes with a stable cwd.
    pub(crate) fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    /// Registry accessor consumed by the `services::custom` submodule
    /// for direct repository access (upsert / delete / enable toggle).
    pub(crate) fn registry(&self) -> &Arc<AgentRegistry> {
        &self.registry
    }
}

// Agent operations
impl AgentService {
    pub async fn list_agents(&self) -> Result<Vec<AgentMetadata>, AppError> {
        Ok(self.registry.list_all().await)
    }

    pub async fn refresh_agents(&self) -> Result<Vec<AgentMetadata>, AppError> {
        self.registry.refresh_availability().await;
        Ok(self.registry.list_all().await)
    }

    pub async fn acp_health_check(&self, req: AcpHealthCheckRequest) -> Result<AcpHealthCheckResponse, AppError> {
        Ok(crate::protocol::cli_detect::health_check(&self.registry, &req.backend).await)
    }

    pub async fn warmup_agents(&self, req: AgentWarmupRequest) -> Result<AgentWarmupResponse, AppError> {
        let mut results = Vec::with_capacity(req.backends.len());
        let mut seen = HashSet::new();

        for raw_backend in req.backends {
            let backend = raw_backend.trim().to_lowercase();
            if backend.is_empty() || !seen.insert(backend.clone()) {
                continue;
            }

            results.push(self.warmup_backend(&backend).await);
        }

        Ok(AgentWarmupResponse { results })
    }

    async fn warmup_backend(&self, backend: &str) -> AgentWarmupResult {
        let Some(meta) = self.registry.find_builtin_by_backend(backend).await else {
            return AgentWarmupResult {
                backend: backend.to_owned(),
                status: AgentWarmupStatus::Skipped,
                agent_id: None,
                error: Some("agent backend is not registered".into()),
            };
        };

        if meta.agent_type != AgentType::Acp || meta.agent_source != AgentSource::Builtin {
            return AgentWarmupResult {
                backend: backend.to_owned(),
                status: AgentWarmupStatus::Skipped,
                agent_id: Some(meta.id),
                error: Some("agent backend is not a builtin ACP agent".into()),
            };
        }

        let Some(command) = meta.resolved_command.clone() else {
            return AgentWarmupResult {
                backend: backend.to_owned(),
                status: AgentWarmupStatus::Skipped,
                agent_id: Some(meta.id),
                error: Some("agent CLI is not available".into()),
            };
        };

        let mut env: HashMap<String, String> = meta
            .env
            .iter()
            .map(|entry| (entry.name.clone(), entry.value.clone()))
            .collect();
        if meta.backend.as_deref() == Some("claude") {
            env.extend(crate::cc_switch::read_claude_provider_env());
        }

        match crate::protocol::custom_agent_probe::acp_initialize(command, &meta.args, &env, &self.data_dir).await {
            Ok(handshake) => {
                if handshake.agent_capabilities.is_some() || handshake.auth_methods.is_some() {
                    self.registry.catalog_sender().send_partial(meta.id.clone(), handshake);
                }
                AgentWarmupResult {
                    backend: backend.to_owned(),
                    status: AgentWarmupStatus::Ready,
                    agent_id: Some(meta.id),
                    error: None,
                }
            }
            Err(error) => AgentWarmupResult {
                backend: backend.to_owned(),
                status: AgentWarmupStatus::Failed,
                agent_id: Some(meta.id),
                error: Some(error),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use aionui_api_types::{AgentWarmupReason, AgentWarmupRequest, AgentWarmupStatus};
    use aionui_db::{IAgentMetadataRepository, SqliteAgentMetadataRepository, UpsertAgentMetadataParams};

    use super::*;

    async fn setup_service() -> (Arc<AgentService>, Arc<dyn IAgentMetadataRepository>) {
        let db = aionui_db::init_database_memory().await.unwrap();
        let repo: Arc<dyn IAgentMetadataRepository> = Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone()));
        let registry = AgentRegistry::new(repo.clone());
        registry.hydrate().await.unwrap();
        (AgentService::new(registry, PathBuf::from(std::env::temp_dir())), repo)
    }

    fn missing_builtin_params<'a>(id: &'a str, backend: &'a str) -> UpsertAgentMetadataParams<'a> {
        UpsertAgentMetadataParams {
            id,
            icon: None,
            name: "Missing Warmup Agent",
            name_i18n: None,
            description: Some("missing warmup test row"),
            description_i18n: None,
            backend: Some(backend),
            agent_type: "acp",
            agent_source: "builtin",
            agent_source_info: Some(r#"{"binary_name":"aionui-definitely-missing-warmup"}"#),
            enabled: true,
            command: Some("aionui-definitely-missing-warmup"),
            args: Some("[]"),
            env: Some("[]"),
            native_skills_dirs: None,
            behavior_policy: None,
            yolo_id: None,
            agent_capabilities: None,
            auth_methods: None,
            config_options: None,
            available_modes: None,
            available_models: None,
            available_commands: None,
            sort_order: 9900,
        }
    }

    #[tokio::test]
    async fn warmup_empty_backends_returns_empty_results() {
        let (service, _repo) = setup_service().await;

        let resp = service
            .warmup_agents(AgentWarmupRequest {
                backends: vec![],
                reason: AgentWarmupReason::Idle,
            })
            .await
            .unwrap();

        assert!(resp.results.is_empty());
    }

    #[tokio::test]
    async fn warmup_unknown_backend_returns_structured_skip() {
        let (service, _repo) = setup_service().await;

        let resp = service
            .warmup_agents(AgentWarmupRequest {
                backends: vec!["not-real-agent".into()],
                reason: AgentWarmupReason::UserSelect,
            })
            .await
            .unwrap();

        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].backend, "not-real-agent");
        assert_eq!(resp.results[0].status, AgentWarmupStatus::Skipped);
        assert!(resp.results[0].error.as_deref().unwrap().contains("not registered"));
    }

    #[tokio::test]
    async fn warmup_missing_builtin_cli_returns_structured_skip() {
        let (service, repo) = setup_service().await;
        repo.upsert(&missing_builtin_params("missing-warmup", "missing-warmup"))
            .await
            .unwrap();
        service.registry.invalidate_and_rehydrate().await.unwrap();

        let resp = service
            .warmup_agents(AgentWarmupRequest {
                backends: vec!["missing-warmup".into()],
                reason: AgentWarmupReason::BeforeSend,
            })
            .await
            .unwrap();

        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].status, AgentWarmupStatus::Skipped);
        assert_eq!(resp.results[0].agent_id.as_deref(), Some("missing-warmup"));
        assert!(resp.results[0].error.as_deref().unwrap().contains("not available"));
    }
}
