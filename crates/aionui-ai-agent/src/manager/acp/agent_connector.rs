//! `IAgentConnector` impl for `AcpAgentManager`.
//!
//! Lives in its own module so the parent `agent.rs` stays under the
//! 1000-line per-file budget mandated by `AGENTS.md`. The implementation
//! here speaks protocol/process language only — no conversation-runtime
//! concepts leak into the connector surface.

use std::pin::Pin;
use std::time::Duration;

use agent_client_protocol::schema::{CancelNotification, SessionId};
use aionui_api_types::{
    AgentModeResponse, ConversationStatus, GetModelInfoResponse, ModelInfoEntry, ModelInfoPayload, SideQuestionRequest,
    SideQuestionResponse, SlashCommandItem,
};
use aionui_common::{AgentKillReason, AgentType, AppError, Confirmation, TimestampMs};
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

    // ── Lifecycle / control surface ─────────────────────────────────────
    //
    // Each method delegates to the crate-private `IAgentTask` impl on
    // `Self` or to the inherent helpers on `AcpAgentManager`.

    fn status(&self) -> Option<ConversationStatus> {
        crate::agent_task::IAgentTask::status(self)
    }

    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError> {
        crate::agent_task::IAgentTask::send_message(self, data).await
    }

    async fn cancel(&self) -> Result<(), AppError> {
        crate::agent_task::IAgentTask::cancel(self).await
    }

    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        crate::agent_task::IAgentTask::kill(self, reason)
    }

    fn kill_and_wait(&self, reason: Option<AgentKillReason>) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        AcpAgentManager::kill_and_wait(self, reason)
    }

    /// ACP currently tracks permission prompts inline through the
    /// permission router (not surfaced here), so returns empty —
    /// matches the existing `AgentInstance::Acp(_)` arm.
    fn get_confirmations(&self) -> Vec<Confirmation> {
        Vec::new()
    }

    fn confirm(
        &self,
        msg_id: &str,
        call_id: &str,
        data: serde_json::Value,
        always_allow: bool,
    ) -> Result<(), AppError> {
        AcpAgentManager::confirm(self, msg_id, call_id, data, always_allow)
    }

    /// Mirrors the existing `AgentInstance::Acp(_)` arm, which always
    /// returns `false` (auto-approval lives outside the ACP session).
    fn check_approval(&self, _action: &str, _command_type: Option<&str>) -> bool {
        false
    }

    /// ACP does not expose an externally visible session key; keys are
    /// negotiated internally by the SDK.
    fn get_session_key(&self) -> Option<String> {
        None
    }

    async fn get_mode(&self) -> Result<AgentModeResponse, AppError> {
        AcpAgentManager::mode(self).await
    }

    async fn set_mode(&self, mode: &str) -> Result<(), AppError> {
        AcpAgentManager::set_mode(self, mode).await
    }

    async fn get_model(&self) -> Result<GetModelInfoResponse, AppError> {
        let sdk_model = AcpAgentManager::model(self).await;
        let sdk_info = sdk_model.map(map_sdk_model_to_payload);
        let cc_switch_info = if AcpAgentManager::is_claude_backend(self) {
            crate::cc_switch::read_claude_model_info()
        } else {
            None
        };
        let model_info = merge_model_info(sdk_info, cc_switch_info);
        Ok(GetModelInfoResponse { model_info })
    }

    async fn set_model(&self, model_id: &str) -> Result<(), AppError> {
        if model_id.trim().is_empty() {
            return Err(AppError::BadRequest("model_id must not be empty".into()));
        }
        AcpAgentManager::set_model(self, model_id).await
    }

    async fn get_usage(&self) -> Result<Option<serde_json::Value>, AppError> {
        let Some(usage) = AcpAgentManager::usage(self).await else {
            return Ok(None);
        };
        let mut value =
            serde_json::to_value(usage).map_err(|e| AppError::Internal(format!("Failed to serialize usage: {e}")))?;
        aionui_common::normalize_keys_to_snake_case(&mut value);
        Ok(Some(value))
    }

    async fn get_slash_commands(&self) -> Result<Vec<SlashCommandItem>, AppError> {
        AcpAgentManager::load_slash_commands(self).await
    }

    async fn handle_side_question(&self, req: SideQuestionRequest) -> Result<SideQuestionResponse, AppError> {
        if req.question.trim().is_empty() {
            return Err(AppError::BadRequest("question must not be empty".into()));
        }
        if !AcpAgentManager::supports_side_question(self) {
            return Ok(SideQuestionResponse {
                status: "unsupported".into(),
                answer: None,
            });
        }
        // Mirrors the placeholder behaviour in `AgentInstance::handle_side_question`
        // for the ACP arm. Full wiring lands in the app integration phase.
        Ok(SideQuestionResponse {
            status: "ok".into(),
            answer: Some("Side question support will be fully wired in app integration phase.".into()),
        })
    }

    async fn get_openclaw_runtime(&self) -> Result<serde_json::Value, AppError> {
        Ok(serde_json::Value::Null)
    }
}

