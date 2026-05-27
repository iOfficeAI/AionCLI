//! Per-conversation runtime owner.
//!
//! See `docs/superpowers/specs/2026-05-26-conversation-layer-refactor-design.md`
//! § ConvActor for design rationale. Closing the cancel→send race
//! (ELECTRON-1KB family) is the load-bearing job here.
//!
//! State machine:
//!
//! ```text
//! NeverOpened ──mark_idle──▶ Idle
//!                              │
//!                              ▼
//!                          begin_turn ──▶ Running { msg_id }
//!                              ▲                   │
//!                              │             TurnHandle dropped
//!                              │                   │
//!                              └──── wait_for_idle ◀
//! ```
//!
//! `wait_for_idle()` is what `IConversationService::cancel` calls AFTER
//! the connector ack — returning only once the running turn has fully
//! released its slot.

use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use aionui_common::AppError;
use tokio::sync::{Mutex, Notify, broadcast};

use crate::conv_service_trait::{ConversationEvent, ConversationStatus};

/// Internal state machine for a single conversation.
///
/// Variants are kept simple by lifting the `turn_done` synchronisation
/// out into `idle_notify` on the parent actor — that way `Running`
/// can be cloned for diagnostic dumps if needed without dragging a
/// non-Clone receiver along.
#[derive(Debug, Clone)]
pub enum ConvState {
    /// Created but never warmed up. Same observable behaviour as
    /// `Idle` from the public-status perspective; tracked separately
    /// so future idle-scanner / metrics code can distinguish "never
    /// used" from "used and finished".
    NeverOpened,
    /// No turn in flight.
    Idle,
    /// A turn is running.
    Running { msg_id: String },
}

/// Per-conversation runtime owner.
///
/// Lifecycle: created on first access in `ConversationService`, lives
/// for as long as the conversation row exists, dropped on
/// `delete()`. The `state` mutex is the single serializer for
/// concurrent send/cancel.
pub struct ConvActor {
    pub id: String,
    /// Source-of-truth state. Held under tokio Mutex because callers
    /// frequently need to await while holding the slot during
    /// transitions.
    pub state: Mutex<ConvState>,
    /// Lifecycle event fan-out for this conversation.
    pub event_tx: broadcast::Sender<ConversationEvent>,
    /// Lock-free read shadow of `state`'s public projection. Updated
    /// inside the same critical sections that mutate `state`. Std
    /// Mutex is fine — these critical sections are tiny and never
    /// span an await.
    public_status: StdMutex<ConversationStatus>,
    /// Notified by `TurnHandle::Drop` once a turn has fully released
    /// the slot. `wait_for_idle` parks here. A `Notify` is the right
    /// primitive: re-entry / multiple waiters / no-op when nothing
    /// waiting are all handled correctly.
    idle_notify: Notify,
}

/// RAII guard returned by [`ConvActor::begin_turn`]. Holding it keeps
/// the conversation in `Running`; dropping it transitions state back
/// to `Idle` (synchronously, via `try_lock`) and notifies any waiter
/// in `wait_for_idle`.
///
/// We use `try_lock` in `Drop` because `Drop` cannot await; the
/// design guarantees that during a turn-end transition no other
/// caller is contending for `state` (the turn task is single-owner
/// of the slot). On the rare contention case we still notify so
/// `wait_for_idle` will re-check on the next iteration.
pub struct TurnHandle {
    actor: Arc<ConvActor>,
    /// `None` once `Drop` has fired — guards against accidental
    /// double-execution if the type ever grows manual `drop` paths.
    armed: bool,
}

impl Drop for TurnHandle {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;

        // Synchronous transition. The turn task is the sole owner of
        // the running slot, so this should never contend.
        if let Ok(mut guard) = self.actor.state.try_lock() {
            *guard = ConvState::Idle;
            self.actor.write_public(ConversationStatus::Idle);
        } else {
            // Defence in depth: if some other path is briefly holding
            // the lock, leave a Notify trail so `wait_for_idle` will
            // re-check. The lock's holder must transition the state
            // itself (only `wait_for_idle` ever holds it during turn
            // end, and it already settles to Idle on its own path).
        }
        self.actor.idle_notify.notify_waiters();
    }
}

