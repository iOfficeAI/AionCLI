//! Internal lifecycle trait shared by every concrete agent manager.
//!
//! External crates consume agents through `Arc<dyn IAgentConnector>`
//! (see `connector::trait_def`). `IAgentTask` is a **crate-private
//! shared base** that each `XxxAgentManager`'s `IAgentConnector` impl
//! delegates to for the four lifecycle operations whose body is
//! identical across variants (`status` / `send_message` / `cancel` /
//! `kill`), avoiding copy-pasted bodies.
//!
//! Production callers must not reach this trait — `lib.rs` does not
//! re-export it. The trait's surface is intentionally minimal: the
//! "trivial getters" (`agent_type` / `conversation_id` / `workspace` /
//! `last_activity_at` / `subscribe`) live directly on each manager
//! rather than on this trait so they don't become dead code on the
//! shared base.

use aionui_api_types::ConversationStatus;
use aionui_common::{AgentKillReason, AppError};

use crate::types::SendMessageData;

/// Crate-private lifecycle trait implemented by every concrete agent
/// manager so the per-manager `IAgentConnector` impl can route
/// status / send / cancel / kill through a single shared body.
#[async_trait::async_trait]
pub(crate) trait IAgentTask: Send + Sync {
    /// Current conversation status. `None` if the agent has not
    /// transitioned into a known status yet.
    fn status(&self) -> Option<ConversationStatus>;

    /// Send a user message to the agent. Returns once the agent has
    /// accepted the turn; actual streaming proceeds on the broadcast
    /// channel exposed by `IAgentConnector::subscribe_legacy`.
    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError>;

    /// Stop the current streaming response without killing the agent.
    async fn cancel(&self) -> Result<(), AppError>;

    /// Terminate the agent process.
    ///
    /// - `reason: Some(IdleTimeout)` — idle cleanup
    /// - `reason: None` — explicit user/system kill
    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError>;
}
