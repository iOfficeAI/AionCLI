//! `IAgentConnector` impl for `AcpAgentManager`.
//!
//! Lives in its own module so the parent `agent.rs` stays under the
//! 1000-line per-file budget mandated by `AGENTS.md`. The implementation
//! here speaks protocol/process language only — no conversation-runtime
//! concepts leak into the connector surface.

use std::time::Duration;

use agent_client_protocol::schema::{CancelNotification, SessionId};
use aionui_common::{AgentKillReason, AgentType, AppError, ConversationStatus, TimestampMs};
use tokio::sync::broadcast;

use crate::connector::{ChunkPayload, ConnectorError, ConnectorEvent, IAgentConnector, StopReason, TurnSummary};
use crate::protocol::error::CloseReason;
use crate::protocol::events::{AgentStreamEvent, FinishEventData};
use crate::types::SendMessageData;

use super::agent::AcpAgentManager;

#[async_trait::async_trait]
impl IAgentConnector for AcpAgentManager {
    fn agent_type(&self) -> AgentType {
        AgentType::Acp
    }
    fn conversation_id(&self) -> &str {
        &self.params.conversation_id
    }
    fn workspace(&self) -> &str {
        &self.params.workspace.path
    }
    fn last_activity_at(&self) -> TimestampMs {
        self.runtime.last_activity_at()
    }
    fn is_open(&self) -> bool {
        // Session is opened if we hold a session_id. Read-only check.
        self.session
            .try_read()
            .map(|s| s.session_id().is_some())
            .unwrap_or(false)
    }

    async fn open(&self) -> Result<(), ConnectorError> {
        self.warmup_session().await.map_err(ConnectorError::Other)
    }

    fn close(&self, reason: Option<AgentKillReason>) {
        let _ = crate::agent_task::IAgentTask::kill(self, reason);
    }

    async fn run_turn(&self, msg: SendMessageData) -> Result<TurnSummary, ConnectorError> {
        // Single-flight is enforced inside ensure_session_and_send via the
        // session write-lock + ACP CLI's own per-session serialization.
        match crate::agent_task::IAgentTask::send_message(self, msg).await {
            Ok(()) => Ok(TurnSummary {
                session_id: self.session.read().await.session_id().map(ToOwned::to_owned),
                stop_reason: Some(StopReason::EndTurn),
            }),
            Err(AppError::Conflict(_)) => Err(ConnectorError::Busy),
            Err(e) => Err(ConnectorError::Protocol(format!("{e}"))),
        }
    }

    async fn cancel_current_turn(&self) -> Result<(), ConnectorError> {
        let session_id = self.session.read().await.session_id().map(ToOwned::to_owned);
        let Some(sid) = session_id else {
            return Ok(()); // No session, nothing to cancel.
        };
        self.protocol
            .cancel(CancelNotification::new(SessionId::new(sid.as_str())));
        self.permission_router.cancel_all();

        // Bound the wait so a pathological CLI hang does not deadlock the
        // conv layer. 5s is generous — the SDK normally returns within
        // ~50ms of receiving the cancel notification.
        let _ = tokio::time::timeout(Duration::from_secs(5), self.cancel_ack.notified()).await;

        // Mirror the existing IAgentTask::cancel side-effects so legacy
        // and new paths agree.
        {
            let mut session = self.session.write().await;
            session.record_close_reason(Some(CloseReason::UserCancel));
        }
        self.runtime.reset_for_new_turn(ConversationStatus::Finished);
        self.runtime
            .emit(AgentStreamEvent::Finish(FinishEventData { session_id: None }));
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<ConnectorEvent> {
        let (tx, rx) = broadcast::channel(64);
        let mut legacy = self.runtime.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = legacy.recv().await {
                let _ = tx.send(ConnectorEvent::Chunk(ChunkPayload { event: ev }));
            }
        });
        rx
    }

    fn subscribe_legacy(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.runtime.subscribe()
    }
}

#[cfg(test)]
mod connector_tests {
    use super::*;

    /// Compile-time check: `AcpAgentManager` implements `IAgentConnector`.
    /// This locks the trait surface in. Behavioral cancel-ack invariants are
    /// covered below by directly exercising the `Notify` wiring without
    /// requiring a live ACP CLI subprocess (the fixture cost would be
    /// prohibitive for what is essentially a notify-await pattern check).
    #[test]
    fn acp_manager_implements_iagent_connector() {
        fn _assert<T: IAgentConnector>() {}
        _assert::<AcpAgentManager>();
    }

    /// Behavioural test for the cancel_ack wiring used by
    /// `IAgentConnector::cancel_current_turn`. The production code
    /// invokes `tokio::time::timeout(.., self.cancel_ack.notified())`.
    /// We cannot drive a real ACP session in unit tests (no CLI), so we
    /// reproduce the same pattern directly: a cancel-waiter awaits the
    /// notify, the test confirms it does not return until the notify is
    /// fired, then signals and confirms unblock.
    #[tokio::test]
    async fn cancel_ack_notify_pattern_blocks_until_signalled() {
        use std::sync::Arc;
        use tokio::sync::Notify;

        let cancel_ack = Arc::new(Notify::new());
        let acked = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let waiter = {
            let cancel_ack = cancel_ack.clone();
            let acked = acked.clone();
            tokio::spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(5), cancel_ack.notified()).await;
                acked.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        };

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !acked.load(std::sync::atomic::Ordering::SeqCst),
            "cancel waiter unblocked before ack notify fired"
        );

        cancel_ack.notify_waiters();
        waiter.await.unwrap();
        assert!(acked.load(std::sync::atomic::Ordering::SeqCst));
    }
}
