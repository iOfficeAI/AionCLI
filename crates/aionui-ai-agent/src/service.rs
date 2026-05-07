//! Business-logic layer for the ai-agent crate.
//!
//! Per `AGENTS.md` "Domain Crate Structure", this is the sole location
//! for agent-related business logic. HTTP handlers in `routes/` should
//! only extract inputs, call methods on this service, and wrap the
//! result in `ApiResponse`. Methods will be added in Stage 2b–2f.

use std::sync::Arc;

use agent_client_protocol::schema::SessionModelState;
use aionui_api_types::{
    AgentModeResponse, GetModelInfoResponse, ModelInfoEntry, ModelInfoPayload, SetModeRequest, SetModelRequest,
};
use aionui_common::AppError;
use aionui_db::IConversationRepository;

use crate::agent_task::AgentInstance;
use crate::persistence::AcpSessionSyncService;
use crate::registry::AgentRegistry;
use crate::task_manager::IWorkerTaskManager;

pub struct AgentService {
    task_manager: Arc<dyn IWorkerTaskManager>,
    // Used by methods added in Stage 2c–2f; suppress dead_code until then.
    #[allow(dead_code)]
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

    pub async fn get_model_info(&self, conversation_id: &str) -> Result<GetModelInfoResponse, AppError> {
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