/// Map the raw ACP SDK model state into the public API payload.
///
/// Reused inside `IAgentConnector::get_model` above; if the shape of
/// `ModelInfoPayload` changes, update it here. Module-private —
/// previously lived in `agent_task.rs`, relocated when the legacy
/// `IAgentTask` / `AgentInstance` types were deleted.
pub(super) fn map_sdk_model_to_payload(m: agent_client_protocol::schema::SessionModelState) -> ModelInfoPayload {
    let available: Vec<ModelInfoEntry> = m
        .available_models
        .iter()
        .map(|am| ModelInfoEntry {
            id: am.model_id.to_string(),
            label: am.name.clone(),
        })
        .collect();
    let current_id = m.current_model_id.to_string();
    let current_label = available
        .iter()
        .find(|e| e.id == current_id)
        .map(|e| e.label.clone())
        .unwrap_or_else(|| current_id.clone());
    ModelInfoPayload {
        current_model_id: Some(current_id),
        current_model_label: Some(current_label),
        available_models: available,
    }
}

/// Merge ACP-SDK and CC-Switch model info, preferring the SDK payload
/// when both are present.
pub(super) fn merge_model_info(
    sdk_info: Option<ModelInfoPayload>,
    cc_switch_info: Option<ModelInfoPayload>,
) -> Option<ModelInfoPayload> {
    sdk_info.or(cc_switch_info)
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

    // ── merge_model_info: SDK payload preferred, CC-Switch fallback ──

    #[test]
    fn merge_prefers_sdk_model_over_cc_switch() {
        let sdk_payload = ModelInfoPayload {
            current_model_id: Some("default".into()),
            current_model_label: Some("Claude Sonnet 4.6".into()),
            available_models: vec![ModelInfoEntry {
                id: "default".into(),
                label: "Claude Sonnet 4.6".into(),
            }],
        };
        let cc_switch_payload = ModelInfoPayload {
            current_model_id: Some("default".into()),
            current_model_label: Some("DeepSeek V4".into()),
            available_models: vec![ModelInfoEntry {
                id: "default".into(),
                label: "DeepSeek V4".into(),
            }],
        };

        let result = merge_model_info(Some(sdk_payload), Some(cc_switch_payload));
        assert_eq!(
            result.unwrap().current_model_label.as_deref(),
            Some("Claude Sonnet 4.6")
        );
    }

    #[test]
    fn merge_falls_back_to_cc_switch_when_sdk_none() {
        let cc_switch_payload = ModelInfoPayload {
            current_model_id: Some("default".into()),
            current_model_label: Some("DeepSeek V4".into()),
            available_models: vec![ModelInfoEntry {
                id: "default".into(),
                label: "DeepSeek V4".into(),
            }],
        };

        let result = merge_model_info(None, Some(cc_switch_payload));
        assert_eq!(result.unwrap().current_model_label.as_deref(), Some("DeepSeek V4"));
    }

    #[test]
    fn merge_returns_none_when_both_none() {
        let result = merge_model_info(None, None);
        assert!(result.is_none());
    }
}
