//! Business-logic layer for the ai-agent crate.
//!
//! Per `AGENTS.md` "Domain Crate Structure", this is the sole location
//! for agent-related business logic. HTTP handlers in `routes/` should
//! only extract inputs, call methods on this service, and wrap the
//! result in `ApiResponse`. Methods will be added in Stage 2b–2f.

use std::path::Component;
use std::sync::Arc;

use agent_client_protocol::schema::SessionModelState;
use aionui_api_types::{
    AcpEnvResponse, AcpHealthCheckRequest, AcpHealthCheckResponse, AcpModelInfo, AgentMetadata, AgentModeResponse,
    DetectCliRequest, DetectCliResponse, GetModelInfoResponse, ModelInfoEntry, ModelInfoPayload, ProbeModelRequest,
    SetConfigOptionRequest, SetConfigOptionsRequest, SetModeRequest, SetModelRequest, SideQuestionRequest,
    SideQuestionResponse, SlashCommandItem, TestCustomAgentRequest, TestCustomAgentResponse, WorkspaceBrowseQuery,
    WorkspaceEntry,
};
use aionui_common::AppError;
use aionui_db::IConversationRepository;

const MAX_DIR_DEPTH: usize = 10;

use crate::agent_task::AgentInstance;
use crate::persistence::AcpSessionSyncService;
use crate::registry::AgentRegistry;
use crate::task_manager::IWorkerTaskManager;

pub struct AgentService {
    task_manager: Arc<dyn IWorkerTaskManager>,
    registry: Arc<AgentRegistry>,
    #[allow(dead_code)]
    conversation_repo: Arc<dyn IConversationRepository>,
    #[allow(dead_code)]
    acp_session_sync: Arc<AcpSessionSyncService>,
}

impl AgentService {
    pub fn new(
        task_manager: Arc<dyn IWorkerTaskManager>,
        registry: Arc<AgentRegistry>,
        conversation_repo: Arc<dyn IConversationRepository>,
        acp_session_sync: Arc<AcpSessionSyncService>,
    ) -> Arc<Self> {
        Arc::new(Self {
            task_manager,
            registry,
            conversation_repo,
            acp_session_sync,
        })
    }

    // Private helper — move logic from routes::session_ops::get_task verbatim
    fn task(&self, conversation_id: &str) -> Result<AgentInstance, AppError> {
        self.task_manager
            .get_task(conversation_id)
            .ok_or_else(|| AppError::NotFound(format!("No active agent for conversation '{conversation_id}'")))
    }

    pub async fn get_mode(&self, conversation_id: &str) -> Result<AgentModeResponse, AppError> {
        let instance = self.task(conversation_id)?;
        instance.get_mode().await
    }

    pub async fn set_mode(&self, conversation_id: &str, req: SetModeRequest) -> Result<(), AppError> {
        if req.mode.trim().is_empty() {
            return Err(AppError::BadRequest("mode must not be empty".into()));
        }
        let instance = self.task(conversation_id)?;
        instance.set_mode(&req.mode).await
    }

    pub async fn get_model(&self, conversation_id: &str) -> Result<GetModelInfoResponse, AppError> {
        let instance = self.task(conversation_id)?;
        let AgentInstance::Acp(acp) = &instance else {
            return Err(AppError::BadRequest(
                "Model info is only available for ACP agents".into(),
            ));
        };
        let sdk_model = acp.model_info().await;
        let model_info = sdk_model.map(map_sdk_model_to_payload);
        Ok(GetModelInfoResponse { model_info })
    }

    pub async fn set_model(&self, conversation_id: &str, req: SetModelRequest) -> Result<(), AppError> {
        if req.model_id.trim().is_empty() {
            return Err(AppError::BadRequest("model_id must not be empty".into()));
        }
        let instance = self.task(conversation_id)?;
        let AgentInstance::Acp(acp) = &instance else {
            return Err(AppError::BadRequest(
                "Model switching is not supported for this agent type".into(),
            ));
        };
        acp.set_model_info(&req.model_id).await
    }

    pub async fn get_config(
        &self,
        conversation_id: &str,
        config_id: &str,
    ) -> Result<Option<agent_client_protocol::schema::SessionConfigOption>, AppError> {
        let instance = self.task(conversation_id)?;
        let AgentInstance::Acp(acp) = &instance else {
            return Err(AppError::BadRequest(
                "Config options are only available for ACP agents".into(),
            ));
        };
        let found = acp
            .config_options()
            .await
            .into_iter()
            .find(|opt| *opt.id.0 == *config_id);
        Ok(found)
    }

    pub async fn set_config(
        &self,
        conversation_id: &str,
        config_id: &str,
        req: SetConfigOptionRequest,
    ) -> Result<(), AppError> {
        let instance = self.task(conversation_id)?;
        let AgentInstance::Acp(acp) = &instance else {
            return Err(AppError::BadRequest(
                "Config updates are not supported for this agent type".into(),
            ));
        };
        acp.set_config_option(config_id, &req.value).await
    }

