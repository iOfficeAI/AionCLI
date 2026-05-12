use aionui_common::AppError;

use super::AgentService;
use crate::agent_task::AgentInstance;

impl AgentService {
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
