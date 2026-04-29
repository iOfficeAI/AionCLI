use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aionui_ai_agent::types::AgentStreamChunk;
use aionui_realtime::EventBroadcaster;
use dashmap::{DashMap, DashSet};
use tokio::sync::{Mutex, broadcast};
use tracing::{debug, warn};

use crate::crash_detection::CrashReason;
use crate::error::TeamError;
use crate::events::TeamEventEmitter;
use crate::mailbox::Mailbox;
use crate::task_board::TaskBoard;
use crate::types::{MailboxMessage, MailboxMessageType, TeamAgent, TeamTask, TeammateRole, TeammateStatus};

pub const WAKE_TIMEOUT_MS: u64 = 60_000;

const FINALIZE_DEDUP_WINDOW: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// normalize_name — canonical form for agent-name conflict checks
// ---------------------------------------------------------------------------

/// Normalize an agent name to its canonical form for conflict detection.
///
/// Rules (see interface-contracts §15.1):
/// 1. Trim leading/trailing whitespace.
/// 2. Drop control characters (`char::is_control`).
/// 3. Lowercase (Unicode-aware via `to_lowercase`).
pub fn normalize_name(name: &str) -> String {
    name.trim()
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .to_lowercase()
}

// ---------------------------------------------------------------------------
// SchedulerAction — actions parsed from an agent's turn response
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum SchedulerAction {
    SendMessage {
        to: String,
        message: String,
    },
    TaskCreate {
        subject: String,
        description: Option<String>,
        owner: Option<String>,
        blocked_by: Vec<String>,
    },
    TaskUpdate {
        task_id: String,
        status: Option<String>,
        description: Option<String>,
        owner: Option<String>,
        blocked_by: Option<Vec<String>>,
    },
    SpawnAgent {
        name: String,
        role: String,
        backend: String,
    },
    IdleNotification {
        summary: Option<String>,
    },
    ShutdownAgent {
        slot_id: String,
        reason: Option<String>,
    },
    RenameAgent {
        slot_id: String,
        new_name: String,
    },
}

// ---------------------------------------------------------------------------
// WakePayload — context assembled for an agent when it is woken up
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WakePayload {
    pub agent: TeamAgent,
    pub tasks: Vec<TeamTask>,
    pub unread_messages: Vec<MailboxMessage>,
}

// ---------------------------------------------------------------------------
// AgentSlot — per-agent runtime state tracked by the scheduler
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct AgentSlot {
    agent: TeamAgent,
    status: TeammateStatus,
}

// ---------------------------------------------------------------------------
// TeammateManager
// ---------------------------------------------------------------------------

pub struct TeammateManager {
    team_id: String,
    slots: Mutex<HashMap<String, AgentSlot>>,
    mailbox: Arc<Mailbox>,
    task_board: Arc<TaskBoard>,
    events: TeamEventEmitter,
    active_wakes: DashSet<String>,
    // Reason: Finish / Error events may fire back-to-back for the same
    // conversation; without this dedup window, finalize_turn would run twice
    // and double-write the IdleNotification (aionui-audit §4.3, §8 #3).
    finalized_turns: Arc<DashMap<String, Instant>>,
    wake_timeouts: Arc<DashMap<String, tokio::task::JoinHandle<()>>>,
}

/// Callback invoked when the wake-timeout watchdog elapses without seeing
/// any stream activity for a slot.
///
/// Reason: `arm_wake_timeout` is written against `origin/main`, where
/// `handle_inactivity_timeout` (W4-D22, PR #99) does not yet exist. Taking
/// the recovery action as an injected closure keeps this module decoupled —
/// once D22 lands, callers just pass `mgr.handle_inactivity_timeout(...)`
/// through this slot without touching `arm_wake_timeout` itself.
pub type WakeTimeoutHandler = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Status set that counts as "settled" for the purpose of
/// "all teammates settled → wake leader" transitions.
///
/// Expanded beyond `Idle` to match the AionUi reference implementation
/// (TeammateManager.ts:440-452): `Completed` and `Error` teammates are
/// terminal and should not block the leader from being woken up.
/// `Pending` is not in the set because the backend currently serde-aliases
/// `"pending"` to `Idle`; it will be reintroduced when the variant is split.
fn is_settled(status: TeammateStatus) -> bool {
    matches!(
        status,
        TeammateStatus::Idle | TeammateStatus::Completed | TeammateStatus::Error
    )
}

// ---------------------------------------------------------------------------
// Crash testament formatting
// ---------------------------------------------------------------------------

/// Format a crash testament message for delivery to the leader.
///
/// The resulting text summarises which agent crashed, the reason, and
/// optionally the last message seen before the crash.
pub fn format_crash_testament(agent_name: &str, reason: &CrashReason, last_message: Option<&str>) -> String {
    let reason_str = match reason {
        CrashReason::ProcessExited => "ProcessExited",
        CrashReason::SessionNotFound => "SessionNotFound",
        CrashReason::Unknown(msg) => return format_with_unknown(agent_name, msg, last_message),
    };
    if let Some(msg) = last_message {
        format!(
            "Teammate '{}' crashed during task (reason: {}). Last message: {}. Please investigate.",
            agent_name, reason_str, msg
        )
    } else {
        format!(
            "Teammate '{}' crashed during task (reason: {}). Please investigate.",
            agent_name, reason_str
        )
    }
}

fn format_with_unknown(agent_name: &str, reason_msg: &str, last_message: Option<&str>) -> String {
    if let Some(msg) = last_message {
        format!(
            "Teammate '{}' crashed during task (reason: Unknown — {}). Last message: {}. Please investigate.",
            agent_name, reason_msg, msg
        )
    } else {
        format!(
            "Teammate '{}' crashed during task (reason: Unknown — {}). Please investigate.",
            agent_name, reason_msg
        )
    }
}

impl TeammateManager {
    pub fn new(
        team_id: String,
        agents: &[TeamAgent],
        mailbox: Arc<Mailbox>,
        task_board: Arc<TaskBoard>,
        broadcaster: Arc<dyn EventBroadcaster>,
    ) -> Self {
        let mut slots = HashMap::new();
        for agent in agents {
            slots.insert(
                agent.slot_id.clone(),
                AgentSlot {
                    agent: agent.clone(),
                    status: TeammateStatus::Idle,
                },
            );
        }
        let events = TeamEventEmitter::new(team_id.clone(), broadcaster);
        Self {
            team_id,
            slots: Mutex::new(slots),
            mailbox,
            task_board,
            events,
            active_wakes: DashSet::new(),
            finalized_turns: Arc::new(DashMap::new()),
            wake_timeouts: Arc::new(DashMap::new()),
        }
    }

    pub async fn set_status(&self, slot_id: &str, status: TeammateStatus) -> Result<(), TeamError> {
        {
            let mut slots = self.slots.lock().await;
            let slot = slots
                .get_mut(slot_id)
                .ok_or_else(|| TeamError::AgentNotFound(slot_id.to_owned()))?;
            slot.status = status;
            slot.agent.status = Some(status);
        }
        self.events.broadcast_agent_status(slot_id, status);
        debug!(team_id = %self.team_id, slot_id, %status, "agent status changed");
        Ok(())
    }

    pub async fn get_status(&self, slot_id: &str) -> Result<TeammateStatus, TeamError> {
        let slots = self.slots.lock().await;
        let slot = slots
            .get(slot_id)
            .ok_or_else(|| TeamError::AgentNotFound(slot_id.to_owned()))?;
        Ok(slot.status)
    }

    pub async fn get_agent(&self, slot_id: &str) -> Result<TeamAgent, TeamError> {
        let slots = self.slots.lock().await;
        let slot = slots
            .get(slot_id)
            .ok_or_else(|| TeamError::AgentNotFound(slot_id.to_owned()))?;
        Ok(slot.agent.clone())
    }

    pub async fn build_wake_payload(&self, slot_id: &str) -> Result<WakePayload, TeamError> {
        let agent = self.get_agent(slot_id).await?;
        let tasks = self.task_board.list_tasks(&self.team_id).await?;
        let unread = self.mailbox.read_unread(&self.team_id, slot_id).await?;
        Ok(WakePayload {
            agent,
            tasks,
            unread_messages: unread,
        })
    }

    /// Attempt to wake an idle agent. Returns the payload to send.
    /// Transitions agent from Idle → Working.
    /// Returns `None` if the agent is not idle (skip duplicate wake).
    pub async fn try_wake(&self, slot_id: &str) -> Result<Option<WakePayload>, TeamError> {
        let current = self.get_status(slot_id).await?;
        if current != TeammateStatus::Idle {
            debug!(
                team_id = %self.team_id,
                slot_id,
                current_status = %current,
                "skip wake: agent not idle"
            );
            return Ok(None);
        }
        self.set_status(slot_id, TeammateStatus::Working).await?;
        let payload = self.build_wake_payload(slot_id).await?;
        Ok(Some(payload))
    }

    /// Mark agent as idle after turn completion or timeout.
    ///
    /// Side effects:
    /// 1. Transition slot status to `Idle` (broadcasts `team.agent.status`).
    /// 2. If `slot_id` is a teammate (not the lead) and a lead exists,
    ///    write an `IdleNotification` to the lead's mailbox. `summary` is
    ///    persisted both as the message content (falling back to `"idle"`)
    ///    and as the structured summary column.
    /// 3. Check whether all teammates are settled → returns the lead slot_id
    ///    that the caller should wake, if any.
    pub async fn mark_idle(&self, slot_id: &str, summary: Option<&str>) -> Result<Option<String>, TeamError> {
        self.set_status(slot_id, TeammateStatus::Idle).await?;

        let is_lead = {
            let slots = self.slots.lock().await;
            let slot = slots
                .get(slot_id)
                .ok_or_else(|| TeamError::AgentNotFound(slot_id.to_owned()))?;
            slot.agent.role == TeammateRole::Lead
        };

        if is_lead {
            return Ok(None);
        }

        if let Some(lead_slot_id) = self.find_lead_slot_id().await
            && lead_slot_id != slot_id
        {
            self.mailbox
                .write(
                    &self.team_id,
                    &lead_slot_id,
                    slot_id,
                    MailboxMessageType::IdleNotification,
                    summary.unwrap_or("idle"),
                    summary,
                )
                .await?;
        }

        self.maybe_wake_leader_when_all_idle().await
    }

