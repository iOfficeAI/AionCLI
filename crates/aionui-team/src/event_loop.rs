use std::sync::Arc;
use std::time::Duration;

use aionui_api_types::SendMessageRequest;
use aionui_conversation::IConversationService;
use aionui_conversation::conv_service_trait::ConversationStatus as ConvServiceStatus;
use aionui_realtime::EventBroadcaster;
use dashmap::DashMap;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::mailbox::Mailbox;
use crate::scheduler::TeammateManager;
use crate::session::TeamSession;
use crate::types::TeammateStatus;

/// Registry of per-agent Notify handles. Used by any trigger source to poke
/// an agent's event loop without needing to know its internals.
pub struct EventLoopRegistry {
    notifiers: DashMap<String, Arc<Notify>>,
    handles: DashMap<String, JoinHandle<()>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
}

impl Default for EventLoopRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl EventLoopRegistry {
    pub fn new() -> Self {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        Self {
            notifiers: DashMap::new(),
            handles: DashMap::new(),
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Check if an event loop is registered for this slot.
    pub fn has(&self, slot_id: &str) -> bool {
        self.notifiers.contains_key(slot_id)
    }

    /// Poke the named agent's event loop so it drains its mailbox.
    pub fn notify(&self, slot_id: &str) {
        if let Some(n) = self.notifiers.get(slot_id) {
            n.notify_one();
        }
    }

    /// Register and spawn an event loop for one agent.
    pub fn spawn(&self, slot_id: &str, ctx: AgentLoopContext) {
        let notify = Arc::new(Notify::new());
        self.notifiers.insert(slot_id.to_owned(), notify.clone());
        let handle = tokio::spawn(run_event_loop(notify, self.shutdown_rx.clone(), ctx));
        self.handles.insert(slot_id.to_owned(), handle);
    }

    /// Remove an agent's event loop (agent removed from team).
    pub fn remove(&self, slot_id: &str) {
        self.notifiers.remove(slot_id);
        if let Some((_, handle)) = self.handles.remove(slot_id) {
            handle.abort();
        }
    }

    /// Shut down all event loops.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        for entry in self.handles.iter() {
            entry.value().abort();
        }
        self.handles.clear();
        self.notifiers.clear();
    }
}

/// Context shared across all iterations of one agent's event loop.
///
/// `conversation_service` is the single conv-layer entry point used
/// by the loop for warmup, send and runtime status reads. Biz-layer
/// code reaches the connect layer only through this trait.
pub struct AgentLoopContext {
    pub team_id: String,
    pub slot_id: String,
    pub user_id: String,
    pub session: Arc<TeamSession>,
    pub scheduler: Arc<TeammateManager>,
    pub mailbox: Arc<Mailbox>,
    pub conversation_service: Arc<dyn IConversationService>,
    pub broadcaster: Arc<dyn EventBroadcaster>,
    /// Used to notify other agents' event loops (e.g. leader after all-settled).
    pub registry: Arc<EventLoopRegistry>,
}

/// The event loop for one agent slot. Spawned as a tokio task.
///
/// Flow:
/// 1. Wait for signal (notify) or shutdown.
/// 2. Drain loop: compute_wake_input → has messages → send_message (blocking) → finalize → repeat.
/// 3. When mailbox empty → back to step 1.
async fn run_event_loop(
    notify: Arc<Notify>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ctx: AgentLoopContext,
) {
    info!(
        team_id = %ctx.team_id,
        slot_id = %ctx.slot_id,
        "agent event loop started"
    );

    loop {
        // Step 1: wait for signal or shutdown
        tokio::select! {
            biased;
            _ = shutdown_rx.wait_for(|v| *v) => {
                info!(
                    team_id = %ctx.team_id,
                    slot_id = %ctx.slot_id,
                    "agent event loop shutting down"
                );
                return;
            }
            _ = notify.notified() => {}
        }

        // Drain loop: keep processing until mailbox is empty
        loop {
            if *shutdown_rx.borrow() {
                return;
            }

            let input = match ctx.session.compute_wake_input(&ctx.slot_id).await {
                Ok(Some(input)) => input,
                Ok(None) => break,
                Err(e) => {
                    warn!(
                        team_id = %ctx.team_id,
                        slot_id = %ctx.slot_id,
                        error = %e,
                        "event loop: compute_wake_input failed"
                    );
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    break;
                }
            };

            if !input.should_send {
                break;
            }

            match execute_turn(&ctx, &input).await {
                Some(finish_ok) => finalize_turn(&ctx, finish_ok, &input.conversation_id).await,
                None => break, // Turn not started (guard/warmup); retry on next signal
            }
        }
    }
}