    pub async fn get_configs_batch(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<agent_client_protocol::schema::SessionConfigOption>, AppError> {
        let instance = self.task(conversation_id)?;
        let AgentInstance::Acp(acp) = &instance else {
            return Err(AppError::BadRequest(
                "Config options are only available for ACP agents".into(),
            ));
        };
        Ok(acp.config_options().await)
    }

    pub async fn set_configs_batch(&self, conversation_id: &str, req: SetConfigOptionsRequest) -> Result<(), AppError> {
        let instance = self.task(conversation_id)?;
        let AgentInstance::Acp(acp) = &instance else {
            return Err(AppError::BadRequest(
                "Config updates are not supported for this agent type".into(),
            ));
        };
        for update in req.config_options {
            if update.config_id.trim().is_empty() {
                return Err(AppError::BadRequest("config_id must not be empty".into()));
            }
            acp.set_config_option(&update.config_id, &update.value).await?;
        }
        Ok(())
    }

    pub async fn get_usage(
        &self,
        conversation_id: &str,
    ) -> Result<Option<agent_client_protocol::schema::UsageUpdate>, AppError> {
        let instance = self.task(conversation_id)?;
        let AgentInstance::Acp(acp) = &instance else {
            return Err(AppError::BadRequest(
                "Usage stats are only available for ACP agents".into(),
            ));
        };
        Ok(acp.usage().await)
    }

    pub async fn get_agent_capabilities(
        &self,
        conversation_id: &str,
    ) -> Result<Option<agent_client_protocol::schema::AgentCapabilities>, AppError> {
        let instance = self.task(conversation_id)?;
        let AgentInstance::Acp(acp) = &instance else {
            return Err(AppError::BadRequest(
                "Agent capabilities are only available for ACP agents".into(),
            ));
        };
        Ok(acp.agent_capabilities().await)
    }

    /// Returns slash commands for ACP agents; returns an empty list for
    /// other agent types (the UI renders "no commands").
    pub async fn get_slash_commands(&self, conversation_id: &str) -> Result<Vec<SlashCommandItem>, AppError> {
        let instance = self.task(conversation_id)?;
        let AgentInstance::Acp(acp) = &instance else {
            return Ok(Vec::new());
        };
        acp.load_slash_commands().await
    }

    /// Side-question endpoint. **Placeholder** — see `tmp/refactoring/1-aionui-ai-agent-review.md` §m7.
    /// Current behaviour: trim-checks the question, returns `unsupported` for non-ACP
    /// agents, returns `unsupported` for ACP agents whose behavior_policy disables it,
    /// otherwise returns a stub "will be fully wired in app integration phase" answer.
    /// Tracked for implement-or-delete decision outside this refactor.
    pub async fn handle_side_question(
        &self,
        conversation_id: &str,
        req: SideQuestionRequest,
    ) -> Result<SideQuestionResponse, AppError> {
        if req.question.trim().is_empty() {
            return Err(AppError::BadRequest("question must not be empty".into()));
        }
        let instance = self.task(conversation_id)?;
        let AgentInstance::Acp(acp) = &instance else {
            return Ok(SideQuestionResponse {
                status: "unsupported".into(),
                answer: None,
            });
        };
        if !acp.supports_side_question() {
            return Ok(SideQuestionResponse {
                status: "unsupported".into(),
                answer: None,
            });
        }
        Ok(SideQuestionResponse {
            status: "ok".into(),
            answer: Some("Side question support will be fully wired in app integration phase.".into()),
        })
    }

    pub async fn get_openclaw_runtime(&self, conversation_id: &str) -> Result<serde_json::Value, AppError> {
        let instance = self.task(conversation_id)?;
        let AgentInstance::OpenClaw(openclaw) = &instance else {
            return Err(AppError::BadRequest(
                "This endpoint is only available for OpenClaw agents".into(),
            ));
        };
        Ok(openclaw.get_diagnostics().await)
    }
}

// Conversation-related operations
impl AgentService {
    /// Reload-context endpoint. **Placeholder** — see `tmp/refactoring/1-aionui-ai-agent-review.md` §m7.
    /// Current behaviour: confirms an active agent exists for the conversation;
    /// does not actually reload anything. Tracked for implement-or-delete decision
    /// outside this refactor.
    pub async fn reload_context(&self, conversation_id: &str) -> Result<(), AppError> {
        let _instance = self.task(conversation_id)?;
        Ok(())
    }

    pub async fn browse_workspace(
        &self,
        conversation_id: &str,
        query: WorkspaceBrowseQuery,
    ) -> Result<Vec<WorkspaceEntry>, AppError> {
        if query.path.trim().is_empty() {
            return Err(AppError::BadRequest("path must not be empty".into()));
        }

        let row = self
            .conversation_repo
            .get(conversation_id)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to load conversation: {e}")))?
            .ok_or_else(|| AppError::NotFound(format!("Conversation '{conversation_id}' not found")))?;

        let extra: serde_json::Value =
            serde_json::from_str(&row.extra).map_err(|e| AppError::Internal(format!("Invalid extra JSON: {e}")))?;
        let workspace = extra
            .get("workspace")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_owned();
        if workspace.is_empty() {
            return Err(AppError::BadRequest("Conversation has no workspace assigned".into()));
        }

        let relative_path = query.path.trim_start_matches('/');
        let relative_path_obj = std::path::Path::new(relative_path);
        if relative_path_obj
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(AppError::BadRequest(
                "Path traversal outside workspace is not allowed".into(),
            ));
        }