    /// Execute a single action from the agent's turn response.
    pub async fn execute_action(
        &self,
        from_slot_id: &str,
        action: &SchedulerAction,
    ) -> Result<Option<String>, TeamError> {
        match action {
            SchedulerAction::SendMessage { to, message } => {
                self.handle_send_message(from_slot_id, to, message).await?;
                Ok(None)
            }
            SchedulerAction::TaskCreate {
                subject,
                description,
                owner,
                blocked_by,
            } => {
                self.task_board
                    .create_task(
                        &self.team_id,
                        subject,
                        description.as_deref(),
                        owner.as_deref(),
                        blocked_by,
                    )
                    .await?;
                Ok(None)
            }
            SchedulerAction::TaskUpdate {
                task_id,
                status,
                description,
                owner,
                blocked_by,
            } => {
                use crate::task_board::TaskUpdate;
                use crate::types::TaskStatus;

                let update = TaskUpdate {
                    status: status.as_deref().and_then(TaskStatus::parse),
                    description: description.clone(),
                    owner: owner.clone(),
                    blocked_by: blocked_by.clone(),
                    ..Default::default()
                };
                self.task_board.update_task(&self.team_id, task_id, &update).await?;
                Ok(None)
            }
            SchedulerAction::IdleNotification { summary } => {
                self.handle_idle_notification(from_slot_id, summary.as_deref()).await
            }
            SchedulerAction::SpawnAgent { name, role, backend } => {
                debug!(
                    team_id = %self.team_id,
                    from = from_slot_id,
                    name, role, backend,
                    "spawn_agent action — requires TeamSession to complete"
                );
                Ok(None)
            }
            SchedulerAction::ShutdownAgent { slot_id, reason } => {
                self.handle_shutdown_agent(from_slot_id, slot_id, reason.as_deref())
                    .await?;
                Ok(None)
            }
            SchedulerAction::RenameAgent { slot_id, new_name } => {
                self.handle_rename_agent(slot_id, new_name).await?;
                Ok(None)
            }
        }
    }

    /// Finalize a turn: execute a batch of actions, then mark agent idle.
    ///
    /// If the batch contains an `IdleNotification`, its `summary` is threaded
    /// through the mark-idle path so the lead mailbox notification carries
    /// the agent-provided summary. The IdleNotification action itself is
    /// skipped during execution to avoid double-writing the notification —
    /// the single trailing `mark_idle` covers both status transition and
    /// mailbox write.
    ///
    /// Returns an optional leader slot_id to wake if all teammates are
    /// settled.
    pub async fn finalize_turn(&self, slot_id: &str, actions: &[SchedulerAction]) -> Result<Option<String>, TeamError> {
        let mut summary: Option<String> = None;
        for action in actions {
            if let SchedulerAction::IdleNotification { summary: s } = action {
                if summary.is_none() {
                    summary.clone_from(s);
                }
                continue;
            }
            self.execute_action(slot_id, action).await?;
        }

        self.mark_idle(slot_id, summary.as_deref()).await
    }

    /// Add a new agent slot at runtime (for spawn_agent).
    pub async fn add_agent(&self, agent: &TeamAgent) {
        let mut slots = self.slots.lock().await;
        slots.insert(
            agent.slot_id.clone(),
            AgentSlot {
                agent: agent.clone(),
                status: TeammateStatus::Idle,
            },
        );
        self.events.broadcast_agent_spawned(agent);
        debug!(
            team_id = %self.team_id,
            slot_id = %agent.slot_id,
            name = %agent.name,
            "agent added to scheduler"
        );
    }

    /// Remove an agent slot at runtime.
    ///
    /// Also clears any scheduler-side state tied to the slot so a later
    /// re-spawn (or a stale callback from the killed agent task) cannot be
    /// blocked by leftover wake locks, wake timeouts, or finalize-dedup
    /// entries. See [`Self::clear_agent_state`] for the state inventory.
    ///
    /// Returns the removed slot's `conversation_id` so the session layer can
    /// terminate the underlying worker process via
    /// `IWorkerTaskManager::kill` (W5-D30d-1). The scheduler does not own the
    /// task manager, mirroring the delegation pattern used by
    /// [`Self::handle_agent_crash`]: cross-crate side effects are handed back
    /// to the session.
    pub async fn remove_agent(&self, slot_id: &str) -> Result<Option<String>, TeamError> {
        let mut slots = self.slots.lock().await;
        let removed = slots
            .remove(slot_id)
            .ok_or_else(|| TeamError::AgentNotFound(slot_id.to_owned()))?;
        let conversation_id = removed.agent.conversation_id.clone();
        drop(slots);
        self.clear_agent_state(slot_id, &conversation_id);
        self.events.broadcast_agent_removed(slot_id);
        debug!(team_id = %self.team_id, slot_id, "agent removed from scheduler");
        Ok(Some(conversation_id))
    }

    /// Clear all scheduler-side state associated with a removed agent.
    ///
    /// Drops three independent entries so nothing survives to affect the
    /// slot's next life (re-spawn with the same id, or a sibling slot that
    /// shares the same conversation):
    /// * `active_wakes` — W4-D18a wake-in-flight lock
    /// * `wake_timeouts` — W4-D18b-1 per-slot wake watchdog task
    /// * `finalized_turns` — W4-D19a Finish/Error dedup window (keyed by
    ///   `conversation_id`, not `slot_id`)
    ///
    /// Idempotent: every underlying call tolerates missing entries.
    pub fn clear_agent_state(&self, slot_id: &str, conversation_id: &str) {
        self.active_wakes.remove(slot_id);
        self.clear_wake_timeout(slot_id);
        self.finalized_turns.remove(conversation_id);
    }

    /// Rename an agent slot.
    pub async fn rename_agent(&self, slot_id: &str, new_name: &str) -> Result<(), TeamError> {
        let mut slots = self.slots.lock().await;
        let slot = slots
            .get_mut(slot_id)
            .ok_or_else(|| TeamError::AgentNotFound(slot_id.to_owned()))?;
        slot.agent.name = new_name.to_owned();
        drop(slots);
        self.events.broadcast_agent_renamed(slot_id, new_name);
        debug!(team_id = %self.team_id, slot_id, new_name, "agent renamed");
        Ok(())
    }

    pub async fn list_agents(&self) -> Vec<TeamAgent> {
        let slots = self.slots.lock().await;
        slots.values().map(|s| s.agent.clone()).collect()
    }

    pub async fn list_tasks(&self) -> Result<Vec<crate::types::TeamTask>, TeamError> {
        self.task_board.list_tasks(&self.team_id).await
    }

    pub async fn find_lead_slot_id(&self) -> Option<String> {
        let slots = self.slots.lock().await;
        slots
            .values()
            .find(|s| s.agent.role == TeammateRole::Lead)
            .map(|s| s.agent.slot_id.clone())
    }

    /// Try to reserve an exclusive wake-in-flight slot for `slot_id`.
    ///
    /// Returns `true` when the caller is responsible for driving the wake
    /// and later calling [`Self::release_wake_lock`]. Returns `false` when
    /// another wake is already running for this slot and the caller should
    /// skip the duplicate wake.
    ///
    /// Backed by a `DashSet` so it is safe to call concurrently from any
    /// task; contention resolves atomically via `DashSet::insert`.
    pub fn acquire_wake_lock(&self, slot_id: &str) -> bool {
        self.active_wakes.insert(slot_id.to_owned())
    }

    /// Release a wake lock previously acquired via [`Self::acquire_wake_lock`].
    ///
    /// Idempotent: no-op if the lock was never held.
    pub fn release_wake_lock(&self, slot_id: &str) {
        self.active_wakes.remove(slot_id);
    }

    /// Cancel and remove the wake timeout task for a slot.
    pub fn clear_wake_timeout(&self, slot_id: &str) {
        if let Some((_, handle)) = self.wake_timeouts.remove(slot_id) {
            handle.abort();
        }
    }

    /// Arm a wake-timeout watchdog for `slot_id`.
    ///
    /// Spawns a background task that subscribes to `stream_rx` and enforces
    /// the [`WAKE_TIMEOUT_MS`] inactivity budget:
    ///
    /// - Any chunk (`Text` / `Thought` / `ToolUse` / `Error`) resets the
    ///   deadline — the agent is still alive and producing output.
    /// - A `Finish` chunk, or a closed / lagging channel, exits the watchdog
    ///   cleanly (no timeout fires).
    /// - If no chunk arrives within the deadline, `on_timeout(slot_id)` is
    ///   invoked exactly once and the watchdog exits.
    ///
    /// In every exit path the map entry is removed so the `JoinHandle` is
    /// dropped promptly.
    ///
    /// If a previous watchdog is still running for this slot, it is aborted
    /// first — a fresh wake supersedes any stale watchdog (aionui-audit §6.2).
    pub fn arm_wake_timeout(
        &self,
        slot_id: &str,
        stream_rx: broadcast::Receiver<AgentStreamChunk>,
        on_timeout: WakeTimeoutHandler,
    ) {
        let slot_id_owned = slot_id.to_owned();
        let map = self.wake_timeouts.clone();
        let map_for_task = map.clone();
        let handle = tokio::spawn(async move {
            let mut rx = stream_rx;
            let timeout = Duration::from_millis(WAKE_TIMEOUT_MS);
            let sleep = tokio::time::sleep(timeout);
            tokio::pin!(sleep);

            let timed_out = loop {
                tokio::select! {
                    chunk = rx.recv() => {
                        match chunk {
                            Ok(AgentStreamChunk::Finish { .. }) => break false,
                            Err(broadcast::error::RecvError::Closed) => break false,
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                warn!(slot_id = %slot_id_owned, skipped = n, "wake watchdog lagged");
                                sleep.as_mut().reset(tokio::time::Instant::now() + timeout);
                            }
                            Ok(_) => {
                                // Activity detected — reset deadline.
                                sleep.as_mut().reset(tokio::time::Instant::now() + timeout);
                            }
                        }
                    }
                    _ = &mut sleep => break true,
                }
            };

            if timed_out {
                on_timeout(slot_id_owned.clone()).await;
            }

            map_for_task.remove(&slot_id_owned);
        });

