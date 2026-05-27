//! Public conv-layer surface. Single source of truth for conversation
//! runtime state.
//!
//! See `docs/superpowers/specs/2026-05-26-conversation-layer-refactor-design.md`.
//!
//! NOTE: this Phase 2 trait introduces a NEW `aionui_conversation::ConversationStatus`
//! (Idle / Running { msg_id }), which is distinct from the legacy
//! `aionui_common::ConversationStatus` (Pending / Running / Finished). Both
//! coexist for one cycle — the legacy enum is removed in Phase 5.

use aionui_api_types::{
    ConversationListResponse, ConversationResponse, CreateConversationRequest, ListConversationsQuery,
    SendMessageRequest,
};
use aionui_common::AppError;
use tokio::sync::broadcast;

/// Runtime status of a conversation as observed by the conv layer.
///
/// `Idle` is the default for both never-opened and finished conversations —
/// callers that need to distinguish these cases must consult `ConvActor`'s
/// internal `ConvState` directly.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConversationStatus {
    #[default]
    Idle,
    Running {
        msg_id: String,
    },
}

/// Lifecycle event emitted by `IConversationService::subscribe` for a
/// single conversation. Distinct from low-level `AgentStreamEvent` —
/// these are the cross-protocol turn-boundary events biz-layer crates
/// (cron, team) need.
#[derive(Debug, Clone)]
pub enum ConversationEvent {
    TurnStarted {
        msg_id: String,
    },
    Chunk {
        msg_id: String,
        payload: serde_json::Value,
    },
    /// A turn finished. `system_responses` carries any cron-style
    /// continuation hints captured by the relay; the conv layer does
    /// NOT interpret them — biz-layer subscribers (e.g. the cron
    /// orchestrator) decide whether to chain a follow-up `send`.
    /// Empty for ordinary user-initiated turns.
    TurnCompleted {
        msg_id: String,
        system_responses: Vec<String>,
    },
    TurnError {
        msg_id: String,
        error: String,
    },
    TurnCancelled {
        msg_id: String,
    },
}

/// Public conv-layer trait. Biz-layer callers (team / cron / assistant)
/// MUST depend on this trait, not on `ConversationService` directly.
#[async_trait::async_trait]
pub trait IConversationService: Send + Sync {
    async fn create(&self, user_id: &str, opts: CreateConversationRequest) -> Result<String, AppError>;
    async fn delete(&self, user_id: &str, id: &str) -> Result<(), AppError>;
    async fn get(&self, user_id: &str, id: &str) -> Result<ConversationResponse, AppError>;
    async fn list(&self, user_id: &str, q: ListConversationsQuery) -> Result<ConversationListResponse, AppError>;

    async fn warmup(&self, user_id: &str, id: &str) -> Result<(), AppError>;

    /// Returns msg_id immediately. Conflict if a turn is already in flight.
    async fn send(&self, user_id: &str, id: &str, req: SendMessageRequest) -> Result<String, AppError>;

    /// Returns only after the in-flight turn has stopped. Idempotent.
    async fn cancel(&self, user_id: &str, id: &str) -> Result<(), AppError>;

    /// Lock-free runtime status read.
    fn status(&self, id: &str) -> ConversationStatus;

    fn subscribe(&self, id: &str) -> broadcast::Receiver<ConversationEvent>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_are_send_sync() {
        fn _assert<T: Send + Sync>() {}
        _assert::<ConversationStatus>();
        _assert::<ConversationEvent>();
    }

    #[test]
    fn iconversation_service_is_object_safe() {
        fn _assert(_: &dyn IConversationService) {}
    }

    #[test]
    fn status_idle_default() {
        assert_eq!(ConversationStatus::default(), ConversationStatus::Idle);
    }

    #[test]
    fn turn_completed_carries_system_responses() {
        let evt = ConversationEvent::TurnCompleted {
            msg_id: "m1".into(),
            system_responses: vec!["next-prompt".into()],
        };
        match evt {
            ConversationEvent::TurnCompleted { system_responses, .. } => {
                assert_eq!(system_responses, vec!["next-prompt".to_owned()]);
            }
            _ => panic!("expected TurnCompleted variant"),
        }
    }
}
