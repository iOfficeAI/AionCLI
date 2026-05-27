//! Cron continuation orchestrator.
//!
//! Phase 4 moved the multi-turn continuation loop OUT of the conv layer
//! and into the biz layer. The conv layer's `IConversationService::send`
//! is now strictly single-turn — it dispatches one turn and emits a
//! `ConversationEvent::TurnCompleted { msg_id, system_responses }` on
//! the conversation's broadcast channel.
//!
//! This orchestrator is a per-cron-turn subscriber that:
//!   1. observes `TurnCompleted` events on a single conversation,
//!   2. when `system_responses` is non-empty, issues a follow-up
//!      `IConversationService::send` carrying the joined responses as
//!      a hidden user message,
//!   3. caps the chain at `max_continuations` to bound runaway loops.
//!
//! The orchestrator owns no conversation state of its own — it is a
//! pure event consumer. Each invocation of [`CronContinuationOrchestrator::run`]
//! is scoped to a single cron-triggered turn and exits as soon as a
//! `TurnCompleted` arrives with empty `system_responses`, the cap is
//! reached, the broadcast channel is closed, or a follow-up `send`
//! fails.
//!
//! See `docs/superpowers/specs/2026-05-26-conversation-layer-refactor-design.md`
//! § Phase 4.

use std::sync::Arc;

use aionui_api_types::SendMessageRequest;
use aionui_conversation::{ConversationEvent, IConversationService};
use tokio::sync::broadcast;
use tracing::{debug, warn};

/// Per-cron-turn context the orchestrator carries while running.
///
/// `max_continuations` is the upper bound on follow-up `send`s the
/// orchestrator will issue after the initial cron-triggered turn.
/// A value of `0` disables continuation chaining entirely.
#[derive(Debug, Clone)]
pub struct CronTurnContext {
    pub user_id: String,
    pub conversation_id: String,
    pub max_continuations: usize,
}

/// Default cap on cron-driven continuation turns per cron trigger.
/// Mirrors the legacy conv-layer constant `MAX_CRON_CONTINUATIONS_PER_TURN`.
pub const DEFAULT_MAX_CRON_CONTINUATIONS: usize = 4;

pub struct CronContinuationOrchestrator {
    service: Arc<dyn IConversationService>,
    ctx: CronTurnContext,
}

impl CronContinuationOrchestrator {
    pub fn new(service: Arc<dyn IConversationService>, ctx: CronTurnContext) -> Self {
        Self { service, ctx }
    }

