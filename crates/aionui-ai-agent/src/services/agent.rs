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

use std::path::PathBuf;
use std::sync::Arc;

use aionui_api_types::{AcpHealthCheckRequest, AcpHealthCheckResponse, AgentMetadata};
use aionui_common::AppError;

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
}
