use std::any::Any;
use std::sync::Arc;

use aionui_common::{
    AgentKillReason, AgentType, AppError, Confirmation, ConversationStatus, TimestampMs,
};
use tokio::sync::broadcast;

use crate::stream_event::AgentStreamEvent;
use crate::types::SendMessageData;

/// Core trait for managing a single Agent instance.
///
/// Each concrete implementation (ACP, Gemini, OpenClaw, Nanobot, Remote, Aionrs)
/// provides the actual process management and communication logic.
/// All methods must be safe to call from any async task (`Send + Sync`).
#[async_trait::async_trait]
pub trait IAgentManager: Send + Sync {
    /// The type of agent this manager controls.
    fn agent_type(&self) -> AgentType;

    /// Current conversation status as seen by this agent.
    /// Returns `None` if the agent has not been initialized yet.
    fn status(&self) -> Option<ConversationStatus>;

    /// Working directory for this agent session.
    fn workspace(&self) -> &str;

    /// Conversation ID this agent is bound to.
    fn conversation_id(&self) -> &str;

    /// Timestamp (ms) of the last activity (message send, event received).
    fn last_activity_at(&self) -> TimestampMs;

    /// Subscribe to the agent's stream event channel.
    ///
    /// Returns a broadcast receiver that yields [`AgentStreamEvent`] values
    /// as the agent processes a message turn.
    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent>;

    /// Send a user message to the agent.
    ///
    /// This triggers the agent to start processing. Events are emitted
    /// on the broadcast channel returned by [`subscribe`](Self::subscribe).
    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError>;

    /// Stop the current streaming response without killing the agent.
    async fn stop(&self) -> Result<(), AppError>;

    /// Submit a confirmation response for a pending tool call.
    ///
    /// If `always_allow` is `true`, the confirmation's `action` (and optional
    /// `command_type`) are recorded in the session-level approval memory so
    /// that future identical requests can be auto-approved by the frontend.
    fn confirm(
        &self,
        msg_id: &str,
        call_id: &str,
        data: serde_json::Value,
        always_allow: bool,
    ) -> Result<(), AppError>;

    /// Get the list of pending confirmation items.
    fn get_confirmations(&self) -> Vec<Confirmation>;

    /// Check whether an action has been marked "always allow" in this session.
    ///
    /// The approval memory is session-level (cleared when the agent is killed).
    fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool;

    /// Terminate the agent process.
    ///
    /// - `reason: Some(IdleTimeout)` — idle cleanup
    /// - `reason: None` — explicit user/system kill
    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError>;

    /// Downcast helper for accessing type-specific extensions.
    ///
    /// Concrete implementations return `self` so that route handlers can
    /// downcast `AgentManagerHandle` to e.g. `AcpAgentManager` for
    /// ACP-specific operations (mode/model/config management).
    fn as_any(&self) -> &dyn Any;
}

/// Type-erased handle to an agent manager, shareable across async tasks.
pub type AgentManagerHandle = Arc<dyn IAgentManager>;
