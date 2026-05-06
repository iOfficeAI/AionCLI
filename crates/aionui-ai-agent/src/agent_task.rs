//! Minimal public contract for a running agent task.
//!
//! `IAgentTask` captures **only** the operations that every agent type
//! implements identically and that the generic task_manager / idle_scanner /
//! message-flow code actually needs. Anything that is type-specific
//! (session modes, session keys, model switching, config options, pending
//! confirmation lists, approval memory, ACP usage, OpenClaw diagnostics,
//! etc.) lives as **inherent** methods on each concrete `XxxAgentManager`
//! and is reached through the `AgentInstance` enum — forcing every callsite
//! to say out loud which agent type it is addressing.
//!
//! Replaces the old bloated `IAgentManager` trait + `as_any()` downcast
//! pattern (deleted in PR #8c).
use std::sync::Arc;

use aionui_common::{AgentKillReason, AgentType, AppError, ConversationStatus, TimestampMs};
use tokio::sync::broadcast;

use crate::acp_agent::AcpAgentManager;
use crate::aionrs_agent::AionrsAgentManager;
use crate::manager::remote::RemoteAgentManager;
use crate::nanobot_agent::NanobotAgentManager;
use crate::openclaw::OpenClawAgentManager;
use crate::stream_event::AgentStreamEvent;
use crate::types::{AgentStreamChunk, SendMessageData};

#[cfg(any(test, feature = "test-support"))]
use aionui_common::Confirmation;

/// Ten-method public surface every agent type implements identically.
///
/// Object-safe by construction (no generic methods, no `Self` by value).
/// Used by generic lifecycle code (task_manager, idle_scanner, stream
/// fan-out) that genuinely does not care which agent type it is dealing
/// with. For type-specific operations, match on [`AgentInstance`] and
/// call the concrete manager's inherent methods.
#[async_trait::async_trait]
pub trait IAgentTask: Send + Sync {
    /// The type of agent this task controls.
    fn agent_type(&self) -> AgentType;

    /// Conversation ID this task is bound to.
    fn conversation_id(&self) -> &str;

    /// Working directory for this agent session.
    fn workspace(&self) -> &str;

    /// Current conversation status. `None` if the agent has not
    /// transitioned into a known status yet.
    fn status(&self) -> Option<ConversationStatus>;

    /// Timestamp (ms) of the last activity (message send, event received).
    fn last_activity_at(&self) -> TimestampMs;

    /// Subscribe to the agent's stream event channel.
    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent>;

    /// Subscribe to the raw stream chunk channel (used by team scheduler
    /// watchdogs). Default implementation returns a receiver that
    /// immediately closes — only ACP currently publishes chunks.
    fn subscribe_stream(&self) -> broadcast::Receiver<AgentStreamChunk> {
        let (tx, _) = broadcast::channel(1);
        tx.subscribe()
    }

    /// Send a user message to the agent. Returns once the agent has
    /// accepted the turn; actual streaming proceeds on the broadcast
    /// channel returned by [`Self::subscribe`].
    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError>;

    /// Stop the current streaming response without killing the agent.
    async fn stop(&self) -> Result<(), AppError>;

    /// Terminate the agent process.
    ///
    /// - `reason: Some(IdleTimeout)` — idle cleanup
    /// - `reason: None` — explicit user/system kill
    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError>;
}

/// Extended trait used exclusively by the `AgentInstance::Mock` variant so
/// tests can inject richer fake behaviour (pending confirmations, approval
/// memory, fake session keys, etc.) without polluting the production
/// `IAgentTask` contract with trait-level defaults that would be lies for
/// at least one concrete manager.
///
/// Every method has a sensible identity-style default so simple mocks only
/// need to implement the ten `IAgentTask` methods and pick up nothing for
/// free.
#[cfg(any(test, feature = "test-support"))]
#[async_trait::async_trait]
pub trait IMockAgent: IAgentTask {
    fn get_confirmations(&self) -> Vec<Confirmation> {
        Vec::new()
    }
    fn check_approval(&self, _action: &str, _command_type: Option<&str>) -> bool {
        false
    }
    fn confirm(
        &self,
        _msg_id: &str,
        _call_id: &str,
        _data: serde_json::Value,
        _always_allow: bool,
    ) -> Result<(), AppError> {
        Ok(())
    }
    fn get_session_key(&self) -> Option<String> {
        None
    }
    async fn get_mode(&self) -> Result<aionui_api_types::AgentModeResponse, AppError> {
        Ok(aionui_api_types::AgentModeResponse {
            mode: "default".into(),
            initialized: false,
        })
    }
    async fn set_mode(&self, _mode: &str) -> Result<(), AppError> {
        Err(AppError::BadRequest(
            "Mode switching is not supported for this mock".into(),
        ))
    }
}