        // Resolve the browsed path relative to the workspace root
        let base = std::path::Path::new(&workspace);
        let browse_path = if relative_path.is_empty() {
            base.to_path_buf()
        } else {
            base.join(relative_path_obj)
        };

        // Security: reject direct traversal outside the workspace root, but allow
        // symlinked directories mounted inside the workspace (e.g. native skill
        // dirs that point at the builtin skills corpus under data-dir).
        let canonical_base = base
            .canonicalize()
            .map_err(|e| AppError::Internal(format!("Failed to resolve workspace path: {e}")))?;
        let canonical_browse = browse_path
            .canonicalize()
            .map_err(|_| AppError::NotFound("Directory not found".into()))?;
        if !browse_path.starts_with(base) && !canonical_browse.starts_with(&canonical_base) {
            return Err(AppError::BadRequest(
                "Path traversal outside workspace is not allowed".into(),
            ));
        }

        // Check depth limit
        let depth = relative_path_obj.components().count();
        if depth > MAX_DIR_DEPTH {
            return Err(AppError::BadRequest(format!(
                "Directory depth exceeds maximum of {MAX_DIR_DEPTH}"
            )));
        }

        let mut entries = Vec::new();
        let mut dir_reader = tokio::fs::read_dir(&canonical_browse)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to read directory: {e}")))?;

        while let Ok(Some(entry)) = dir_reader.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();

            // Apply search filter if provided
            if let Some(ref search) = query.search
                && !search.is_empty()
                && !name.to_lowercase().contains(&search.to_lowercase())
            {
                continue;
            }

            let entry_path = entry.path();
            let metadata = tokio::fs::metadata(&entry_path)
                .await
                .map_err(|e| AppError::Internal(format!("Failed to read entry metadata: {e}")))?;

            let entry_type = if metadata.is_dir() { "directory" } else { "file" };

            entries.push(WorkspaceEntry {
                name,
                entry_type: entry_type.into(),
            });
        }

        // Sort: directories first, then alphabetically
        entries.sort_by(|a, b| {
            let type_cmp = a.entry_type.cmp(&b.entry_type);
            if type_cmp == std::cmp::Ordering::Equal {
                a.name.to_lowercase().cmp(&b.name.to_lowercase())
            } else {
                type_cmp
            }
        });

        Ok(entries)
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

    pub fn test_custom_agent(&self, req: TestCustomAgentRequest) -> Result<TestCustomAgentResponse, AppError> {
        crate::protocol::cli_detect::test_custom_agent(&req.command, &req.acp_args, &req.env)
    }
}

// ACP related operations
impl AgentService {
    pub async fn detect_cli(&self, req: DetectCliRequest) -> Result<DetectCliResponse, AppError> {
        Ok(crate::protocol::cli_detect::detect_cli(&self.registry, &req.backend).await)
    }

    pub async fn acp_health_check(&self, req: AcpHealthCheckRequest) -> Result<AcpHealthCheckResponse, AppError> {
        Ok(crate::protocol::cli_detect::health_check(&self.registry, &req.backend).await)
    }

    pub fn acp_env(&self) -> Result<AcpEnvResponse, AppError> {
        Ok(crate::protocol::cli_detect::get_env())
    }

    /// Probe a model. **Placeholder** — returns `None` once CLI availability is
    /// confirmed. Full probing will be wired when integrated with real ACP sessions.
    pub async fn probe_model(&self, req: ProbeModelRequest) -> Result<Option<AcpModelInfo>, AppError> {
        let detection = crate::protocol::cli_detect::detect_cli(&self.registry, &req.backend).await;
        if detection.path.is_none() {
            return Err(AppError::BadRequest(format!(
                "Backend '{}' CLI not found, cannot probe model",
                req.backend
            )));
        }
        Ok(None)
    }
}

fn map_sdk_model_to_payload(m: SessionModelState) -> ModelInfoPayload {
    let available: Vec<ModelInfoEntry> = m
        .available_models
        .iter()
        .map(|am| ModelInfoEntry {
            id: am.model_id.to_string(),
            label: am.name.clone(),
        })
        .collect();
    let current_id = m.current_model_id.to_string();
    let current_label = available
        .iter()
        .find(|e| e.id == current_id)
        .map(|e| e.label.clone())
        .unwrap_or_else(|| current_id.clone());
    ModelInfoPayload {
        current_model_id: Some(current_id),
        current_model_label: Some(current_label),
        available_models: available,
    }
}
