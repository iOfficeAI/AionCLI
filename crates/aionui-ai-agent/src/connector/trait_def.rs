//! New connect-layer trait. See spec § Connect layer: `IAgentConnector`.
//!
//! Speaks protocol/process language only. Does **not** import or expose
//! conversation-status types — those belong to the conv layer (see
//! `aionui-conversation`).
//!
//! Object-safe by construction: no generic methods, no `Self` by value.

use aionui_common::{AgentKillReason, AgentType, AppError, TimestampMs};
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

    /// Subscribe to the legacy `AgentStreamEvent` channel — kept for
    /// callers that have not yet migrated. Will be removed in Phase 5.
    fn subscribe_legacy(&self) -> broadcast::Receiver<AgentStreamEvent>;
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