/// Concrete, closed-set dispatcher for the five agent variants.
///
/// Every generic path holds an `AgentInstance` (not `Arc<dyn IAgentTask>`):
/// this gives us the `IAgentTask` ten-method surface via [`Self::as_task`]
/// **and** lets type-specific routes recover the concrete manager with a
/// single `match` — no `as_any` / `downcast_ref` anywhere. Adding a new
/// agent type means adding a new variant here; every `match` in the
/// codebase then fails to compile until it explicitly handles the new
/// type, which is the compile-time pressure we want.
#[derive(Clone)]
pub enum AgentInstance {
    Acp(Arc<AcpAgentManager>),
    Aionrs(Arc<AionrsAgentManager>),
    OpenClaw(Arc<OpenClawAgentManager>),
    Nanobot(Arc<NanobotAgentManager>),
    Remote(Arc<RemoteAgentManager>),
    /// Test-only trait-object escape hatch used by downstream crates
    /// (conversation/cron/team/app tests) to inject fake agents without
    /// spinning up a real CLI or WebSocket connection. Gated behind
    /// `#[cfg(any(test, feature = "test-support"))]`: production builds
    /// never see this variant, so every `match` in release code can
    /// rely on the five-variant closed set. The trait object is
    /// [`IMockAgent`] (extends `IAgentTask`) so mocks can also override
    /// the enum-level helpers — `get_confirmations`, `check_approval`,
    /// `confirm`, `get_session_key`, `get_mode`, `set_mode`.
    #[cfg(any(test, feature = "test-support"))]
    Mock(Arc<dyn IMockAgent>),
}

impl AgentInstance {
    /// Common `IAgentTask` view, regardless of variant.
    pub fn as_task(&self) -> &dyn IAgentTask {
        match self {
            Self::Acp(m) => m.as_ref(),
            Self::Aionrs(m) => m.as_ref(),
            Self::OpenClaw(m) => m.as_ref(),
            Self::Nanobot(m) => m.as_ref(),
            Self::Remote(m) => m.as_ref(),
            #[cfg(any(test, feature = "test-support"))]
            Self::Mock(m) => m.as_ref(),
        }
    }

    // ── Convenience forwarders ───────────────────────────────────────
    //
    // These stay in the final API (not a migration crutch): they turn
    // `instance.agent_type()` into a direct vtable-free call on the
    // concrete `Arc<XxxManager>`, and they keep callsites terse.

    /// The type of agent this instance controls.
    pub fn agent_type(&self) -> AgentType {
        self.as_task().agent_type()
    }

    /// Conversation ID this task is bound to.
    pub fn conversation_id(&self) -> &str {
        self.as_task().conversation_id()
    }

    /// Working directory for this agent session.
    pub fn workspace(&self) -> &str {
        self.as_task().workspace()
    }

    /// Current conversation status.
    pub fn status(&self) -> Option<ConversationStatus> {
        self.as_task().status()
    }

    /// Timestamp (ms) of the last activity.
    pub fn last_activity_at(&self) -> TimestampMs {
        self.as_task().last_activity_at()
    }

