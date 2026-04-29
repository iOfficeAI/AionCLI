use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aionui_realtime::EventBroadcaster;
use dashmap::{DashMap, DashSet};
use tokio::sync::Mutex;
use tracing::debug;

use crate::error::TeamError;
use crate::events::TeamEventEmitter;
use crate::mailbox::Mailbox;
use crate::task_board::TaskBoard;
use crate::types::{
    MailboxMessage, MailboxMessageType, TeamAgent, TeamTask, TeammateRole, TeammateStatus,
};

pub const WAKE_TIMEOUT_MS: u64 = 60_000;

const FINALIZE_DEDUP_WINDOW: Duration = Duration::from_secs(5);

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
}

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
    pub async fn mark_idle(
        &self,
        slot_id: &str,
        summary: Option<&str>,
    ) -> Result<Option<String>, TeamError> {
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
                self.task_board
                    .update_task(&self.team_id, task_id, &update)
                    .await?;
                Ok(None)
            }
            SchedulerAction::IdleNotification { summary } => {
                self.handle_idle_notification(from_slot_id, summary.as_deref())
                    .await
            }
            SchedulerAction::SpawnAgent {
                name,
                role,
                backend,
            } => {
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
    pub async fn finalize_turn(
        &self,
        slot_id: &str,
        actions: &[SchedulerAction],
    ) -> Result<Option<String>, TeamError> {
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
    pub async fn remove_agent(&self, slot_id: &str) -> Result<(), TeamError> {
        let mut slots = self.slots.lock().await;
        slots
            .remove(slot_id)
            .ok_or_else(|| TeamError::AgentNotFound(slot_id.to_owned()))?;
        drop(slots);
        self.events.broadcast_agent_removed(slot_id);
        debug!(team_id = %self.team_id, slot_id, "agent removed from scheduler");
        Ok(())
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

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    async fn handle_send_message(
        &self,
        from_slot_id: &str,
        to: &str,
        message: &str,
    ) -> Result<(), TeamError> {
        if to == "*" {
            let slots = self.slots.lock().await;
            let targets: Vec<String> = slots
                .keys()
                .filter(|id| id.as_str() != from_slot_id)
                .cloned()
                .collect();
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
            return Err(TeamError::InvalidRequest(
                "only lead can shutdown agents".into(),
            ));
        }

        {
            let slots = self.slots.lock().await;
            if !slots.contains_key(target_slot_id) {
                return Err(TeamError::AgentNotFound(target_slot_id.to_owned()));
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
        let mgr = TeammateManager::new(
            "t1".into(),
            agents,
            mailbox,
            task_board,
            broadcaster.clone(),
        );
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

        mgr.set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();

        assert_eq!(
            mgr.get_status("worker-1").await.unwrap(),
            TeammateStatus::Working
        );

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

        assert_eq!(
            mgr.get_status("worker-1").await.unwrap(),
            TeammateStatus::Working
        );

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

        mgr.set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();

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

        mgr.set_status("lead-1", TeammateStatus::Working)
            .await
            .unwrap();
        let wake_target = mgr.mark_idle("lead-1", None).await.unwrap();
        assert!(wake_target.is_none());
    }

    // -- Anti-deadloop: All teammates idle → wake leader ---------------------

    #[tokio::test]
    async fn all_teammates_idle_signals_wake_leader() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working)
            .await
            .unwrap();

        let result = mgr.mark_idle("worker-1", None).await.unwrap();
        assert!(result.is_none(), "not all teammates idle yet");

        let result = mgr.mark_idle("worker-2", None).await.unwrap();
        assert_eq!(result.as_deref(), Some("lead-1"));
    }

    #[tokio::test]
    async fn partial_teammates_idle_does_not_wake_leader() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working)
            .await
            .unwrap();

        let result = mgr.mark_idle("worker-1", None).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn leader_not_woken_if_already_working() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("lead-1", TeammateStatus::Working)
            .await
            .unwrap();
        mgr.set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();

        let result = mgr.mark_idle("worker-1", None).await.unwrap();
        assert!(result.is_none());
    }

    // -- Solo team (lead only, no teammates) ---------------------------------

    #[tokio::test]
    async fn solo_team_no_teammates_no_wake_signal() {
        let agents = vec![make_agent("lead-1", "Lead", TeammateRole::Lead)];
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("lead-1", TeammateStatus::Working)
            .await
            .unwrap();
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

        mgr.remove_agent("worker-2").await.unwrap();

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
    async fn rename_agent_broadcasts_renamed_event() {
        let agents = make_team_agents();
        let (mgr, bc) = make_manager(&agents);

        mgr.rename_agent("worker-1", "Renamed Worker")
            .await
            .unwrap();

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
        let mgr = TeammateManager::new(
            "t1".into(),
            &agents,
            mailbox.clone(),
            task_board,
            broadcaster,
        );

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
        let mgr = TeammateManager::new(
            "t1".into(),
            &agents,
            mailbox.clone(),
            task_board,
            broadcaster,
        );

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
        let mgr = TeammateManager::new(
            "t1".into(),
            &agents,
            mailbox,
            task_board.clone(),
            broadcaster,
        );

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
        let mgr = TeammateManager::new(
            "t1".into(),
            &agents,
            mailbox,
            task_board.clone(),
            broadcaster,
        );

        let task = task_board
            .create_task("t1", "Work", None, None, &[])
            .await
            .unwrap();

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
        let mgr = TeammateManager::new(
            "t1".into(),
            &agents,
            mailbox.clone(),
            task_board,
            broadcaster,
        );

        mgr.set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();

        let action = SchedulerAction::IdleNotification {
            summary: Some("Task done".into()),
        };
        mgr.execute_action("worker-1", &action).await.unwrap();

        assert_eq!(
            mgr.get_status("worker-1").await.unwrap(),
            TeammateStatus::Idle
        );

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
        let mgr = TeammateManager::new(
            "t1".into(),
            &agents,
            mailbox.clone(),
            task_board,
            broadcaster,
        );

        mgr.set_status("lead-1", TeammateStatus::Working)
            .await
            .unwrap();

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
        let mgr = TeammateManager::new(
            "t1".into(),
            &agents,
            mailbox.clone(),
            task_board,
            broadcaster,
        );

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
        let mgr = TeammateManager::new(
            "t1".into(),
            &agents,
            mailbox.clone(),
            task_board.clone(),
            broadcaster,
        );

        mgr.set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working)
            .await
            .unwrap();

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

        assert_eq!(
            mgr.get_status("worker-1").await.unwrap(),
            TeammateStatus::Idle
        );

        let tasks = task_board.list_tasks("t1").await.unwrap();
        assert_eq!(tasks.len(), 1);

        // Two messages arrive at the lead:
        // 1. the explicit SendMessage from the action list ("Done with sub-task")
        // 2. the IdleNotification that mark_idle now writes automatically
        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert_eq!(lead_msgs.len(), 2);
        assert!(lead_msgs.iter().any(
            |m| m.msg_type == MailboxMessageType::Message && m.content == "Done with sub-task"
        ));
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
        let mgr = TeammateManager::new(
            "t1".into(),
            &agents,
            mailbox,
            task_board,
            broadcaster.clone(),
        );

        mgr.set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();

        let actions = vec![SchedulerAction::IdleNotification {
            summary: Some("All done".into()),
        }];

        mgr.finalize_turn("worker-1", &actions).await.unwrap();

        assert_eq!(
            mgr.get_status("worker-1").await.unwrap(),
            TeammateStatus::Idle
        );

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

        mgr.set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working)
            .await
            .unwrap();

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
        let mgr = TeammateManager::new(
            "t1".into(),
            &agents,
            mailbox.clone(),
            task_board.clone(),
            broadcaster,
        );

        task_board
            .create_task("t1", "Task A", None, None, &[])
            .await
            .unwrap();

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
        let mgr = TeammateManager::new(
            "t1".into(),
            &agents,
            mailbox.clone(),
            task_board,
            broadcaster,
        );

        mgr.set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();
        mgr.mark_idle("worker-1", Some("sub-task done"))
            .await
            .unwrap();

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
        let mgr = TeammateManager::new(
            "t1".into(),
            &agents,
            mailbox.clone(),
            task_board,
            broadcaster,
        );

        mgr.set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();
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
        let mgr = TeammateManager::new(
            "t1".into(),
            &agents,
            mailbox.clone(),
            task_board,
            broadcaster,
        );

        mgr.set_status("lead-1", TeammateStatus::Working)
            .await
            .unwrap();
        mgr.mark_idle("lead-1", Some("done")).await.unwrap();

        let lead_msgs = mailbox.read_unread("t1", "lead-1").await.unwrap();
        assert!(lead_msgs.is_empty());
    }

    #[tokio::test]
    async fn mark_idle_broadcasts_status_event() {
        let agents = make_team_agents();
        let (mgr, bc) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();
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

        mgr.set_status("worker-1", TeammateStatus::Completed)
            .await
            .unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working)
            .await
            .unwrap();

        let result = mgr.mark_idle("worker-2", None).await.unwrap();
        assert_eq!(result.as_deref(), Some("lead-1"));
    }

    #[tokio::test]
    async fn all_teammates_settled_with_error_wakes_leader() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Error)
            .await
            .unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working)
            .await
            .unwrap();

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

        mgr.set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working)
            .await
            .unwrap();

        let result = mgr.mark_idle("worker-1", None).await.unwrap();
        assert!(result.is_none(), "worker-2 still Working blocks wake");
    }

    #[tokio::test]
    async fn thinking_teammate_blocks_leader_wake() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::Thinking)
            .await
            .unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working)
            .await
            .unwrap();

        let result = mgr.mark_idle("worker-2", None).await.unwrap();
        assert!(result.is_none(), "Thinking is not settled");
    }

    #[tokio::test]
    async fn tool_use_teammate_blocks_leader_wake() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        mgr.set_status("worker-1", TeammateStatus::ToolUse)
            .await
            .unwrap();
        mgr.set_status("worker-2", TeammateStatus::Working)
            .await
            .unwrap();

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
        assert!(
            mgr.acquire_wake_lock("worker-1"),
            "lock is reusable after release"
        );

        mgr.release_wake_lock("worker-1");
        mgr.release_wake_lock("worker-1"); // double release is a no-op
    }

    #[tokio::test]
    async fn wake_lock_is_scoped_per_slot() {
        let agents = make_team_agents();
        let (mgr, _) = make_manager(&agents);

        assert!(mgr.acquire_wake_lock("worker-1"));
        assert!(
            mgr.acquire_wake_lock("worker-2"),
            "different slot must not be blocked"
        );
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
            handles.push(tokio::spawn(
                async move { mgr.acquire_wake_lock("worker-1") },
            ));
        }

        let mut winners = 0usize;
        for h in handles {
            if h.await.unwrap() {
                winners += 1;
            }
        }
        assert_eq!(
            winners, 1,
            "exactly one concurrent acquire should win the lock"
        );
    }
}
