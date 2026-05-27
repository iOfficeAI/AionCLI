//! Connect-layer trait. See spec § Connect layer: `IAgentConnector`.
//!
//! The trait carries the full agent control surface (turn lifecycle plus
//! mode/model/usage/confirmations) so external callers consume agents
//! exclusively through `Arc<dyn IAgentConnector>`. The
//! `status() -> Option<ConversationStatus>` hook is the one concession to
//! legacy semantics: it is the only piece of conversation-runtime
//! vocabulary that lives here, kept so the idle scanner / collect-idle
//! code paths continue to work against a `dyn IAgentConnector`.
//!
//! Object-safe by construction: no generic methods, no `Self` by value.

use std::pin::Pin;

use aionui_api_types::{
    AgentModeResponse, ConversationStatus, GetModelInfoResponse, SideQuestionRequest, SideQuestionResponse,
    SlashCommandItem,
};
use aionui_common::{AgentKillReason, AgentType, AppError, Confirmation, TimestampMs};
use tokio::sync::broadcast;

use crate::protocol::events::AgentStreamEvent;
use crate::types::SendMessageData;

#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    #[error("connector not open")]
    NotOpen,
    #[error("turn already in flight")]
    Busy,
    #[error("turn cancelled by user")]
    Cancelled,
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("subprocess died: {0}")]
    SubprocessDied(String),
    #[error(transparent)]
    Other(#[from] AppError),
}

#[derive(Debug, Clone, Default)]
pub struct TurnSummary {
    pub session_id: Option<String>,
    pub stop_reason: Option<StopReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    Cancelled,
    MaxTokens,
    MaxTurns,
    Refusal,
    Other(String),
}

#[derive(Debug, Clone)]
pub struct ChunkPayload {
    pub event: AgentStreamEvent,
}

#[derive(Debug, Clone)]
pub struct ToolUsePayload {
    pub call_id: String,
    pub tool_name: String,
}

#[derive(Debug, Clone)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum ConnectorEvent {
    Chunk(ChunkPayload),
    ToolUse(ToolUsePayload),
    StopReason(StopReason),
    SubprocessDied(ExitInfo),
}

#[async_trait::async_trait]
pub trait IAgentConnector: Send + Sync {
    fn agent_type(&self) -> AgentType;
    fn conversation_id(&self) -> &str;
    fn workspace(&self) -> &str;
    fn last_activity_at(&self) -> TimestampMs;
    fn is_open(&self) -> bool;

    async fn open(&self) -> Result<(), ConnectorError>;
    fn close(&self, reason: Option<AgentKillReason>);

    /// Runs one full turn to completion. Returns when the agent emits a
    /// terminal stopReason. Implementations MUST serialize concurrent
    /// callers (defense in depth on top of conv-layer mutex).
    async fn run_turn(&self, msg: SendMessageData) -> Result<TurnSummary, ConnectorError>;

    /// Aborts the in-flight turn. MUST NOT return until the underlying
    /// protocol has acknowledged the stop:
    ///   - ACP: cancel notification + StopReason::Cancelled observed.
    ///   - aionrs: oneshot done channel signalled (engine.run() future fully dropped).
    ///   - remote: HTTP/WS cancel ack returned.
    /// Idempotent: returns Ok if no turn is in flight.
    async fn cancel_current_turn(&self) -> Result<(), ConnectorError>;

    /// Subscribe to the protocol-level connector event stream.
    fn subscribe(&self) -> broadcast::Receiver<ConnectorEvent>;

    /// Subscribe to the token-level [`AgentStreamEvent`] channel that
    /// the conv-layer `StreamRelay` consumes. Distinct from
    /// [`Self::subscribe`], which carries turn-level lifecycle events.
    fn subscribe_legacy(&self) -> broadcast::Receiver<AgentStreamEvent>;

    // ── Lifecycle / control surface ─────────────────────────────────────

    /// Current conversation status. `None` if the agent has not
    /// transitioned into a known status yet.
    fn status(&self) -> Option<ConversationStatus>;

    /// Send a user message to the agent. Returns once the agent has
    /// accepted the turn; actual streaming proceeds on the broadcast
    /// channel returned by [`Self::subscribe_legacy`].
    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError>;

    /// Stop the current streaming response without killing the agent.
    /// `AppError`-flavoured counterpart to [`Self::cancel_current_turn`]
    /// so the conv layer's `agent.cancel().await?` flow does not need
    /// to re-shape its error handling.
    async fn cancel(&self) -> Result<(), AppError>;

    /// Terminate the agent process.
    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError>;

    /// Terminate the agent process and return a future that resolves
    /// when the underlying OS process / WS connection has fully closed.
    fn kill_and_wait(&self, reason: Option<AgentKillReason>) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

    /// Pending confirmation items for this connector. Variants without
    /// a confirmation surface return an empty list.
    fn get_confirmations(&self) -> Vec<Confirmation>;

    /// Submit a confirmation response for a pending tool call.
    fn confirm(&self, msg_id: &str, call_id: &str, data: serde_json::Value, always_allow: bool)
    -> Result<(), AppError>;

    /// Whether an action is auto-approved in this session.
    fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool;

    /// Session key for connectors that expose one (currently
    /// OpenClaw).
    fn get_session_key(&self) -> Option<String>;

    /// Current session mode. Connectors without a modal concept return
    /// `mode = "default"`, `initialized = false`.
    async fn get_mode(&self) -> Result<AgentModeResponse, AppError>;

    /// Set the session mode. Connectors without a modal concept
    /// surface `BadRequest`.
    async fn set_mode(&self, mode: &str) -> Result<(), AppError>;

    /// Current model info (id + label + available list). Connectors
    /// without a model picker return `model_info = None`.
    async fn get_model(&self) -> Result<GetModelInfoResponse, AppError>;

    /// Switch the active model. Connectors without a model picker
    /// surface `BadRequest`.
    async fn set_model(&self, model_id: &str) -> Result<(), AppError>;

    /// Cached session usage as a snake_case JSON object (per the ACP
    /// SDK schema, normalised). Connectors that don't track usage
    /// return `None`.
    async fn get_usage(&self) -> Result<Option<serde_json::Value>, AppError>;

    /// Slash commands available in the current session. Connectors
    /// without a slash-command catalog return an empty list.
    async fn get_slash_commands(&self) -> Result<Vec<SlashCommandItem>, AppError>;

    /// Dispatch a side-question to the agent. Connectors without
    /// side-question support return `status = "unsupported"`.
    async fn handle_side_question(&self, req: SideQuestionRequest) -> Result<SideQuestionResponse, AppError>;

    /// OpenClaw-specific runtime diagnostics. All other connectors
    /// return `Value::Null` so diagnostic UIs degrade gracefully.
    async fn get_openclaw_runtime(&self) -> Result<serde_json::Value, AppError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_event_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ConnectorEvent>();
        assert_send_sync::<ConnectorError>();
        assert_send_sync::<TurnSummary>();
    }

    #[test]
    fn iagentconnector_is_object_safe() {
        // Will fail to compile if the trait is not object-safe.
        fn _assert(_: &dyn IAgentConnector) {}
    }
}