    /// Subscribe to the stream event channel.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.as_task().subscribe()
    }

    /// Subscribe to the raw stream chunk channel.
    pub fn subscribe_stream(&self) -> broadcast::Receiver<AgentStreamChunk> {
        self.as_task().subscribe_stream()
    }

    /// Send a user message to the agent.
    pub async fn send_message(&self, data: SendMessageData) -> Result<(), AppError> {
        self.as_task().send_message(data).await
    }

    /// Stop the current streaming response without killing the agent.
    pub async fn stop(&self) -> Result<(), AppError> {
        self.as_task().stop().await
    }

    /// Terminate the agent process.
    pub fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        self.as_task().kill(reason)
    }

    // ── Cross-variant semi-specific helpers ──────────────────────────
    //
    // These fan out to inherent methods on concrete managers. Variants
    // that don't support the operation return a sensible zero-value
    // rather than an error: "no pending confirmations" and "no session
    // key" are honest statements about those variants.

    /// Pending confirmation items for this task.
    ///
    /// ACP currently tracks permission prompts inline through the
    /// permission router (not surfaced here), so returns empty.
    /// Aionrs / OpenClaw / Remote maintain inline confirmation lists.
    /// Nanobot has no concept of confirmations.
    pub fn get_confirmations(&self) -> Vec<aionui_common::Confirmation> {
        match self {
            Self::Acp(_) => Vec::new(),
            Self::Aionrs(m) => m.get_confirmations(),
            Self::OpenClaw(m) => m.get_confirmations(),
            Self::Nanobot(_) => Vec::new(),
            Self::Remote(m) => m.get_confirmations(),
            #[cfg(any(test, feature = "test-support"))]
            Self::Mock(m) => m.get_confirmations(),
        }
    }

    /// Submit a confirmation response for a pending tool call.
    pub fn confirm(
        &self,
        msg_id: &str,
        call_id: &str,
        data: serde_json::Value,
        always_allow: bool,
    ) -> Result<(), AppError> {
        match self {
            Self::Acp(m) => m.confirm(msg_id, call_id, data, always_allow),
            Self::Aionrs(m) => m.confirm(msg_id, call_id, data, always_allow),
            Self::OpenClaw(m) => m.confirm(msg_id, call_id, data, always_allow),
            Self::Nanobot(m) => m.confirm(msg_id, call_id, data, always_allow),
            Self::Remote(m) => m.confirm(msg_id, call_id, data, always_allow),
            #[cfg(any(test, feature = "test-support"))]
            Self::Mock(m) => m.confirm(msg_id, call_id, data, always_allow),
        }
    }

    /// Check whether an action is auto-approved in this session.
    pub fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool {
        match self {
            Self::Acp(_) => false,
            Self::Aionrs(m) => m.check_approval(action, command_type),
            Self::OpenClaw(m) => m.check_approval(action, command_type),
            Self::Nanobot(_) => false,
            Self::Remote(m) => m.check_approval(action, command_type),
            #[cfg(any(test, feature = "test-support"))]
            Self::Mock(m) => m.check_approval(action, command_type),
        }
    }

    /// Session key for agent types that expose one (currently OpenClaw).
    pub fn get_session_key(&self) -> Option<String> {
        match self {
            Self::OpenClaw(m) => m.get_session_key(),
            Self::Acp(_) | Self::Aionrs(_) | Self::Nanobot(_) | Self::Remote(_) => None,
            #[cfg(any(test, feature = "test-support"))]
            Self::Mock(m) => m.get_session_key(),
        }
    }

    /// Get the current session mode. Only ACP and Aionrs model a mode;
    /// other variants report `mode = "default"`, `initialized = false`
    /// so cron / UI can skip mode reconciliation.
    pub async fn get_mode(&self) -> Result<aionui_api_types::AgentModeResponse, AppError> {
        match self {
            Self::Acp(m) => m.get_mode().await,
            Self::Aionrs(m) => m.get_mode().await,
            Self::OpenClaw(_) | Self::Nanobot(_) | Self::Remote(_) => Ok(aionui_api_types::AgentModeResponse {
                mode: "default".into(),
                initialized: false,
            }),
            #[cfg(any(test, feature = "test-support"))]
            Self::Mock(m) => m.get_mode().await,
        }
    }

    /// Set the session mode. Unsupported for variants other than ACP /
    /// Aionrs — returns a `BadRequest` so the caller can surface an
    /// actionable error rather than silently no-op.
    pub async fn set_mode(&self, mode: &str) -> Result<(), AppError> {
        match self {
            Self::Acp(m) => m.set_mode(mode).await,
            Self::Aionrs(m) => m.set_mode(mode).await,
            Self::OpenClaw(_) | Self::Nanobot(_) | Self::Remote(_) => Err(AppError::BadRequest(
                "Mode switching is not supported for this agent type".into(),
            )),
            #[cfg(any(test, feature = "test-support"))]
            Self::Mock(m) => m.set_mode(mode).await,
        }
    }
}
