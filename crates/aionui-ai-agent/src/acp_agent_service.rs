//! Facade for ACP-specific per-session state.
//!
//! Reads and writes live in the service; `AcpAgentManager` is a pure
//! producer that exposes `subscribe()` and `preload_snapshot(...)`. On
//! every new `AcpAgentManager`, the service subscribes to its broadcast
//! and spawns a per-conversation consumer task that:
//!
//! - filters the events that carry persistable user choices
//!   (`AcpModeInfo` / `AcpModelInfo` / `AcpConfigOption` /
//!   `AcpContextUsage`);
//! - merges them inside a 500 ms debounce window — `AcpContextUsage`
//!   can fire many times per LLM turn, and coalescing avoids a DB
//!   write for every tick;
//! - commits each coalesced partial to
//!   `acp_session.session_config.runtime` through `IAcpSessionRepository`.
//!
//! When the manager is dropped or killed, its broadcast channel closes,
//! `recv()` returns `Closed`, the consumer flushes and exits. No
//! explicit detach needed from callers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aionui_db::{IAcpSessionRepository, PersistedSessionState, SaveRuntimeStateParams};
use serde_json::Value;
use tokio::sync::{RwLock, broadcast};
use tokio::task::JoinHandle;
use tokio::time::sleep_until;
use tracing::{debug, warn};

use crate::acp_agent::AcpAgentManager;
use crate::acp_runtime_snapshot::PersistedSessionState as SnapshotPersistedState;
use crate::agent_manager::AgentManagerHandle;
use crate::stream_event::AgentStreamEvent;

/// Debounce window per conversation. Short enough to feel live, long
/// enough to coalesce a typical LLM turn's stream of `AcpContextUsage`
/// events into a single DB write.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(500);

/// Fields a consumer task may accumulate before flushing to DB.
///
/// Each field is `Option<Option<String>>`:
/// - Outer `None` — this flush does not touch the field.
/// - `Some(None)` — clear the field in the DB row.
/// - `Some(Some(..))` — set the field to this value.
#[derive(Debug, Clone, Default)]
struct PendingUpdate {
    current_mode_id: Option<Option<String>>,
    current_model_id: Option<Option<String>>,
    config_selections_json: Option<Option<String>>,
    context_usage_json: Option<Option<String>>,
}

impl PendingUpdate {
    fn is_empty(&self) -> bool {
        self.current_mode_id.is_none()
            && self.current_model_id.is_none()
            && self.config_selections_json.is_none()
            && self.context_usage_json.is_none()
    }

    fn as_save_params(&self) -> SaveRuntimeStateParams<'_> {
        SaveRuntimeStateParams {
            current_mode_id: self.current_mode_id.as_ref().map(Option::as_deref),
            current_model_id: self.current_model_id.as_ref().map(Option::as_deref),
            config_selections_json: self.config_selections_json.as_ref().map(Option::as_deref),
            context_usage_json: self.context_usage_json.as_ref().map(Option::as_deref),
        }
    }

    /// Project a session event onto this accumulator. Returns `true`
    /// when the event actually contributed a field; unrelated events
    /// are silently ignored.
    fn merge_from_event(&mut self, event: &AgentStreamEvent) -> bool {
        match event {
            AgentStreamEvent::AcpModeInfo(v) => {
                if let Some(id) = v
                    .get("currentModeId")
                    .or_else(|| v.get("current_mode_id"))
                    .and_then(Value::as_str)
                {
                    self.current_mode_id = Some(Some(id.to_owned()));
                    return true;
                }
                false
            }
            AgentStreamEvent::AcpModelInfo(v) => {
                if let Some(id) = v
                    .get("currentModelId")
                    .or_else(|| v.get("current_model_id"))
                    .and_then(Value::as_str)
                {
                    self.current_model_id = Some(Some(id.to_owned()));
                    return true;
                }
                false
            }
            AgentStreamEvent::AcpConfigOption(v) => {
                // Event shape matches the SDK `ConfigOptionUpdate`:
                //   { config_options: [ { id, kind: { current_value, ... }, ... } ] }
                // We project each option to `id -> current_value` and
                // persist that as the user's selection map. `current_value`
                // lives inside the flattened `kind` payload (Select /
                // Boolean / …), so read the top-level key directly.
                let items = v
                    .get("config_options")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut selections = serde_json::Map::new();
                for item in &items {
                    let Some(cid) = item.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    // Only string-valued Select options are persisted as
                    // user selections — Boolean / future kinds don't map
                    // cleanly to the HashMap<String, String> that
                    // `acp_session.session_config.runtime.config_selections`
                    // expects. Missing `current_value` keys are skipped.
                    let Some(current) = item.get("current_value").and_then(Value::as_str) else {
                        continue;
                    };
                    selections.insert(cid.to_owned(), Value::String(current.to_owned()));
                }
                if selections.is_empty() {
                    return false;
                }
                self.config_selections_json = Some(Some(Value::Object(selections).to_string()));
                true
            }
            AgentStreamEvent::AcpContextUsage(v) => {
                self.context_usage_json = Some(Some(v.to_string()));
                true
            }
            _ => false,
        }
    }
}

