//! Internal lifecycle trait shared by every concrete agent manager.
//!
//! Phase 5 reshaped the public surface: external crates now consume agents
//! through `Arc<dyn IAgentConnector>` (see `connector::trait_def`) rather
//! than the `AgentInstance` enum + `IAgentTask` trait pair this file used
//! to expose. The remaining `IAgentTask` trait stays as a **crate-private
//! shared base** so each `XxxAgentManager`'s `IAgentConnector` impl can
//! delegate the four lifecycle operations whose body is identical across
//! variants (`status` / `send_message` / `cancel` / `kill`) without
//! copy-pasting the implementation into each manager.
//!
//! Production callers must not reach this trait — `lib.rs` no longer
//! re-exports it. The `AgentInstance` enum, `IMockAgent` trait, and
//! `WorkerTaskManagerImpl` were deleted in the same phase; their tests
//! and helpers either moved to the new test fixtures
//! (`crate::test_support`) or to the connector impls
//! (`crate::manager::acp::agent_connector` for the ACP-specific
//! `map_sdk_model_to_payload` / `merge_model_info`).
//!
//! The trait's surface is intentionally minimal — we only keep methods
//! that are actually dispatched through `IAgentTask` from another
//! module. The five "trivial getters" (`agent_type` / `conversation_id`
//! / `workspace` / `last_activity_at` / `subscribe`) used to live here
//! too, but every `IAgentConnector` impl now reads those values directly
//! from the manager's fields, so keeping them on the trait would be
//! dead code.

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