        if let Some(old) = map.insert(slot_id.to_owned(), handle) {
            old.abort();
        }
    }

    /// Attempt to claim the right to finalize the turn for `conversation_id`.
    ///
    /// Returns `true` when the caller should proceed with finalize. Returns
    /// `false` when another finalize ran for the same conversation within
    /// the last [`FINALIZE_DEDUP_WINDOW`] and the caller should skip.
    ///
    /// On success, records the current instant and spawns a task to remove
    /// the entry after the window elapses — keeps the map bounded without
    /// requiring callers to clean up.
    pub fn begin_finalize(&self, conversation_id: &str) -> bool {
        let now = Instant::now();
        let should_proceed = !matches!(
            self.finalized_turns.get(conversation_id),
            Some(entry) if now.duration_since(*entry.value()) < FINALIZE_DEDUP_WINDOW
        );
        if should_proceed {
            self.finalized_turns.insert(conversation_id.to_owned(), now);
            let map = self.finalized_turns.clone();
            let key = conversation_id.to_owned();
            tokio::spawn(async move {
                tokio::time::sleep(FINALIZE_DEDUP_WINDOW).await;
                map.remove(&key);
            });
        }
        should_proceed
    }

    /// Drop the finalize-dedup entry for `conversation_id` immediately.
    ///
    /// Reason: re-wake paths (W4-D18) start a fresh turn whose own Finish
    /// would otherwise be swallowed by a lingering dedup entry from the
    /// previous turn (aionui-audit §8 #3).
    pub fn clear_finalized_turn(&self, conversation_id: &str) {
        self.finalized_turns.remove(conversation_id);
    }

    /// Handle a teammate crash: notify lead, update status, drop locks/timeouts.
    ///
    /// Runs the five-step recovery sequence for a non-lead agent that the
    /// scheduler has detected as crashed (W4-D20a `detect_crash` classifier):
    ///
    /// 1. Write a crash testament to the lead's mailbox
    ///    (via [`Self::write_crash_testament`]). The testament carries the
    ///    reason and optional last message so the lead can decide how to
    ///    recover.
    /// 2. Transition the crashed slot to [`TeammateStatus::Error`] — the
    ///    enum's terminal failure state. `"failed"` is serde-aliased to
    ///    `Error`, matching the frontend contract.
    /// 3. Release the wake lock in case one was held — otherwise a future
    ///    wake for the same slot would be blocked forever.
    /// 4. Cancel any pending wake timeout — the crashed slot will never
    ///    answer, so keeping the timeout alive wastes a handle and risks a
    ///    late spurious callback.
    /// 5. Return the lead slot_id so the caller (session layer) can wake
    ///    the lead. Mirrors [`Self::mark_idle`]'s contract: the scheduler
    ///    does not invoke agent managers directly; it hands the wake target
    ///    back to the session, which owns the `compute_wake_input` +
    ///    `send_message` plumbing.
    ///
    /// Leader crash (W4-D20c): there is no higher-ranked agent to notify or
    /// wake, so steps 1 and 5 degrade to no-ops — no self-addressed testament,
    /// no self-wake. Steps 2-4 still run so status, wake lock, and wake
    /// timeout do not leak. The leader slot stays in the roster so downstream
    /// session code can still emit an error event for it.
    pub async fn handle_agent_crash(
        &self,
        slot_id: &str,
        reason: CrashReason,
        last_message: Option<&str>,
    ) -> Result<Option<String>, TeamError> {
        let (agent_name, is_lead) = {
            let slots = self.slots.lock().await;
            let slot = slots
                .get(slot_id)
                .ok_or_else(|| TeamError::AgentNotFound(slot_id.to_owned()))?;
            (slot.agent.name.clone(), slot.agent.role == TeammateRole::Lead)
        };

        // Step 1: testament to lead (no-op when crasher is the lead or no lead exists).
        self.write_crash_testament(slot_id, &agent_name, &reason, last_message)
            .await?;

        // Step 2: mark the slot as terminally failed.
        self.set_status(slot_id, TeammateStatus::Error).await?;

        // Step 3/4: release wake lock and cancel pending wake timeout.
        self.release_wake_lock(slot_id);
        self.clear_wake_timeout(slot_id);

        // Step 5: hand the lead slot back to the caller to wake, but only
        // when the crasher is a teammate. On leader crash there is no
        // higher-ranked agent to escalate to — return None (W4-D20c).
        if is_lead {
            return Ok(None);
        }
        Ok(self.find_lead_slot_id().await)
    }

    /// Handle a teammate that went silent for longer than the wake-timeout
    /// window (see [`WAKE_TIMEOUT_MS`]).
    ///
    /// This is invoked by the inactivity watchdog spawned in W4-D18b-2: when
    /// no stream chunk arrives for the slot within the timeout window, the
    /// watchdog calls this handler to recover local state. It mirrors the
    /// bookkeeping performed by crash recovery (status + locks + timeouts)
    /// but does not emit a crash testament — the diagnosis is different
    /// (inactivity, not process death) and the message routes through the
    /// normal mailbox channel so the lead sees it like any other teammate
    /// update.
    ///
    /// Flow:
    /// 1. Transition the slot to [`TeammateStatus::Error`] — mirrors how
    ///    crash recovery marks terminal failure. A stuck agent is useless
    ///    to the team until the lead intervenes.
    /// 2. Release the wake lock — the watchdog itself acquired it when
    ///    starting the turn, and nothing else will release it now.
    /// 3. Clear the wake timeout entry — this handler is the timer's own
    ///    body, so the stored `JoinHandle` is already about to complete,
    ///    but removing it keeps the map bounded.
    /// 4. If the stuck slot is a teammate, write a diagnostic message to
    ///    the lead mailbox and return the lead slot id so the caller can
    ///    wake the lead. If the stuck slot is the lead itself, there is
    ///    nobody to notify or wake — return `None` and stop.
    pub async fn handle_inactivity_timeout(&self, slot_id: &str) -> Result<Option<String>, TeamError> {
        let (agent_name, is_lead) = {
            let slots = self.slots.lock().await;
            let slot = slots
                .get(slot_id)
                .ok_or_else(|| TeamError::AgentNotFound(slot_id.to_owned()))?;
            (slot.agent.name.clone(), slot.agent.role == TeammateRole::Lead)
        };

        self.set_status(slot_id, TeammateStatus::Error).await?;
        self.release_wake_lock(slot_id);
        self.clear_wake_timeout(slot_id);

        if is_lead {
            return Ok(None);
        }

        let Some(lead_slot_id) = self.find_lead_slot_id().await else {
            return Ok(None);
        };
        let message = format!(
            "Teammate '{}' timed out after 60s of inactivity. Please investigate.",
            agent_name
        );
        self.mailbox
            .write(
                &self.team_id,
                &lead_slot_id,
                slot_id,
                MailboxMessageType::Message,
                &message,
                None,
            )
            .await?;
        Ok(Some(lead_slot_id))
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Write a crash testament message to the leader's mailbox.
    ///
    /// When a teammate crashes, this delivers a diagnostic message to the
    /// lead so it can decide how to recover (reassign, respawn, etc.).
    /// No-op when no lead slot exists in the team.
    pub async fn write_crash_testament(
        &self,
        slot_id: &str,
        agent_name: &str,
        reason: &CrashReason,
        last_message: Option<&str>,
    ) -> Result<(), TeamError> {
        let Some(lead_slot_id) = self.find_lead_slot_id().await else {
            return Ok(());
        };
        if lead_slot_id == slot_id {
            // Leader crashed into itself — nothing to notify.
            return Ok(());
        }
        let testament = format_crash_testament(agent_name, reason, last_message);
        self.mailbox
            .write(
                &self.team_id,
                &lead_slot_id,
                slot_id,
                MailboxMessageType::Message,
                &testament,
                None,
            )
            .await?;
        Ok(())
    }

    /// Notify the leader that a teammate declined a shutdown request.
    ///
    /// Called when `team_send_message` intercepts a `shutdown_rejected: <reason>`
    /// payload. The teammate continues to run; the lead receives a mailbox
    /// message describing who refused and why. No-op when no lead exists or
    /// when the sender is the lead itself.
    pub async fn notify_shutdown_rejected(&self, from_slot_id: &str, reason: &str) -> Result<(), TeamError> {
        let Some(lead_slot_id) = self.find_lead_slot_id().await else {
            return Ok(());
        };
        if lead_slot_id == from_slot_id {
            return Ok(());
        }
        let agent_name = self
            .get_agent(from_slot_id)
            .await
            .map(|a| a.name)
            .unwrap_or_else(|_| from_slot_id.to_owned());
        let content = format!("Teammate '{agent_name}' declined shutdown: {reason}");
        self.mailbox
            .write(
                &self.team_id,
                &lead_slot_id,
                from_slot_id,
                MailboxMessageType::Message,
                &content,
                None,
            )
            .await?;
        Ok(())
    }

    async fn handle_send_message(&self, from_slot_id: &str, to: &str, message: &str) -> Result<(), TeamError> {
        if to == "*" {
            let slots = self.slots.lock().await;
            let targets: Vec<String> = slots.keys().filter(|id| id.as_str() != from_slot_id).cloned().collect();
            drop(slots);

            for target in &targets {
                self.mailbox
                    .write(
                        &self.team_id,
                        target,
                        from_slot_id,
                        MailboxMessageType::Message,
                        message,
                        None,
                    )
                    .await?;
            }
        } else {
            self.mailbox
                .write(
                    &self.team_id,
                    to,
                    from_slot_id,
                    MailboxMessageType::Message,
                    message,
                    None,
                )
                .await?;
        }
        Ok(())
    }

    async fn handle_idle_notification(
        &self,
        from_slot_id: &str,
        summary: Option<&str>,
    ) -> Result<Option<String>, TeamError> {
        // `mark_idle` writes the IdleNotification to the lead mailbox on our
        // behalf when `from_slot_id` is a teammate, so we do not need to
        // call `mailbox.write` here.
        self.mark_idle(from_slot_id, summary).await
    }

    async fn handle_shutdown_agent(
        &self,
        from_slot_id: &str,
        target_slot_id: &str,
        reason: Option<&str>,
    ) -> Result<(), TeamError> {
        let from_role = {
            let slots = self.slots.lock().await;
            let slot = slots
                .get(from_slot_id)
                .ok_or_else(|| TeamError::AgentNotFound(from_slot_id.to_owned()))?;
            slot.agent.role
        };

        if from_role != TeammateRole::Lead {
            return Err(TeamError::InvalidRequest("only lead can shutdown agents".into()));
        }

        {
            let slots = self.slots.lock().await;
            let target = slots
                .get(target_slot_id)
                .ok_or_else(|| TeamError::AgentNotFound(target_slot_id.to_owned()))?;
            if target.agent.role == TeammateRole::Lead {
                return Err(TeamError::InvalidRequest("cannot shutdown the team lead".into()));
            }
        }

        self.mailbox
            .write(
                &self.team_id,
                target_slot_id,
                from_slot_id,
                MailboxMessageType::ShutdownRequest,
                reason.unwrap_or("shutdown requested"),
                None,
            )
            .await?;

        Ok(())
    }

    async fn handle_rename_agent(&self, slot_id: &str, new_name: &str) -> Result<(), TeamError> {
        self.rename_agent(slot_id, new_name).await
    }

    async fn maybe_wake_leader_when_all_idle(&self) -> Result<Option<String>, TeamError> {
        let slots = self.slots.lock().await;

        let mut lead_slot_id = None;
        let mut all_teammates_settled = true;
        let mut has_teammates = false;

        for slot in slots.values() {
            if slot.agent.role == TeammateRole::Lead {
                lead_slot_id = Some(slot.agent.slot_id.clone());
                continue;
            }
            has_teammates = true;
            if !is_settled(slot.status) {
                all_teammates_settled = false;
                break;
            }
        }

        let Some(lead_id) = lead_slot_id else {
            return Ok(None);
        };

        if !has_teammates {
            return Ok(None);
        }

        if !all_teammates_settled {
            return Ok(None);
        }

        // The leader itself still needs to be Idle so we don't interrupt
        // an in-flight leader turn with another wake.
        let lead_is_idle = slots
            .get(&lead_id)
            .map(|s| s.status == TeammateStatus::Idle)
            .unwrap_or(false);

        if !lead_is_idle {
            return Ok(None);
        }

        drop(slots);

        debug!(
            team_id = %self.team_id,
            lead_slot_id = %lead_id,
            "all teammates settled — signaling to wake leader"
        );

        Ok(Some(lead_id))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockTeamRepo;
    use aionui_api_types::WebSocketMessage;

    // -----------------------------------------------------------------
    // normalize_name — §15.1 contract
    // -----------------------------------------------------------------

    #[test]
    fn normalize_name_trims_outer_whitespace() {
        assert_eq!(normalize_name("  Alice  "), "alice");
        assert_eq!(normalize_name("\tBob\n"), "bob");
    }

    #[test]
    fn normalize_name_lowercases_ascii_and_unicode() {
        assert_eq!(normalize_name("ALICE"), "alice");
        assert_eq!(normalize_name("Crème"), "crème");
    }

    #[test]
    fn normalize_name_filters_control_characters() {
        // Null + bell in the middle + outer whitespace.
        assert_eq!(normalize_name("  Ali\x00ce\x07 "), "alice");
    }

    #[test]
    fn normalize_name_collides_on_case_and_whitespace() {
        // Conflict-detection invariant: two inputs that only differ by
        // surrounding whitespace / case must normalize to the same string.
        assert_eq!(normalize_name("  Leader  "), normalize_name("leader"));
    }

    struct RecordingBroadcaster {
        events: std::sync::Mutex<Vec<WebSocketMessage<serde_json::Value>>>,
    }

    impl RecordingBroadcaster {
        fn new() -> Self {
            Self {
                events: std::sync::Mutex::new(vec![]),
            }
        }

        fn events(&self) -> Vec<WebSocketMessage<serde_json::Value>> {
            self.events.lock().unwrap().clone()
        }
    }

    impl EventBroadcaster for RecordingBroadcaster {
        fn broadcast(&self, event: WebSocketMessage<serde_json::Value>) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn make_agent(slot_id: &str, name: &str, role: TeammateRole) -> TeamAgent {
        TeamAgent {
            slot_id: slot_id.into(),
            name: name.into(),
            role,
            conversation_id: format!("conv-{slot_id}"),
            backend: "acp".into(),
            model: "claude".into(),
            custom_agent_id: None,
            status: None,
            conversation_type: None,
            cli_path: None,
        }
    }

    fn make_team_agents() -> Vec<TeamAgent> {
        vec![
            make_agent("lead-1", "Lead", TeammateRole::Lead),
            make_agent("worker-1", "Worker1", TeammateRole::Teammate),
            make_agent("worker-2", "Worker2", TeammateRole::Teammate),
        ]
    }

    fn make_manager(agents: &[TeamAgent]) -> (TeammateManager, Arc<RecordingBroadcaster>) {
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), agents, mailbox, task_board, broadcaster.clone());
        (mgr, broadcaster)
    }

    // -- Status management ---------------------------------------------------

    #[tokio::test]
    async fn initial_status_is_idle() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        for agent in &agents {
            let status = mgr.get_status(&agent.slot_id).await.unwrap();
            assert_eq!(status, TeammateStatus::Idle);
        }
    }

    #[tokio::test]
    async fn set_status_updates_and_broadcasts() {
        let agents = make_team_agents();
        let (mgr, bc) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();

        assert_eq!(mgr.get_status("worker-1").await.unwrap(), TeammateStatus::Working);

        let events = bc.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "team.agent.status");
        assert_eq!(events[0].data["slot_id"], "worker-1");
        assert_eq!(events[0].data["status"], "working");
    }

    #[tokio::test]
    async fn set_status_nonexistent_agent_fails() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        let result = mgr.set_status("ghost", TeammateStatus::Working).await;
        assert!(matches!(result, Err(TeamError::AgentNotFound(_))));
    }

    // -- Wake / try_wake -----------------------------------------------------

    #[tokio::test]
    async fn try_wake_idle_agent_returns_payload() {
        let agents = make_team_agents();
        let (mgr, bc) = make_manager(&agents);

        let payload = mgr.try_wake("worker-1").await.unwrap();
        assert!(payload.is_some());

        let p = payload.unwrap();
        assert_eq!(p.agent.slot_id, "worker-1");
        assert!(p.tasks.is_empty());
        assert!(p.unread_messages.is_empty());

        assert_eq!(mgr.get_status("worker-1").await.unwrap(), TeammateStatus::Working);

        let status_events: Vec<_> = bc
            .events()
            .into_iter()
            .filter(|e| e.name == "team.agent.status")
            .collect();
        assert_eq!(status_events.len(), 1);
        assert_eq!(status_events[0].data["status"], "working");
    }

    #[tokio::test]
    async fn try_wake_non_idle_agent_returns_none() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();

        let payload = mgr.try_wake("worker-1").await.unwrap();
        assert!(payload.is_none());
    }

    #[tokio::test]
    async fn try_wake_nonexistent_agent_fails() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        let result = mgr.try_wake("ghost").await;
        assert!(matches!(result, Err(TeamError::AgentNotFound(_))));
    }

    // -- Anti-deadloop: Lead idle after turn ----------------------------------

    #[tokio::test]
    async fn lead_mark_idle_does_not_wake_self() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("lead-1", TeammateStatus::Working).await.unwrap();
        let wake_target = mgr.mark_idle("lead-1", None).await.unwrap();
        assert!(wake_target.is_none());
    }

    // -- Anti-deadloop: All teammates idle → wake leader ---------------------

    #[tokio::test]
    async fn all_teammates_idle_signals_wake_leader() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working).await.unwrap();

        let result = mgr.mark_idle("worker-1", None).await.unwrap();
        assert!(result.is_none(), "not all teammates idle yet");

        let result = mgr.mark_idle("worker-2", None).await.unwrap();
        assert_eq!(result.as_deref(), Some("lead-1"));
    }

    #[tokio::test]
    async fn partial_teammates_idle_does_not_wake_leader() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working).await.unwrap();

        let result = mgr.mark_idle("worker-1", None).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn leader_not_woken_if_already_working() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("lead-1", TeammateStatus::Working).await.unwrap();
        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();

        let result = mgr.mark_idle("worker-1", None).await.unwrap();
        assert!(result.is_none());
    }

    // -- Solo team (lead only, no teammates) ---------------------------------

    #[tokio::test]
    async fn solo_team_no_teammates_no_wake_signal() {
        let agents = vec![make_agent("lead-1", "Lead", TeammateRole::Lead)];
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("lead-1", TeammateStatus::Working).await.unwrap();
        let result = mgr.mark_idle("lead-1", None).await.unwrap();
        assert!(result.is_none());
    }

    // -- Agent lifecycle (add/remove/rename) ---------------------------------

    #[tokio::test]
    async fn add_agent_broadcasts_spawned_event() {
        let agents = make_team_agents();
        let (mgr, bc) = make_manager(&agents);

        let new_agent = make_agent("worker-3", "Worker3", TeammateRole::Teammate);
        mgr.add_agent(&new_agent).await;

        let all = mgr.list_agents().await;
        assert_eq!(all.len(), 4);

        let spawned_events: Vec<_> = bc
            .events()
            .into_iter()
            .filter(|e| e.name == "team.agent.spawned")
            .collect();
        assert_eq!(spawned_events.len(), 1);
    }

    #[tokio::test]
    async fn remove_agent_broadcasts_removed_event() {
        let agents = make_team_agents();
        let (mgr, bc) = make_manager(&agents);

        let conv_id = mgr.remove_agent("worker-2").await.unwrap();
        assert_eq!(conv_id.as_deref(), Some("conv-worker-2"));

        let all = mgr.list_agents().await;
        assert_eq!(all.len(), 2);

        let removed_events: Vec<_> = bc
            .events()
            .into_iter()
            .filter(|e| e.name == "team.agent.removed")
            .collect();
        assert_eq!(removed_events.len(), 1);
        assert_eq!(removed_events[0].data["slot_id"], "worker-2");
    }

    #[tokio::test]
    async fn remove_nonexistent_agent_fails() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        let result = mgr.remove_agent("ghost").await;
        assert!(matches!(result, Err(TeamError::AgentNotFound(_))));
    }

    #[tokio::test]
    async fn remove_agent_clears_wake_lock_timeout_and_finalize_dedup() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);
        let conv_id = "conv-worker-2"; // matches make_agent("worker-2")

        // Populate all three state stores for worker-2.
        assert!(mgr.acquire_wake_lock("worker-2"));
        let handle = tokio::spawn(async { tokio::time::sleep(Duration::from_secs(999)).await });
        mgr.wake_timeouts.insert("worker-2".into(), handle);
        assert!(mgr.begin_finalize(conv_id));

        mgr.remove_agent("worker-2").await.unwrap();

        assert!(
            !mgr.active_wakes.contains("worker-2"),
            "active_wakes must not retain a removed slot"
        );
        assert!(
            mgr.wake_timeouts.get("worker-2").is_none(),
            "wake_timeouts must not retain a removed slot"
        );
        assert!(
            mgr.finalized_turns.get(conv_id).is_none(),
            "finalized_turns must not retain the removed slot's conversation_id"
        );
    }

    #[tokio::test]
    async fn remove_agent_clear_state_is_idempotent() {
        // clear_agent_state tolerates missing entries — calling it on a slot
        // that never populated any of the three stores is a no-op, not a panic.
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.remove_agent("worker-1").await.unwrap();

        assert!(!mgr.active_wakes.contains("worker-1"));
        assert!(mgr.wake_timeouts.get("worker-1").is_none());
        assert!(mgr.finalized_turns.get("conv-worker-1").is_none());
    }

    #[tokio::test]
    async fn rename_agent_broadcasts_renamed_event() {
        let agents = make_team_agents();
        let (mgr, bc) = make_manager(&agents);

        mgr.rename_agent("worker-1", "Renamed Worker").await.unwrap();

        let agent = mgr.get_agent("worker-1").await.unwrap();
        assert_eq!(agent.name, "Renamed Worker");

        let renamed_events: Vec<_> = bc
            .events()
            .into_iter()
            .filter(|e| e.name == "team.agent.renamed")
            .collect();
        assert_eq!(renamed_events.len(), 1);
        assert_eq!(renamed_events[0].data["name"], "Renamed Worker");
    }

    // -- execute_action: SendMessage -----------------------------------------

    #[tokio::test]
    async fn execute_send_message_writes_to_mailbox() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        let action = SchedulerAction::SendMessage {
            to: "worker-1".into(),
            message: "Do task X".into(),
        };
        mgr.execute_action("lead-1", &action).await.unwrap();

        let unread = mailbox.read_unread("t1", "worker-1").await.unwrap();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].content, "Do task X");
        assert_eq!(unread[0].from_agent_id, "lead-1");
    }

    #[tokio::test]
    async fn execute_broadcast_message_writes_to_all_others() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        let action = SchedulerAction::SendMessage {
            to: "*".into(),
            message: "Attention all".into(),
        };
        mgr.execute_action("lead-1", &action).await.unwrap();

        let u1 = mailbox.read_unread("t1", "worker-1").await.unwrap();
        assert_eq!(u1.len(), 1);
        let u2 = mailbox.read_unread("t1", "worker-2").await.unwrap();
        assert_eq!(u2.len(), 1);
        let u_lead = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert!(u_lead.is_empty());
    }

    // -- execute_action: TaskCreate ------------------------------------------

    #[tokio::test]
    async fn execute_task_create() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox, task_board.clone(), broadcaster);

        let action = SchedulerAction::TaskCreate {
            subject: "Implement feature".into(),
            description: Some("Details here".into()),
            owner: Some("worker-1".into()),
            blocked_by: vec![],
        };
        mgr.execute_action("lead-1", &action).await.unwrap();

        let tasks = task_board.list_tasks("t1").await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].subject, "Implement feature");
        assert_eq!(tasks[0].owner.as_deref(), Some("worker-1"));
    }

    // -- execute_action: TaskUpdate ------------------------------------------

    #[tokio::test]
    async fn execute_task_update() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox, task_board.clone(), broadcaster);

        let task = task_board.create_task("t1", "Work", None, None, &[]).await.unwrap();

        let action = SchedulerAction::TaskUpdate {
            task_id: task.id.clone(),
            status: Some("in_progress".into()),
            description: None,
            owner: None,
            blocked_by: None,
        };
        mgr.execute_action("worker-1", &action).await.unwrap();

        let tasks = task_board.list_tasks("t1").await.unwrap();
        assert_eq!(tasks[0].status, crate::types::TaskStatus::InProgress);
    }

    // -- execute_action: IdleNotification ------------------------------------

    #[tokio::test]
    async fn execute_idle_notification_writes_to_lead_mailbox() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();

        let action = SchedulerAction::IdleNotification {
            summary: Some("Task done".into()),
        };
        mgr.execute_action("worker-1", &action).await.unwrap();

        assert_eq!(mgr.get_status("worker-1").await.unwrap(), TeammateStatus::Idle);

        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert_eq!(lead_msgs.len(), 1);
        assert_eq!(lead_msgs[0].msg_type, MailboxMessageType::IdleNotification);
        assert_eq!(lead_msgs[0].from_agent_id, "worker-1");
    }

    #[tokio::test]
    async fn lead_idle_notification_does_not_write_to_self() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        mgr.set_status("lead-1", TeammateStatus::Working).await.unwrap();

        let action = SchedulerAction::IdleNotification {
            summary: Some("Done delegating".into()),
        };
        mgr.execute_action("lead-1", &action).await.unwrap();

        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert!(lead_msgs.is_empty());
    }

    // -- execute_action: ShutdownAgent ---------------------------------------

    #[tokio::test]
    async fn execute_shutdown_agent_writes_shutdown_request() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        let action = SchedulerAction::ShutdownAgent {
            slot_id: "worker-1".into(),
            reason: Some("No longer needed".into()),
        };
        mgr.execute_action("lead-1", &action).await.unwrap();

        let msgs = mailbox.read_unread("t1", "worker-1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].msg_type, MailboxMessageType::ShutdownRequest);
        assert_eq!(msgs[0].content, "No longer needed");
    }

    #[tokio::test]
    async fn non_lead_cannot_shutdown_agent() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        let action = SchedulerAction::ShutdownAgent {
            slot_id: "worker-2".into(),
            reason: None,
        };
        let result = mgr.execute_action("worker-1", &action).await;
        assert!(matches!(result, Err(TeamError::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn lead_cannot_shutdown_lead() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        let action = SchedulerAction::ShutdownAgent {
            slot_id: "lead-1".into(),
            reason: Some("trying to shutdown self".into()),
        };
        let result = mgr.execute_action("lead-1", &action).await;
        assert!(
            matches!(&result, Err(TeamError::InvalidRequest(msg)) if msg.contains("lead")),
            "lead shutting down lead must be rejected, got {result:?}"
        );

        // No ShutdownRequest message should have been written to the lead's mailbox.
        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert!(lead_msgs.is_empty());
    }

    #[tokio::test]
    async fn lead_can_shutdown_worker() {
        // Positive-path sanity check that the new target-role guard does not
        // regress the normal shutdown flow.
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        let action = SchedulerAction::ShutdownAgent {
            slot_id: "worker-1".into(),
            reason: Some("not needed".into()),
        };
        mgr.execute_action("lead-1", &action).await.unwrap();

        let msgs = mailbox.read_unread("t1", "worker-1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].msg_type, MailboxMessageType::ShutdownRequest);
    }

    // -- execute_action: RenameAgent -----------------------------------------

    #[tokio::test]
    async fn execute_rename_agent() {
        let agents = make_team_agents();
        let (mgr, bc) = make_manager(&agents);

        let action = SchedulerAction::RenameAgent {
            slot_id: "worker-1".into(),
            new_name: "SuperWorker".into(),
        };
        mgr.execute_action("lead-1", &action).await.unwrap();

        let agent = mgr.get_agent("worker-1").await.unwrap();
        assert_eq!(agent.name, "SuperWorker");

        let renamed: Vec<_> = bc
            .events()
            .into_iter()
            .filter(|e| e.name == "team.agent.renamed")
            .collect();
        assert_eq!(renamed.len(), 1);
    }

    // -- finalize_turn -------------------------------------------------------

    #[tokio::test]
    async fn finalize_turn_executes_actions_and_marks_idle() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board.clone(), broadcaster);

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working).await.unwrap();

        let actions = vec![
            SchedulerAction::TaskCreate {
                subject: "Sub-task".into(),
                description: None,
                owner: None,
                blocked_by: vec![],
            },
            SchedulerAction::SendMessage {
                to: "lead-1".into(),
                message: "Done with sub-task".into(),
            },
        ];

        let wake_signal = mgr.finalize_turn("worker-1", &actions).await.unwrap();

        assert_eq!(mgr.get_status("worker-1").await.unwrap(), TeammateStatus::Idle);

        let tasks = task_board.list_tasks("t1").await.unwrap();
        assert_eq!(tasks.len(), 1);

        // Two messages arrive at the lead:
        // 1. the explicit SendMessage from the action list ("Done with sub-task")
        // 2. the IdleNotification that mark_idle now writes automatically
        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert_eq!(lead_msgs.len(), 2);
        assert!(
            lead_msgs
                .iter()
                .any(|m| m.msg_type == MailboxMessageType::Message && m.content == "Done with sub-task")
        );
        assert!(
            lead_msgs
                .iter()
                .any(|m| m.msg_type == MailboxMessageType::IdleNotification)
        );

        assert!(wake_signal.is_none(), "worker-2 still working");
    }

    #[tokio::test]
    async fn finalize_turn_with_idle_notification_skips_double_idle() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox, task_board, broadcaster.clone());

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();

        let actions = vec![SchedulerAction::IdleNotification {
            summary: Some("All done".into()),
        }];

        mgr.finalize_turn("worker-1", &actions).await.unwrap();

        assert_eq!(mgr.get_status("worker-1").await.unwrap(), TeammateStatus::Idle);

        let idle_events: Vec<_> = broadcaster
            .events()
            .into_iter()
            .filter(|e| e.name == "team.agent.status" && e.data["status"] == "idle")
            .collect();
        assert_eq!(idle_events.len(), 1, "idle should be set exactly once");
    }

    #[tokio::test]
    async fn finalize_turn_all_teammates_done_signals_leader_wake() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox, task_board, broadcaster);

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working).await.unwrap();

        mgr.finalize_turn("worker-1", &[]).await.unwrap();

        let wake_signal = mgr.finalize_turn("worker-2", &[]).await.unwrap();
        assert_eq!(wake_signal.as_deref(), Some("lead-1"));
    }

    // -- build_wake_payload with unread messages and tasks --------------------

    #[tokio::test]
    async fn wake_payload_includes_tasks_and_unread() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board.clone(), broadcaster);

        task_board.create_task("t1", "Task A", None, None, &[]).await.unwrap();

        mailbox
            .write(
                "t1",
                "worker-1",
                "lead-1",
                MailboxMessageType::Message,
                "Do task A",
                None,
            )
            .await
            .unwrap();

        let payload = mgr.build_wake_payload("worker-1").await.unwrap();
        assert_eq!(payload.tasks.len(), 1);
        assert_eq!(payload.unread_messages.len(), 1);
        assert_eq!(payload.unread_messages[0].content, "Do task A");
    }

    // -- D8: mark_idle writes IdleNotification with summary -------------------

    #[tokio::test]
    async fn mark_idle_with_summary_writes_idle_notification_to_lead() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();
        mgr.mark_idle("worker-1", Some("sub-task done")).await.unwrap();

        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert_eq!(lead_msgs.len(), 1);
        assert_eq!(lead_msgs[0].msg_type, MailboxMessageType::IdleNotification);
        assert_eq!(lead_msgs[0].from_agent_id, "worker-1");
        assert_eq!(lead_msgs[0].content, "sub-task done");
        assert_eq!(lead_msgs[0].summary.as_deref(), Some("sub-task done"));
    }

    #[tokio::test]
    async fn mark_idle_without_summary_still_writes_fallback_content() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();
        mgr.mark_idle("worker-1", None).await.unwrap();

        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert_eq!(lead_msgs.len(), 1);
        assert_eq!(lead_msgs[0].content, "idle");
        assert!(lead_msgs[0].summary.is_none());
    }

    #[tokio::test]
    async fn mark_idle_from_lead_does_not_write_notification() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        mgr.set_status("lead-1", TeammateStatus::Working).await.unwrap();
        mgr.mark_idle("lead-1", Some("done")).await.unwrap();

        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert!(lead_msgs.is_empty());
    }

    #[tokio::test]
    async fn mark_idle_broadcasts_status_event() {
        let agents = make_team_agents();
        let (mgr, bc) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();
        bc.events.lock().unwrap().clear();

        mgr.mark_idle("worker-1", Some("ok")).await.unwrap();

        let idle_events: Vec<_> = bc
            .events()
            .into_iter()
            .filter(|e| e.name == "team.agent.status" && e.data["status"] == "idle")
            .collect();
        assert_eq!(idle_events.len(), 1);
        assert_eq!(idle_events[0].data["slot_id"], "worker-1");
    }

    // -- D8: settled set expansion ({Idle, Completed, Error}) -----------------

    #[tokio::test]
    async fn all_teammates_settled_with_completed_wakes_leader() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Completed).await.unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working).await.unwrap();

        let result = mgr.mark_idle("worker-2", None).await.unwrap();
        assert_eq!(result.as_deref(), Some("lead-1"));
    }

    #[tokio::test]
    async fn all_teammates_settled_with_error_wakes_leader() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Error).await.unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working).await.unwrap();

        let result = mgr.mark_idle("worker-2", None).await.unwrap();
        assert_eq!(
            result.as_deref(),
            Some("lead-1"),
            "Error counts as settled — leader should be woken"
        );
    }

    #[tokio::test]
    async fn working_teammate_blocks_leader_wake() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working).await.unwrap();

        let result = mgr.mark_idle("worker-1", None).await.unwrap();
        assert!(result.is_none(), "worker-2 still Working blocks wake");
    }

    #[tokio::test]
    async fn thinking_teammate_blocks_leader_wake() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Thinking).await.unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working).await.unwrap();

        let result = mgr.mark_idle("worker-2", None).await.unwrap();
        assert!(result.is_none(), "Thinking is not settled");
    }

    #[tokio::test]
    async fn tool_use_teammate_blocks_leader_wake() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::ToolUse).await.unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working).await.unwrap();

        let result = mgr.mark_idle("worker-2", None).await.unwrap();
        assert!(result.is_none(), "ToolUse is not settled");
    }

    // -- D8: acquire_wake_lock / release_wake_lock ----------------------------

    #[tokio::test]
    async fn wake_lock_first_caller_wins_and_release_is_idempotent() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        assert!(mgr.acquire_wake_lock("worker-1"));
        assert!(
            !mgr.acquire_wake_lock("worker-1"),
            "second acquire must fail while lock is held"
        );

        mgr.release_wake_lock("worker-1");
        assert!(mgr.acquire_wake_lock("worker-1"), "lock is reusable after release");

        mgr.release_wake_lock("worker-1");
        mgr.release_wake_lock("worker-1"); // double release is a no-op
    }

    #[tokio::test]
    async fn wake_lock_is_scoped_per_slot() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        assert!(mgr.acquire_wake_lock("worker-1"));
        assert!(mgr.acquire_wake_lock("worker-2"), "different slot must not be blocked");
    }

    // -- W4-D19a: finalize-turn dedup ----------------------------------------

    #[tokio::test]
    async fn begin_finalize_first_call_returns_true() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        assert!(mgr.begin_finalize("conv-worker-1"));
    }

    #[tokio::test]
    async fn begin_finalize_within_window_returns_false() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        assert!(mgr.begin_finalize("conv-worker-1"));
        assert!(
            !mgr.begin_finalize("conv-worker-1"),
            "second finalize within 5s window must be deduped"
        );
    }

    #[tokio::test]
    async fn clear_finalized_turn_allows_immediate_retry() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        assert!(mgr.begin_finalize("conv-worker-1"));
        mgr.clear_finalized_turn("conv-worker-1");
        assert!(
            mgr.begin_finalize("conv-worker-1"),
            "clearing the dedup entry must let the next finalize proceed"
        );
    }

    #[tokio::test]
    async fn wake_lock_concurrent_acquire_exactly_one_succeeds() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);
        let mgr = Arc::new(mgr);

        let mut handles = vec![];
        for _ in 0..16 {
            let mgr = mgr.clone();
            handles.push(tokio::spawn(async move { mgr.acquire_wake_lock("worker-1") }));
        }

        let mut winners = 0usize;
        for h in handles {
            if h.await.unwrap() {
                winners += 1;
            }
        }
        assert_eq!(winners, 1, "exactly one concurrent acquire should win the lock");
    }

    // -- W4-D18b: wake_timeouts -------------------------------------------------

    #[tokio::test]
    async fn clear_wake_timeout_removes_entry() {
        let handle = tokio::spawn(async { tokio::time::sleep(std::time::Duration::from_secs(999)).await });
        let map: DashMap<String, tokio::task::JoinHandle<()>> = DashMap::new();
        map.insert("slot-1".into(), handle);
        // Simulate clear
        if let Some((_, h)) = map.remove("slot-1") {
            h.abort();
        }
        assert!(map.get("slot-1").is_none());
    }

    #[test]
    fn clear_nonexistent_slot_no_panic() {
        let map: DashMap<String, tokio::task::JoinHandle<()>> = DashMap::new();
        // Should not panic
        map.remove("nonexistent");
    }

    // -- W4-D18b-2: arm_wake_timeout --------------------------------------------

    use std::sync::atomic::{AtomicU32, Ordering};

    fn counting_handler(counter: Arc<AtomicU32>) -> WakeTimeoutHandler {
        Arc::new(move |_slot_id: String| {
            let c = counter.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
            })
        })
    }

    async fn wait_for_map_empty(mgr: &TeammateManager, slot_id: &str, ticks: u32) {
        for _ in 0..ticks {
            if mgr.wake_timeouts.get(slot_id).is_none() {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    /// Yield repeatedly so a freshly spawned watchdog task gets a chance to
    /// reach its `select!` (and arm its sleep) before the test advances time.
    async fn let_watchdog_settle() {
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn arm_wake_timeout_fires_handler_after_deadline() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);
        let counter = Arc::new(AtomicU32::new(0));
        let (tx, rx) = broadcast::channel::<AgentStreamChunk>(8);

        mgr.arm_wake_timeout("worker-1", rx, counting_handler(counter.clone()));
        // Keep the sender alive so the channel does not close before the deadline.
        let_watchdog_settle().await;
        // Advance slightly past the deadline — handler must fire exactly once.
        tokio::time::advance(Duration::from_millis(WAKE_TIMEOUT_MS + 500)).await;
        wait_for_map_empty(&mgr, "worker-1", 128).await;

        assert_eq!(counter.load(Ordering::SeqCst), 1, "handler must fire on inactivity");
        assert!(
            mgr.wake_timeouts.get("worker-1").is_none(),
            "map entry must be cleared after watchdog exit"
        );
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn arm_wake_timeout_activity_resets_deadline() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);
        let counter = Arc::new(AtomicU32::new(0));
        let (tx, rx) = broadcast::channel::<AgentStreamChunk>(8);

        mgr.arm_wake_timeout("worker-1", rx, counting_handler(counter.clone()));
        let_watchdog_settle().await;

        // Just before the first deadline, an activity chunk arrives.
        tokio::time::advance(Duration::from_millis(WAKE_TIMEOUT_MS - 1_000)).await;
        tx.send(AgentStreamChunk::Text { text: "hi".into() }).unwrap();
        // Let the select branch observe the chunk before advancing again.
        tokio::task::yield_now().await;

        // Advance another near-full window — deadline should have been reset,
        // so no timeout has fired yet.
        tokio::time::advance(Duration::from_millis(WAKE_TIMEOUT_MS - 1_000)).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "activity must reset deadline; handler must not have fired"
        );

        // Cross the new deadline — handler fires.
        tokio::time::advance(Duration::from_millis(2_000)).await;
        wait_for_map_empty(&mgr, "worker-1", 128).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn arm_wake_timeout_finish_exits_without_firing() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);
        let counter = Arc::new(AtomicU32::new(0));
        let (tx, rx) = broadcast::channel::<AgentStreamChunk>(8);

        mgr.arm_wake_timeout("worker-1", rx, counting_handler(counter.clone()));
        let_watchdog_settle().await;

        tx.send(AgentStreamChunk::Finish {
            agent_crash: false,
            stop_reason: Some("end_turn".into()),
        })
        .unwrap();
        wait_for_map_empty(&mgr, "worker-1", 128).await;

        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "Finish must not trigger timeout handler"
        );
        assert!(
            mgr.wake_timeouts.get("worker-1").is_none(),
            "map entry must be cleared after Finish"
        );

        // Advance past the would-be deadline to make sure no lingering timer fires.
        tokio::time::advance(Duration::from_millis(WAKE_TIMEOUT_MS * 2)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn arm_wake_timeout_channel_close_exits_without_firing() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);
        let counter = Arc::new(AtomicU32::new(0));
        let (tx, rx) = broadcast::channel::<AgentStreamChunk>(8);

        mgr.arm_wake_timeout("worker-1", rx, counting_handler(counter.clone()));
        let_watchdog_settle().await;
        drop(tx);
        wait_for_map_empty(&mgr, "worker-1", 128).await;

        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "closed channel must not fire inactivity handler"
        );
        assert!(mgr.wake_timeouts.get("worker-1").is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn arm_wake_timeout_replaces_existing_watchdog() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);
        let counter_a = Arc::new(AtomicU32::new(0));
        let counter_b = Arc::new(AtomicU32::new(0));

        let (tx_a, rx_a) = broadcast::channel::<AgentStreamChunk>(8);
        mgr.arm_wake_timeout("worker-1", rx_a, counting_handler(counter_a.clone()));

        // Immediately re-arm — the first watchdog must be aborted.
        let (tx_b, rx_b) = broadcast::channel::<AgentStreamChunk>(8);
        mgr.arm_wake_timeout("worker-1", rx_b, counting_handler(counter_b.clone()));
        let_watchdog_settle().await;

        // Cross the deadline. Only one handler (the second watchdog's) may fire.
        tokio::time::advance(Duration::from_millis(WAKE_TIMEOUT_MS + 500)).await;
        wait_for_map_empty(&mgr, "worker-1", 128).await;

        assert_eq!(counter_a.load(Ordering::SeqCst), 0, "aborted watchdog must not fire");
        assert_eq!(counter_b.load(Ordering::SeqCst), 1, "replacement watchdog must fire");
        drop(tx_a);
        drop(tx_b);
    }

    // -- W4-D20b1: crash testament formatting -----------------------------------

    #[test]
    fn crash_testament_contains_reason_keyword() {
        use crate::crash_detection::CrashReason;

        for (reason, keyword) in [
            (CrashReason::ProcessExited, "ProcessExited"),
            (CrashReason::SessionNotFound, "SessionNotFound"),
            (CrashReason::Unknown("segfault".into()), "Unknown — segfault"),
        ] {
            let testament = format_crash_testament("Bob", &reason, None);
            assert!(
                testament.contains(keyword),
                "expected '{}' in testament: {}",
                keyword,
                testament
            );
        }
    }

    #[test]
    fn crash_testament_includes_last_message_when_provided() {
        use crate::crash_detection::CrashReason;

        let testament = format_crash_testament("Alice", &CrashReason::ProcessExited, Some("working on task X"));
        assert!(testament.contains("Last message: working on task X"));
        assert!(testament.contains("ProcessExited"));
        assert!(testament.contains("Alice"));
    }

    #[test]
    fn crash_testament_omits_last_message_when_none() {
        use crate::crash_detection::CrashReason;

        let testament = format_crash_testament("Charlie", &CrashReason::SessionNotFound, None);
        assert!(!testament.contains("Last message"));
        assert!(testament.contains("SessionNotFound"));
        assert!(testament.contains("Charlie"));
    }

    #[tokio::test]
    async fn write_crash_testament_delivers_to_lead_mailbox() {
        use crate::crash_detection::CrashReason;

        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        mgr.write_crash_testament("worker-1", "Worker1", &CrashReason::ProcessExited, None)
            .await
            .unwrap();

        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert_eq!(lead_msgs.len(), 1);
        assert_eq!(lead_msgs[0].from_agent_id, "worker-1");
        assert!(lead_msgs[0].content.contains("ProcessExited"));
        assert!(lead_msgs[0].content.contains("Worker1"));
    }

    #[tokio::test]
    async fn write_crash_testament_noop_when_no_lead() {
        use crate::crash_detection::CrashReason;

        // Team with no lead
        let agents = vec![
            make_agent("worker-1", "Worker1", TeammateRole::Teammate),
            make_agent("worker-2", "Worker2", TeammateRole::Teammate),
        ];
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        // Should not panic or error
        mgr.write_crash_testament("worker-1", "Worker1", &CrashReason::SessionNotFound, Some("last words"))
            .await
            .unwrap();

        // No messages delivered to anyone
        let msgs1 = mailbox.read_unread("t1", "worker-1").await.unwrap();
        let msgs2 = mailbox.read_unread("t1", "worker-2").await.unwrap();
        assert!(msgs1.is_empty());
        assert!(msgs2.is_empty());
    }

    #[tokio::test]
    async fn write_crash_testament_noop_when_lead_crashes() {
        use crate::crash_detection::CrashReason;

        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        // Lead crashing should not write to itself
        mgr.write_crash_testament("lead-1", "Lead", &CrashReason::ProcessExited, None)
            .await
            .unwrap();

        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert!(lead_msgs.is_empty());
    }

    // -- W4-D20b-2: handle_agent_crash -----------------------------------------

    #[tokio::test]
    async fn handle_agent_crash_marks_slot_as_error() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();

        let wake_target = mgr
            .handle_agent_crash("worker-1", CrashReason::ProcessExited, None)
            .await
            .unwrap();

        assert_eq!(
            mgr.get_status("worker-1").await.unwrap(),
            TeammateStatus::Error,
            "crashed slot must end in Error (aka Failed)"
        );
        assert_eq!(wake_target, Some("lead-1".to_string()));
    }

    #[tokio::test]
    async fn handle_agent_crash_releases_wake_lock() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        assert!(mgr.acquire_wake_lock("worker-1"));

        mgr.handle_agent_crash("worker-1", CrashReason::SessionNotFound, Some("last words"))
            .await
            .unwrap();

        assert!(
            mgr.acquire_wake_lock("worker-1"),
            "wake lock must be released after crash so the slot is reusable"
        );
    }

    #[tokio::test]
    async fn handle_agent_crash_writes_testament_to_lead() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        mgr.handle_agent_crash("worker-1", CrashReason::Unknown("segfault".into()), Some("cleaning up"))
            .await
            .unwrap();

        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert_eq!(lead_msgs.len(), 1);
        assert_eq!(lead_msgs[0].from_agent_id, "worker-1");
        assert!(lead_msgs[0].content.contains("Worker1"));
        assert!(lead_msgs[0].content.contains("segfault"));
        assert!(lead_msgs[0].content.contains("cleaning up"));
    }

    #[tokio::test]
    async fn handle_agent_crash_returns_lead_slot_for_teammate_crash() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        let wake_target = mgr
            .handle_agent_crash("worker-2", CrashReason::ProcessExited, None)
            .await
            .unwrap();

        assert_eq!(
            wake_target,
            Some("lead-1".to_string()),
            "caller needs the lead slot id to trigger a wake"
        );
    }

    // -- W4-D20c: handle_agent_crash leader branch -----------------------------

    #[tokio::test]
    async fn handle_agent_crash_leader_branch_returns_none() {
        // Leader crash has no higher-ranked agent to wake. handle_agent_crash
        // must not self-wake and must not remove the leader slot — downstream
        // session code inspects the leader entry to emit the error event.
        // Local state (status/locks) still gets cleaned so nothing leaks.
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        mgr.set_status("lead-1", TeammateStatus::Working).await.unwrap();
        assert!(mgr.acquire_wake_lock("lead-1"));

        let wake_target = mgr
            .handle_agent_crash("lead-1", CrashReason::ProcessExited, Some("last words"))
            .await
            .unwrap();

        assert_eq!(wake_target, None, "leader crash must not self-wake");
        assert_eq!(mgr.get_status("lead-1").await.unwrap(), TeammateStatus::Error);
        assert!(
            mgr.acquire_wake_lock("lead-1"),
            "lock must be released even for the leader branch"
        );

        // Leader cannot write a testament to itself — the mailbox must be
        // empty, otherwise the leader would read its own death notice on a
        // future resume.
        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert!(
            lead_msgs.is_empty(),
            "leader crash must not produce a self-addressed testament"
        );
    }

    #[tokio::test]
    async fn handle_agent_crash_leader_keeps_agents_list_intact() {
        // Leader crash must not remove slots from the roster — the session
        // layer still needs to enumerate teammates to emit finalization /
        // error events.
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        let before: Vec<String> = mgr.list_agents().await.into_iter().map(|a| a.slot_id).collect();

        mgr.handle_agent_crash("lead-1", CrashReason::SessionNotFound, None)
            .await
            .unwrap();

        let after: Vec<String> = mgr.list_agents().await.into_iter().map(|a| a.slot_id).collect();

        assert_eq!(before, after, "leader crash must preserve the agents list");
    }

    #[tokio::test]
    async fn handle_agent_crash_leader_clears_wake_timeout() {
        // Pending wake timeouts for the leader must be cancelled on crash —
        // the slot will never answer, so a lingering timer only risks a late
        // spurious callback.
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        let handle = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(999)).await;
        });
        mgr.wake_timeouts.insert("lead-1".into(), handle);

        mgr.handle_agent_crash("lead-1", CrashReason::ProcessExited, None)
            .await
            .unwrap();

        assert!(
            mgr.wake_timeouts.get("lead-1").is_none(),
            "wake timeout entry must be removed after leader crash"
        );
    }

    #[tokio::test]
    async fn handle_agent_crash_clears_wake_timeout() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        // Install a long-running dummy timeout so we can observe that it was
        // cancelled once the crash handler ran.
        let handle = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(999)).await;
        });
        mgr.wake_timeouts.insert("worker-1".into(), handle);

        mgr.handle_agent_crash("worker-1", CrashReason::ProcessExited, None)
            .await
            .unwrap();

        assert!(
            mgr.wake_timeouts.get("worker-1").is_none(),
            "wake timeout entry must be removed after crash"
        );
    }

    #[tokio::test]
    async fn handle_agent_crash_unknown_slot_errors() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        let result = mgr.handle_agent_crash("ghost", CrashReason::ProcessExited, None).await;

        assert!(matches!(result, Err(TeamError::AgentNotFound(_))));
    }

    // -- W4-D22: handle_inactivity_timeout -------------------------------------

    #[tokio::test]
    async fn handle_inactivity_timeout_teammate_marks_error_and_wakes_lead() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        mgr.set_status("worker-1", TeammateStatus::Working).await.unwrap();

        let wake_target = mgr.handle_inactivity_timeout("worker-1").await.unwrap();

        assert_eq!(
            mgr.get_status("worker-1").await.unwrap(),
            TeammateStatus::Error,
            "stuck slot must end in Error"
        );
        assert_eq!(wake_target, Some("lead-1".to_string()));

        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert_eq!(lead_msgs.len(), 1);
        assert_eq!(lead_msgs[0].from_agent_id, "worker-1");
        assert_eq!(lead_msgs[0].msg_type, MailboxMessageType::Message);
        assert!(lead_msgs[0].content.contains("Worker1"));
        assert!(lead_msgs[0].content.contains("timed out"));
    }

    #[tokio::test]
    async fn handle_inactivity_timeout_leader_returns_none_no_mailbox_write() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        mgr.set_status("lead-1", TeammateStatus::Working).await.unwrap();
        assert!(mgr.acquire_wake_lock("lead-1"));

        let wake_target = mgr.handle_inactivity_timeout("lead-1").await.unwrap();

        assert_eq!(wake_target, None, "leader inactivity must not self-wake");
        assert_eq!(mgr.get_status("lead-1").await.unwrap(), TeammateStatus::Error);
        assert!(
            mgr.acquire_wake_lock("lead-1"),
            "lock must be released even when leader stuck"
        );

        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert!(
            lead_msgs.is_empty(),
            "leader must not receive a self-addressed timeout message"
        );
    }

    #[tokio::test]
    async fn handle_inactivity_timeout_releases_wake_lock() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        assert!(mgr.acquire_wake_lock("worker-1"));

        mgr.handle_inactivity_timeout("worker-1").await.unwrap();

        assert!(
            mgr.acquire_wake_lock("worker-1"),
            "wake lock must be released after inactivity timeout"
        );
    }

    #[tokio::test]
    async fn handle_inactivity_timeout_clears_wake_timeout_entry() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        let handle = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(999)).await;
        });
        mgr.wake_timeouts.insert("worker-1".into(), handle);

        mgr.handle_inactivity_timeout("worker-1").await.unwrap();

        assert!(
            mgr.wake_timeouts.get("worker-1").is_none(),
            "wake timeout entry must be removed after inactivity recovery"
        );
    }

    #[tokio::test]
    async fn handle_inactivity_timeout_unknown_slot_errors() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        let result = mgr.handle_inactivity_timeout("ghost").await;

        assert!(matches!(result, Err(TeamError::AgentNotFound(_))));
    }

    #[tokio::test]
    async fn handle_inactivity_timeout_no_lead_returns_none() {
        // Team with no lead: a stuck teammate has nowhere to route the
        // diagnostic message. The handler must still clean local state
        // and must not panic or return an error.
        let agents = vec![
            make_agent("worker-1", "Worker1", TeammateRole::Teammate),
            make_agent("worker-2", "Worker2", TeammateRole::Teammate),
        ];
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        let wake_target = mgr.handle_inactivity_timeout("worker-1").await.unwrap();

        assert_eq!(wake_target, None);
        assert_eq!(mgr.get_status("worker-1").await.unwrap(), TeammateStatus::Error);

        let msgs2 = mailbox.read_unread("t1", "worker-2").await.unwrap();
        assert!(msgs2.is_empty());
    }

    // -- W5-D30b: notify_shutdown_rejected -------------------------------------

    #[tokio::test]
    async fn notify_shutdown_rejected_delivers_to_lead_mailbox() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        mgr.notify_shutdown_rejected("worker-1", "still working on task X")
            .await
            .unwrap();

        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert_eq!(lead_msgs.len(), 1);
        assert_eq!(lead_msgs[0].from_agent_id, "worker-1");
        assert!(lead_msgs[0].content.contains("Worker1"));
        assert!(lead_msgs[0].content.contains("declined shutdown"));
        assert!(lead_msgs[0].content.contains("still working on task X"));

        // Agent was not removed
        assert!(mgr.get_agent("worker-1").await.is_ok());
    }

    #[tokio::test]
    async fn notify_shutdown_rejected_noop_when_no_lead() {
        let agents = vec![
            make_agent("worker-1", "Worker1", TeammateRole::Teammate),
            make_agent("worker-2", "Worker2", TeammateRole::Teammate),
        ];
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        mgr.notify_shutdown_rejected("worker-1", "busy").await.unwrap();

        let msgs1 = mailbox.read_unread("t1", "worker-1").await.unwrap();
        let msgs2 = mailbox.read_unread("t1", "worker-2").await.unwrap();
        assert!(msgs1.is_empty());
        assert!(msgs2.is_empty());
    }

    #[tokio::test]
    async fn notify_shutdown_rejected_noop_when_sender_is_lead() {
        let agents = make_team_agents();
        let repo = Arc::new(MockTeamRepo::new());
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(RecordingBroadcaster::new());
        let mgr = TeammateManager::new("t1".into(), &agents, mailbox.clone(), task_board, broadcaster);

        mgr.notify_shutdown_rejected("lead-1", "irrelevant").await.unwrap();

        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert!(lead_msgs.is_empty());
    }
}