/// Global service that loads and persists ACP per-session runtime
/// state on behalf of the conversation route. One instance per
/// process, held by `AppServices`.
pub struct AcpAgentService {
    repo: Arc<dyn IAcpSessionRepository>,
    /// JoinHandles of consumer tasks, keyed by conversation_id. Stored
    /// so the service can be dropped cleanly at shutdown — tasks exit
    /// on their own when the manager's broadcast closes, so we only
    /// abort here if shutdown outruns natural termination.
    active: RwLock<HashMap<String, JoinHandle<()>>>,
}

impl AcpAgentService {
    pub fn new(repo: Arc<dyn IAcpSessionRepository>) -> Arc<Self> {
        Arc::new(Self {
            repo,
            active: RwLock::new(HashMap::new()),
        })
    }

    /// Read the persisted per-session state for `conversation_id`.
    /// Called by the conversation route before resuming an ACP session
    /// so the manager can preload its snapshot.
    pub async fn load_persisted(&self, conversation_id: &str) -> Option<PersistedSessionState> {
        match self.repo.load_runtime_state(conversation_id).await {
            Ok(state) => state,
            Err(err) => {
                warn!(
                    conversation_id,
                    error = %err,
                    "AcpAgentService::load_persisted failed"
                );
                None
            }
        }
    }

    /// Wire a newly-constructed manager into the service:
    ///
    /// 1. Load persisted runtime state from `acp_session` and seed the
    ///    manager's snapshot (so resume paths have `current_mode_id` /
    ///    `current_model_id` / `context_usage` ready before the CLI
    ///    `session/load` reply arrives).
    /// 2. Subscribe to the manager's event broadcast and spawn a
    ///    per-conversation consumer task that writes subsequent
    ///    choices back to the DB with a 500 ms debounce.
    ///
    /// Only `AcpAgentManager` handles are preloaded — other agent
    /// types do not persist runtime state. The subscription still runs
    /// for everyone but the consumer ignores unrelated events.
    ///
    /// If a previous task for this `conversation_id` is still running
    /// (e.g. the manager was rebuilt after a crash), it is aborted
    /// before being replaced so we do not double-write.
    pub async fn attach(&self, conversation_id: String, handle: AgentManagerHandle) {
        if let Some(acp) = handle.as_any().downcast_ref::<AcpAgentManager>() {
            // Restore runtime snapshot (modes, models, config selections)
            if let Some(state) = self.load_snapshot_state(&conversation_id).await {
                acp.preload_snapshot(state).await;
            }
            // Restore session_id from DB so resume path works after restart
            if let Ok(Some(row)) = self.repo.get(&conversation_id).await
                && let Some(sid) = row.session_id
            {
                acp.restore_session_id(sid).await;
            }
        }

        let rx = handle.subscribe();
        let repo = self.repo.clone();
        let cid = conversation_id.clone();
        let task = tokio::spawn(per_conversation_consumer(cid, rx, repo));

        let mut guard = self.active.write().await;
        if let Some(prev) = guard.insert(conversation_id, task) {
            // Defensive: the broadcast from the old manager is already
            // dead, but the task may still be in its debounce sleep.
            prev.abort();
        }
    }

    /// Load and decode the `session_config.runtime` payload into the
    /// shape `AcpRuntimeSnapshot::preload_persisted` expects. Errors
    /// and malformed JSON are downgraded to `None` — preload is
    /// best-effort; the CLI's `session/load` reply still refills the
    /// snapshot with its own values.
    async fn load_snapshot_state(&self, conversation_id: &str) -> Option<SnapshotPersistedState> {
        let row = match self.repo.load_runtime_state(conversation_id).await {
            Ok(Some(row)) => row,
            Ok(None) => return None,
            Err(err) => {
                warn!(
                    conversation_id,
                    error = %err,
                    "load_snapshot_state: repository failed; skipping preload"
                );
                return None;
            }
        };

        let mut state = SnapshotPersistedState {
            current_mode_id: row.current_mode_id,
            current_model_id: row.current_model_id,
            ..Default::default()
        };
        if let Some(raw) = row.config_selections_json
            && let Ok(map) = serde_json::from_str(&raw)
        {
            state.config_selections = map;
        }
        if let Some(raw) = row.context_usage_json
            && let Ok(usage) = serde_json::from_str(&raw)
        {
            state.context_usage = Some(usage);
        }
        Some(state)
    }
}

