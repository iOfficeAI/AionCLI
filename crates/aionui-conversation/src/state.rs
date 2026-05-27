use std::sync::Arc;

use crate::service::ConversationService;
use aionui_ai_agent::IAgentConnectorFactory;

/// Shared state for conversation route handlers.
#[derive(Clone)]
pub struct ConversationRouterState {
    pub service: ConversationService,
    pub connector_factory: Arc<dyn IAgentConnectorFactory>,
}