/// Execute one agent turn through the conv-layer service.
///
/// Flow:
/// 1. Mirror unread mailbox rows into the agent conversation as left bubbles.
/// 2. `IConversationService::warmup` ensures the connector is alive.
/// 3. `IConversationService::status` skips the turn if a previous one is
///    still running (the conv-layer `ConvActor` mutex is the single
///    serializer; this read is a lock-free fast-path that avoids
///    contending the actor when a redundant wake fires).
/// 4. `IConversationService::send` dispatches the turn — relay,
///    persistence, broadcast and continuation handling all live in the
///    conv layer now, so the team event loop only owns scheduler and
///    mailbox bookkeeping.
///
/// Returns `Some(true)` on success, `Some(false)` on send failure,
/// `None` if the turn was skipped (warmup failed or already running).
async fn execute_turn(ctx: &AgentLoopContext, input: &crate::session::WakeInput) -> Option<bool> {
    ctx.session.mirror_unread_to_conversation(input).await;

    if let Err(e) = ctx
        .conversation_service
        .warmup(&ctx.user_id, &input.conversation_id)
        .await
    {
        warn!(
            team_id = %ctx.team_id,
            slot_id = %ctx.slot_id,
            conversation_id = %input.conversation_id,
            error = %e,
            "event loop: warmup failed"
        );
        return None;
    }

    if matches!(
        ctx.conversation_service.status(&input.conversation_id),
        ConvServiceStatus::Running { .. }
    ) {
        debug!(
            team_id = %ctx.team_id,
            slot_id = %ctx.slot_id,
            conversation_id = %input.conversation_id,
            "event loop: turn already running, skipping"
        );
        return None;
    }

    // Point-of-no-return: switch the slot to Working before invoking send
    // so the UI reflects intent immediately. DB.status writes are gone —
    // the conv-layer ConvActor is the single source of truth.
    let _ = ctx.scheduler.set_status(&ctx.slot_id, TeammateStatus::Working).await;

    // Collect files from unread messages (user-attached files)
    let files: Vec<String> = input
        .unread
        .iter()
        .filter_map(|m| m.files.as_ref())
        .flatten()
        .cloned()
        .collect();

    let req = SendMessageRequest {
        content: input.first_message.clone(),
        files,
        inject_skills: Vec::new(),
        hidden: false,
    };

    let turn_ok = match ctx
        .conversation_service
        .send(&ctx.user_id, &input.conversation_id, req)
        .await
    {
        Ok(_msg_id) => true,
        Err(e) => {
            warn!(
                team_id = %ctx.team_id,
                slot_id = %ctx.slot_id,
                conversation_id = %input.conversation_id,
                error = %e,
                "event loop: send failed"
            );
            false
        }
    };

    // Mark messages as read regardless of turn outcome
    let msg_ids: Vec<String> = input.unread.iter().map(|m| m.id.clone()).collect();
    if !msg_ids.is_empty()
        && let Err(e) = ctx.mailbox.mark_read_batch(&msg_ids).await
    {
        warn!(
            team_id = %ctx.team_id,
            slot_id = %ctx.slot_id,
            error = %e,
            "event loop: mark_read_batch failed (non-fatal)"
        );
    }

    Some(turn_ok)
}

/// Finalize a completed turn: reset DB status, mark idle (or error), cascade to leader.
async fn finalize_turn(ctx: &AgentLoopContext, finish_ok: bool, _conversation_id: &str) {
    // DB.status writes are gone — the conv-layer ConvActor and
    // StreamRelay finalize the conversation row. The team event
    // loop only owns slot-scheduler bookkeeping and the cross-agent
    // wake cascade.
    if !finish_ok {
        let _ = ctx.scheduler.set_status(&ctx.slot_id, TeammateStatus::Error).await;
    }
    match ctx.scheduler.finalize_turn(&ctx.slot_id, &[]).await {
        Ok(Some(wake_target)) => {
            if wake_target != ctx.slot_id {
                ctx.registry.notify(&wake_target);
            }
        }
        Ok(None) => {}
        Err(e) => {
            warn!(
                team_id = %ctx.team_id,
                slot_id = %ctx.slot_id,
                error = %e,
                "event loop: finalize_turn failed"
            );
        }
    }
}