    /// Drives the orchestrator until one of the following happens:
    /// - a `TurnCompleted` event arrives with empty `system_responses`,
    /// - `max_continuations` follow-ups have already been issued,
    /// - the broadcast receiver is closed,
    /// - a follow-up `send` fails.
    ///
    /// All exit paths return cleanly; the orchestrator does not
    /// propagate errors to its caller. Production-visible failures are
    /// surfaced via `warn!` logs without sensitive payloads.
    pub async fn run(self, mut rx: broadcast::Receiver<ConversationEvent>) {
        let mut continuation_count = 0usize;
        loop {
            match rx.recv().await {
                Ok(ConversationEvent::TurnCompleted {
                    system_responses,
                    msg_id,
                }) => {
                    debug!(
                        conversation_id = %self.ctx.conversation_id,
                        msg_id = %msg_id,
                        responses = system_responses.len(),
                        "cron orchestrator observed TurnCompleted"
                    );
                    if system_responses.is_empty() {
                        return;
                    }
                    if continuation_count >= self.ctx.max_continuations {
                        warn!(
                            conversation_id = %self.ctx.conversation_id,
                            max = self.ctx.max_continuations,
                            "cron orchestrator hit continuation cap; ending"
                        );
                        return;
                    }
                    continuation_count += 1;
                    // Note: `content` carries cron-injected continuation
                    // text (e.g. "[System: …]"). It is NOT user-supplied
                    // and not sensitive in the privacy sense, but we
                    // still avoid logging it at info level — only its
                    // count and msg_id appear above.
                    let req = SendMessageRequest {
                        content: system_responses.join("\n"),
                        files: vec![],
                        inject_skills: vec![],
                        hidden: true,
                    };
                    if let Err(e) = self
                        .service
                        .send(&self.ctx.user_id, &self.ctx.conversation_id, req)
                        .await
                    {
                        warn!(
                            conversation_id = %self.ctx.conversation_id,
                            error = %e,
                            "cron orchestrator failed to issue continuation send"
                        );
                        return;
                    }
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        conversation_id = %self.ctx.conversation_id,
                        skipped = n,
                        "cron orchestrator lagged on event stream"
                    );
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_api_types::{
        ConversationListResponse, ConversationResponse, CreateConversationRequest, ListConversationsQuery,
    };
    use aionui_common::AppError;
    use aionui_conversation::ConvConversationStatus;
    use std::sync::Mutex;

    /// Mock `IConversationService` that records `send` calls and exposes
    /// a broadcast sender so tests can drive `TurnCompleted` events on
    /// demand.
    #[derive(Clone)]
    struct MockConvService {
        sends: Arc<Mutex<Vec<RecordedSend>>>,
        tx: broadcast::Sender<ConversationEvent>,
    }

    #[derive(Clone, Debug)]
    struct RecordedSend {
        content: String,
        hidden: bool,
    }

    impl MockConvService {
        fn new() -> Self {
            let (tx, _rx) = broadcast::channel(64);
            Self {
                sends: Arc::new(Mutex::new(Vec::new())),
                tx,
            }
        }

        fn send_count(&self) -> usize {
            self.sends.lock().unwrap().len()
        }

        fn last_send(&self) -> Option<RecordedSend> {
            self.sends.lock().unwrap().last().cloned()
        }

        fn emit_turn_completed(&self, msg_id: &str, system_responses: Vec<String>) {
            // Best-effort: no subscriber yet means the orchestrator
            // hasn't started — tests sleep briefly to wait for it.
            let _ = self.tx.send(ConversationEvent::TurnCompleted {
                msg_id: msg_id.to_owned(),
                system_responses,
            });
        }
    }

    #[async_trait::async_trait]
    impl IConversationService for MockConvService {
        async fn create(&self, _user_id: &str, _opts: CreateConversationRequest) -> Result<String, AppError> {
            unreachable!("not used by orchestrator tests")
        }
        async fn delete(&self, _user_id: &str, _id: &str) -> Result<(), AppError> {
            unreachable!()
        }
        async fn get(&self, _user_id: &str, _id: &str) -> Result<ConversationResponse, AppError> {
            unreachable!()
        }
        async fn list(&self, _user_id: &str, _q: ListConversationsQuery) -> Result<ConversationListResponse, AppError> {
            unreachable!()
        }
        async fn warmup(&self, _user_id: &str, _id: &str) -> Result<(), AppError> {
            Ok(())
        }
        async fn send(&self, _user_id: &str, _id: &str, req: SendMessageRequest) -> Result<String, AppError> {
            self.sends.lock().unwrap().push(RecordedSend {
                content: req.content,
                hidden: req.hidden,
            });
            Ok("mock-msg-id".into())
        }
        async fn cancel(&self, _user_id: &str, _id: &str) -> Result<(), AppError> {
            Ok(())
        }
        fn status(&self, _id: &str) -> ConvConversationStatus {
            ConvConversationStatus::Idle
        }
        fn subscribe(&self, _id: &str) -> broadcast::Receiver<ConversationEvent> {
            self.tx.subscribe()
        }
    }

    fn ctx(max: usize) -> CronTurnContext {
        CronTurnContext {
            user_id: "u".into(),
            conversation_id: "c".into(),
            max_continuations: max,
        }
    }

    /// Wait until `predicate` returns true, polling every 5ms up to 1s.
    /// Returns false if the predicate never becomes true.
    async fn wait_until<F: Fn() -> bool>(predicate: F) -> bool {
        for _ in 0..200 {
            if predicate() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        false
    }

    #[tokio::test]
    async fn empty_system_responses_does_not_send_again() {
        let svc = Arc::new(MockConvService::new());
        let trait_svc: Arc<dyn IConversationService> = svc.clone();
        let orch = CronContinuationOrchestrator::new(trait_svc.clone(), ctx(4));
        let rx = trait_svc.subscribe("c");
        let handle = tokio::spawn(orch.run(rx));

        // First TurnCompleted with no responses — orchestrator should
        // exit immediately without issuing a send.
        svc.emit_turn_completed("m1", vec![]);
        handle.await.unwrap();
        assert_eq!(svc.send_count(), 0);
    }

    #[tokio::test]
    async fn first_turn_with_system_response_triggers_follow_up_send() {
        let svc = Arc::new(MockConvService::new());
        let trait_svc: Arc<dyn IConversationService> = svc.clone();
        let orch = CronContinuationOrchestrator::new(trait_svc.clone(), ctx(4));
        let rx = trait_svc.subscribe("c");
        let handle = tokio::spawn(orch.run(rx));

        // First TurnCompleted carries a system_response: orchestrator
        // must dispatch one follow-up send (hidden = true).
        svc.emit_turn_completed("m1", vec!["next-prompt".into()]);
        assert!(wait_until(|| svc.send_count() >= 1).await);

        // Then emit a clean TurnCompleted to release the orchestrator.
        svc.emit_turn_completed("m2", vec![]);
        handle.await.unwrap();

        assert_eq!(svc.send_count(), 1);
        let last = svc.last_send().unwrap();
        assert_eq!(last.content, "next-prompt");
        assert!(last.hidden, "continuation send must be hidden");
    }

    #[tokio::test]
    async fn caps_continuations_at_max() {
        let svc = Arc::new(MockConvService::new());
        let trait_svc: Arc<dyn IConversationService> = svc.clone();
        let orch = CronContinuationOrchestrator::new(trait_svc.clone(), ctx(2));
        let rx = trait_svc.subscribe("c");
        let handle = tokio::spawn(orch.run(rx));

        // Always emit a non-empty system_response — orchestrator must
        // stop after issuing exactly `max_continuations` (= 2) sends.
        for i in 0..5 {
            svc.emit_turn_completed(&format!("m{i}"), vec!["again".into()]);
            // Yield so the orchestrator gets a chance to drain before
            // the next emit lands in the broadcast buffer.
            tokio::task::yield_now().await;
        }

        // Orchestrator should exit after the cap hits — it observes the
        // 3rd TurnCompleted with the count already at 2 and returns.
        // Drain remaining queue to unblock the recv if needed by sending
        // an empty TurnCompleted; this is harmless because the
        // orchestrator has already returned after hitting the cap.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;

        assert_eq!(svc.send_count(), 2);
    }

    #[tokio::test]
    async fn other_events_are_ignored() {
        let svc = Arc::new(MockConvService::new());
        let trait_svc: Arc<dyn IConversationService> = svc.clone();
        let orch = CronContinuationOrchestrator::new(trait_svc.clone(), ctx(4));
        let rx = trait_svc.subscribe("c");
        let handle = tokio::spawn(orch.run(rx));

        // Non-TurnCompleted events must not cause a send and must not
        // terminate the orchestrator.
        let _ = svc.tx.send(ConversationEvent::TurnStarted { msg_id: "m0".into() });
        let _ = svc.tx.send(ConversationEvent::TurnError {
            msg_id: "m0".into(),
            error: "x".into(),
        });
        let _ = svc.tx.send(ConversationEvent::TurnCancelled { msg_id: "m0".into() });

        // Now finish with a clean TurnCompleted.
        svc.emit_turn_completed("m1", vec![]);
        handle.await.unwrap();
        assert_eq!(svc.send_count(), 0);
    }
}