/// Drain one manager's event stream into the `acp_session` row.
///
/// Exits when the broadcast closes (manager dropped / killed) or the
/// task is aborted. Any pending update is flushed on exit — "Finish"
/// events end turns, not tasks.
async fn per_conversation_consumer(
    conversation_id: String,
    mut rx: broadcast::Receiver<AgentStreamEvent>,
    repo: Arc<dyn IAcpSessionRepository>,
) {
    let mut pending = PendingUpdate::default();
    let mut flush_at: Option<Instant> = None;

    loop {
        // Pick the earliest pending deadline (if any) — it governs how
        // long we may block waiting for more events before flushing.
        let recv = match flush_at {
            Some(deadline) => {
                tokio::select! {
                    biased;
                    maybe_event = rx.recv() => maybe_event,
                    () = sleep_until(deadline.into()) => {
                        flush(&repo, &conversation_id, &mut pending).await;
                        flush_at = None;
                        continue;
                    }
                }
            }
            None => rx.recv().await,
        };

        match recv {
            Ok(event) => {
                // Persist the CLI-assigned session id immediately — this
                // event fires exactly once per conversation and the id
                // is needed for later resume-via-`session/load`. No
                // debounce here; the cost is a single write.
                if let AgentStreamEvent::SessionAssigned(data) = &event {
                    match repo.update_session_id(&conversation_id, &data.session_id).await {
                        Ok(true) => {}
                        Ok(false) => {
                            debug!(
                                conversation_id,
                                "session-sync: acp_session row missing; session_id not written"
                            );
                        }
                        Err(err) => {
                            warn!(
                                conversation_id,
                                error = %err,
                                "session-sync: update_session_id failed"
                            );
                        }
                    }
                }
                if pending.merge_from_event(&event) {
                    flush_at = Some(Instant::now() + DEBOUNCE_WINDOW);
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                // Lost some events — not fatal, we'll catch up on the
                // next one. Persisted state is best-effort.
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                flush(&repo, &conversation_id, &mut pending).await;
                debug!(conversation_id, "session-sync consumer exiting");
                return;
            }
        }
    }
}

async fn flush(repo: &Arc<dyn IAcpSessionRepository>, conversation_id: &str, pending: &mut PendingUpdate) {
    if pending.is_empty() {
        return;
    }
    let params = pending.as_save_params();
    match repo.save_runtime_state(conversation_id, &params).await {
        Ok(true) => {}
        Ok(false) => {
            debug!(conversation_id, "session sync: acp_session row missing; update dropped");
        }
        Err(err) => {
            warn!(
                conversation_id,
                error = %err,
                "session sync: save_runtime_state failed"
            );
        }
    }
    *pending = PendingUpdate::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_manager::{AgentManagerHandle, IAgentManager};
    use crate::stream_event::FinishEventData;
    use aionui_common::{AgentKillReason, AgentType, AppError, Confirmation, ConversationStatus, TimestampMs};
    use aionui_db::{CreateAcpSessionParams, SqliteAcpSessionRepository, init_database_memory};
    use serde_json::json;
    use tokio::sync::broadcast;
    use tokio::time::sleep;

    /// Bare-bones manager exposing only `subscribe()` and `agent_type()`.
    /// Production managers have the same public surface; this stub lets
    /// us push synthetic events into the consumer without spawning a
    /// real CLI process.
    struct StubManager {
        tx: broadcast::Sender<AgentStreamEvent>,
    }

    impl StubManager {
        fn new() -> (Arc<Self>, broadcast::Sender<AgentStreamEvent>) {
            let (tx, _) = broadcast::channel(64);
            (Arc::new(Self { tx: tx.clone() }), tx)
        }
    }

    #[async_trait::async_trait]
    impl IAgentManager for StubManager {
        fn agent_type(&self) -> AgentType {
            AgentType::Acp
        }
        fn status(&self) -> Option<ConversationStatus> {
            None
        }
        fn workspace(&self) -> &str {
            ""
        }
        fn conversation_id(&self) -> &str {
            ""
        }
        fn last_activity_at(&self) -> TimestampMs {
            0
        }
        fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
            self.tx.subscribe()
        }
        async fn send_message(&self, _: crate::types::SendMessageData) -> Result<(), AppError> {
            Ok(())
        }
        async fn stop(&self) -> Result<(), AppError> {
            Ok(())
        }
        fn confirm(&self, _: &str, _: &str, _: serde_json::Value, _: bool) -> Result<(), AppError> {
            Ok(())
        }
        fn get_confirmations(&self) -> Vec<Confirmation> {
            Vec::new()
        }
        fn check_approval(&self, _: &str, _: Option<&str>) -> bool {
            false
        }
        fn kill(&self, _: Option<AgentKillReason>) -> Result<(), AppError> {
            Ok(())
        }
        async fn get_mode(&self) -> Result<aionui_api_types::AgentModeResponse, AppError> {
            Err(AppError::Internal("stub".into()))
        }
        async fn set_mode(&self, _: &str) -> Result<(), AppError> {
            Ok(())
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    async fn setup() -> (Arc<AcpAgentService>, Arc<dyn IAcpSessionRepository>) {
        let db = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAcpSessionRepository> = Arc::new(SqliteAcpSessionRepository::new(db.pool().clone()));
        repo.create(&CreateAcpSessionParams {
            conversation_id: "conv-1",
            agent_backend: "claude",
            agent_source: "builtin",
            agent_id: "2d23ff1c",
        })
        .await
        .unwrap();
        let svc = AcpAgentService::new(repo.clone());
        (svc, repo)
    }

    #[tokio::test]
    async fn load_persisted_round_trips() {
        let (svc, repo) = setup().await;
        repo.save_runtime_state(
            "conv-1",
            &SaveRuntimeStateParams {
                current_mode_id: Some(Some("plan")),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let state = svc.load_persisted("conv-1").await.unwrap();
        assert_eq!(state.current_mode_id.as_deref(), Some("plan"));
    }

    /// A single event is flushed after the debounce window has
    /// elapsed, not immediately.
    #[tokio::test(flavor = "current_thread")]
    async fn flushes_after_debounce_window() {
        let (svc, repo) = setup().await;
        let (mgr, tx) = StubManager::new();
        svc.attach("conv-1".into(), mgr as AgentManagerHandle).await;

        let _ = tx.send(AgentStreamEvent::AcpModeInfo(json!({
            "currentModeId": "code"
        })));

        sleep(Duration::from_millis(200)).await;
        let state = repo.load_runtime_state("conv-1").await.unwrap().unwrap();
        assert!(state.current_mode_id.is_none(), "debounce not yet elapsed");

        sleep(Duration::from_millis(400)).await;
        let state = repo.load_runtime_state("conv-1").await.unwrap().unwrap();
        assert_eq!(state.current_mode_id.as_deref(), Some("code"));
    }

    /// A burst of events on the same conversation coalesces into a
    /// single write carrying only the latest value per field.
    #[tokio::test(flavor = "current_thread")]
    async fn coalesces_burst_into_single_write() {
        let (svc, repo) = setup().await;
        let (mgr, tx) = StubManager::new();
        svc.attach("conv-1".into(), mgr as AgentManagerHandle).await;

        for label in ["code", "plan", "ask"] {
            let _ = tx.send(AgentStreamEvent::AcpModeInfo(json!({
                "currentModeId": label
            })));
            sleep(Duration::from_millis(100)).await;
        }
        // Window resets on every event, so 600 ms past the last send
        // is enough to catch the flush.
        sleep(Duration::from_millis(600)).await;

        let state = repo.load_runtime_state("conv-1").await.unwrap().unwrap();
        assert_eq!(state.current_mode_id.as_deref(), Some("ask"));
    }

    /// Unrelated events (Finish, etc.) never touch the debounce state
    /// and never trigger a DB write.
    #[tokio::test(flavor = "current_thread")]
    async fn unrelated_events_are_ignored() {
        let (svc, repo) = setup().await;
        let (mgr, tx) = StubManager::new();
        svc.attach("conv-1".into(), mgr as AgentManagerHandle).await;

        let _ = tx.send(AgentStreamEvent::Finish(FinishEventData { session_id: None }));
        sleep(Duration::from_millis(600)).await;

        let state = repo.load_runtime_state("conv-1").await.unwrap().unwrap();
        assert!(state.current_mode_id.is_none());
    }

    /// When the manager drops (sender dropped), the consumer flushes
    /// pending work and exits cleanly.
    #[tokio::test(flavor = "current_thread")]
    async fn flushes_and_exits_on_broadcast_close() {
        let (svc, repo) = setup().await;
        let (mgr, tx) = StubManager::new();
        svc.attach("conv-1".into(), mgr as AgentManagerHandle).await;

        let _ = tx.send(AgentStreamEvent::AcpModeInfo(json!({
            "currentModeId": "plan"
        })));
        // Drop both sender and manager → broadcast closes.
        drop(tx);
        sleep(Duration::from_millis(50)).await;

        let state = repo.load_runtime_state("conv-1").await.unwrap().unwrap();
        assert_eq!(
            state.current_mode_id.as_deref(),
            Some("plan"),
            "pending update must flush on broadcast close"
        );
    }
}