impl ConvActor {
    pub fn new(id: String) -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(256);
        Arc::new(Self {
            id,
            state: Mutex::new(ConvState::NeverOpened),
            event_tx,
            public_status: StdMutex::new(ConversationStatus::Idle),
            idle_notify: Notify::new(),
        })
    }

    /// Lock-free read of the public status projection.
    pub fn public_status(&self) -> ConversationStatus {
        match self.public_status.lock() {
            Ok(guard) => guard.clone(),
            // A poisoned lock means we panicked while updating the
            // shadow. Treat as Idle — safer than propagating panic.
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ConversationEvent> {
        self.event_tx.subscribe()
    }

    /// Mark the actor as idle. Used when warmup has succeeded but no
    /// turn has run yet, and by tests.
    pub async fn mark_idle(self: &Arc<Self>) {
        let mut guard = self.state.lock().await;
        *guard = ConvState::Idle;
        self.write_public(ConversationStatus::Idle);
    }

    /// Acquire the running slot. Returns a `TurnHandle` whose `Drop`
    /// transitions state back to `Idle` and notifies any waiter.
    ///
    /// Returns `AppError::Conflict` if a turn is already in flight —
    /// this is the structural replacement for the legacy DB-status
    /// 409 guard.
    pub async fn begin_turn(self: &Arc<Self>, msg_id: String) -> Result<TurnHandle, AppError> {
        let mut guard = self.state.lock().await;
        match *guard {
            ConvState::Running { .. } => Err(AppError::Conflict(
                "Conversation is already processing a message".into(),
            )),
            ConvState::Idle | ConvState::NeverOpened => {
                *guard = ConvState::Running { msg_id: msg_id.clone() };
                self.write_public(ConversationStatus::Running { msg_id: msg_id.clone() });
                // Best-effort: subscribers may have dropped — that is
                // fine, we still want the slot reserved.
                let _ = self.event_tx.send(ConversationEvent::TurnStarted { msg_id });
                Ok(TurnHandle {
                    actor: self.clone(),
                    armed: true,
                })
            }
        }
    }

    /// Wait until any in-flight turn has fully released its slot.
    /// Returns immediately if already idle. Idempotent.
    ///
    /// `IConversationService::cancel` calls this AFTER the connector
    /// has acknowledged the stop. The waker is the `idle_notify`
    /// notify; the publisher is `TurnHandle::Drop`.
    pub async fn wait_for_idle(self: &Arc<Self>) {
        loop {
            // Acquire a notify ticket BEFORE the state check so we
            // cannot miss a notify that fires between check and park.
            let notified = self.idle_notify.notified();
            tokio::pin!(notified);
            // Enable() must be called before the state check.
            notified.as_mut().enable();

            {
                let guard = self.state.lock().await;
                match *guard {
                    ConvState::Idle | ConvState::NeverOpened => {
                        // Snap public projection just in case it
                        // drifted (NeverOpened externally == Idle).
                        self.write_public(ConversationStatus::Idle);
                        return;
                    }
                    ConvState::Running { .. } => {
                        // Drop the lock before parking.
                    }
                }
            }

            notified.await;
            // Loop and re-check. A spurious notify (or one for a
            // turn that immediately respawned) sends us around again.
        }
    }

    fn write_public(&self, status: ConversationStatus) {
        match self.public_status.lock() {
            Ok(mut guard) => *guard = status,
            Err(mut poisoned) => **poisoned.get_mut() = status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    fn fresh_actor() -> Arc<ConvActor> {
        ConvActor::new("conv-1".to_owned())
    }

    #[tokio::test]
    async fn new_actor_is_never_opened() {
        let actor = fresh_actor();
        assert!(matches!(*actor.state.lock().await, ConvState::NeverOpened));
        assert_eq!(actor.public_status(), ConversationStatus::Idle);
    }

    #[tokio::test]
    async fn begin_turn_succeeds_when_idle() {
        let actor = fresh_actor();
        actor.mark_idle().await;
        let handle = actor.begin_turn("msg-1".into()).await.unwrap();
        assert_eq!(
            actor.public_status(),
            ConversationStatus::Running { msg_id: "msg-1".into() }
        );
        drop(handle);
        // wait_for_idle is the contract — it should observe the drop
        // and settle to Idle.
        tokio::time::timeout(Duration::from_millis(200), actor.wait_for_idle())
            .await
            .expect("wait_for_idle should resolve once handle is dropped");
        assert_eq!(actor.public_status(), ConversationStatus::Idle);
    }

    #[tokio::test]
    async fn begin_turn_returns_conflict_when_running() {
        let actor = fresh_actor();
        actor.mark_idle().await;
        let _h1 = actor.begin_turn("msg-1".into()).await.unwrap();
        let res = actor.begin_turn("msg-2".into()).await;
        assert!(matches!(res, Err(AppError::Conflict(_))));
    }

    #[tokio::test]
    async fn begin_turn_succeeds_from_never_opened() {
        let actor = fresh_actor();
        // No mark_idle — go straight from NeverOpened to Running.
        let _h = actor.begin_turn("msg-x".into()).await.unwrap();
        assert!(matches!(actor.public_status(), ConversationStatus::Running { .. }));
    }

    #[tokio::test]
    async fn cancel_waits_for_turn_handle_to_drop() {
        let actor = fresh_actor();
        actor.mark_idle().await;
        let handle = actor.begin_turn("msg-1".into()).await.unwrap();

        let release = Arc::new(tokio::sync::Notify::new());
        let release_for_task = release.clone();
        let drop_task = tokio::spawn(async move {
            release_for_task.notified().await;
            drop(handle);
        });

        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let actor_for_cancel = actor.clone();
        let cancel_task = tokio::spawn(async move {
            actor_for_cancel.wait_for_idle().await;
            flag.store(true, Ordering::SeqCst);
        });

        // wait_for_idle must not return while the handle is still alive.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!cancelled.load(Ordering::SeqCst));

        release.notify_one();
        drop_task.await.unwrap();
        cancel_task.await.unwrap();
        assert_eq!(actor.public_status(), ConversationStatus::Idle);
    }

    #[tokio::test]
    async fn wait_for_idle_is_noop_when_already_idle() {
        let actor = fresh_actor();
        actor.mark_idle().await;
        // Should resolve essentially immediately.
        tokio::time::timeout(Duration::from_millis(50), actor.wait_for_idle())
            .await
            .expect("wait_for_idle should be a no-op when idle");
    }

    #[tokio::test]
    async fn wait_for_idle_is_noop_when_never_opened() {
        let actor = fresh_actor();
        tokio::time::timeout(Duration::from_millis(50), actor.wait_for_idle())
            .await
            .expect("wait_for_idle should be a no-op when never opened");
        assert_eq!(actor.public_status(), ConversationStatus::Idle);
    }

    #[tokio::test]
    async fn turn_started_event_emitted_on_begin_turn() {
        let actor = fresh_actor();
        let mut rx = actor.subscribe();
        actor.mark_idle().await;
        let _h = actor.begin_turn("msg-evt".into()).await.unwrap();
        let event = tokio::time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("should receive event")
            .expect("channel open");
        match event {
            ConversationEvent::TurnStarted { msg_id } => assert_eq!(msg_id, "msg-evt"),
            other => panic!("expected TurnStarted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn second_begin_turn_succeeds_after_first_handle_dropped() {
        let actor = fresh_actor();
        actor.mark_idle().await;
        let h1 = actor.begin_turn("msg-1".into()).await.unwrap();
        drop(h1);
        actor.wait_for_idle().await;
        let _h2 = actor.begin_turn("msg-2".into()).await.unwrap();
        assert!(matches!(actor.public_status(), ConversationStatus::Running { .. }));
    }
}
