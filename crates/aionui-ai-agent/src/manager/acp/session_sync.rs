//! Per-session persistence consumer driven by domain events.
//!
//! Subscribes to `mpsc::Receiver<AcpSessionEvent>` (not the UI broadcast)
//! and writes user *intent* changes to `acp_session.session_config.runtime`.
//! This eliminates the previous anti-pattern of reverse-engineering desired
//! state from CLI observation events (`currentModeId` in `AcpModeInfo`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aionui_db::{IAcpSessionRepository, SaveRuntimeStateParams};
use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;
use tokio::time::sleep_until;
use tracing::{debug, warn};

use crate::acp_agent::AcpAgentManager;
use crate::agent_manager::AgentManagerHandle;
use crate::manager::acp::PersistedSessionState as SnapshotPersistedState;
use crate::manager::acp::events::AcpSessionEvent;
use crate::stream_event::AgentStreamEvent;

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(500);

/// Global service that loads and persists ACP per-session runtime
/// state on behalf of the conversation route. One instance per
/// process, held by `AppServices`.
pub struct AcpSessionSyncService {
    repo: Arc<dyn IAcpSessionRepository>,
    active: RwLock<HashMap<String, JoinHandle<()>>>,
}

impl AcpSessionSyncService {
    pub fn new(repo: Arc<dyn IAcpSessionRepository>) -> Arc<Self> {
        Arc::new(Self {
            repo,
            active: RwLock::new(HashMap::new()),
        })
    }

    /// Read the persisted per-session state for `conversation_id`.
    pub async fn load_persisted(&self, conversation_id: &str) -> Option<aionui_db::PersistedSessionState> {
        match self.repo.load_runtime_state(conversation_id).await {
            Ok(state) => state,
            Err(err) => {
                warn!(
                    conversation_id,
                    error = %err,
                    "AcpSessionSyncService::load_persisted failed"
                );
                None
            }
        }
    }

    /// Wire a newly-constructed manager into the service:
    ///
    /// 1. Load persisted runtime state and seed the manager's session.
    /// 2. Spawn a per-conversation consumer task that subscribes to the
    ///    manager's domain event channel and writes intent changes to DB.
    pub async fn attach(
        &self,
        conversation_id: String,
        handle: AgentManagerHandle,
        domain_rx: mpsc::Receiver<AcpSessionEvent>,
    ) {
        if let Some(acp) = handle.as_any().downcast_ref::<AcpAgentManager>() {
            if let Some(state) = self.load_snapshot_state(&conversation_id).await {
                acp.preload_snapshot(state).await;
            }
            if let Ok(Some(row)) = self.repo.get(&conversation_id).await
                && let Some(sid) = row.session_id
            {
                acp.restore_session_id(sid).await;
            }
        }

        // Spawn domain-event consumer (replaces broadcast-based consumer).
        let repo = self.repo.clone();
        let cid = conversation_id.clone();
        let task = tokio::spawn(domain_event_consumer(cid, domain_rx, repo));

        // Also spawn broadcast consumer for session_id assignment which
        // fires through the existing AgentStreamEvent::SessionAssigned path.
        let rx = handle.subscribe();
        let repo2 = self.repo.clone();
        let cid2 = conversation_id.clone();
        tokio::spawn(session_id_consumer(cid2, rx, repo2));

        let mut guard = self.active.write().await;
        if let Some(prev) = guard.insert(conversation_id, task) {
            prev.abort();
        }
    }

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

/// Pending DB update fields accumulated from domain events.
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

    fn merge_from_domain_event(&mut self, event: &AcpSessionEvent) -> bool {
        match event {
            AcpSessionEvent::DesiredModeChanged { mode_id } => {
                self.current_mode_id = Some(Some(mode_id.as_str().to_owned()));
                true
            }
            AcpSessionEvent::DesiredConfigChanged { selections } => {
                let string_map: HashMap<String, String> = selections
                    .iter()
                    .map(|(k, v)| (k.as_str().to_owned(), v.as_str().to_owned()))
                    .collect();
                let json = serde_json::to_string(&string_map).unwrap_or_default();
                self.config_selections_json = Some(Some(json));
                true
            }
            AcpSessionEvent::ObservedModelSynced { model_id } => {
                self.current_model_id = Some(Some(model_id.as_str().to_owned()));
                true
            }
            _ => false,
        }
    }
}

/// Consume domain events from the session aggregate and persist user
/// intent changes with a debounce window.
async fn domain_event_consumer(
    conversation_id: String,
    mut rx: mpsc::Receiver<AcpSessionEvent>,
    repo: Arc<dyn IAcpSessionRepository>,
) {
    let mut pending = PendingUpdate::default();
    let mut flush_at: Option<Instant> = None;

    loop {
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
            Some(event) => {
                if pending.merge_from_domain_event(&event) {
                    flush_at = Some(Instant::now() + DEBOUNCE_WINDOW);
                }
            }
            None => {
                flush(&repo, &conversation_id, &mut pending).await;
                debug!(conversation_id, "session-sync domain consumer exiting");
                return;
            }
        }
    }
}

/// Lightweight consumer that only handles session_id assignment from the
/// broadcast stream. This event fires exactly once per conversation when
/// the CLI responds to `session/new`.
async fn session_id_consumer(
    conversation_id: String,
    mut rx: tokio::sync::broadcast::Receiver<AgentStreamEvent>,
    repo: Arc<dyn IAcpSessionRepository>,
) {
    loop {
        match rx.recv().await {
            Ok(AgentStreamEvent::SessionAssigned(data)) => {
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
            Ok(_) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
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
    use aionui_db::{CreateAcpSessionParams, SqliteAcpSessionRepository, init_database_memory};
    use tokio::time::sleep;

    async fn setup() -> (Arc<AcpSessionSyncService>, Arc<dyn IAcpSessionRepository>) {
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
        let svc = AcpSessionSyncService::new(repo.clone());
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

    /// Domain event DesiredModeChanged flushes after debounce.
    #[tokio::test(flavor = "current_thread")]
    async fn domain_event_flushes_after_debounce() {
        let (_svc, repo) = setup().await;
        let (tx, rx) = mpsc::channel(64);

        let cid = "conv-1".to_owned();
        tokio::spawn(domain_event_consumer(cid, rx, repo.clone()));

        tx.send(AcpSessionEvent::DesiredModeChanged { mode_id: "plan".into() })
            .await
            .unwrap();

        sleep(Duration::from_millis(200)).await;
        let state = repo.load_runtime_state("conv-1").await.unwrap().unwrap();
        assert!(state.current_mode_id.is_none(), "debounce not yet elapsed");

        sleep(Duration::from_millis(400)).await;
        let state = repo.load_runtime_state("conv-1").await.unwrap().unwrap();
        assert_eq!(state.current_mode_id.as_deref(), Some("plan"));
    }

    /// Burst of events coalesces into a single write.
    #[tokio::test(flavor = "current_thread")]
    async fn coalesces_burst_into_single_write() {
        let (_svc, repo) = setup().await;
        let (tx, rx) = mpsc::channel(64);

        let cid = "conv-1".to_owned();
        tokio::spawn(domain_event_consumer(cid, rx, repo.clone()));

        for label in ["code", "plan", "ask"] {
            tx.send(AcpSessionEvent::DesiredModeChanged { mode_id: label.into() })
                .await
                .unwrap();
            sleep(Duration::from_millis(100)).await;
        }
        sleep(Duration::from_millis(600)).await;

        let state = repo.load_runtime_state("conv-1").await.unwrap().unwrap();
        assert_eq!(state.current_mode_id.as_deref(), Some("ask"));
    }

    /// Unrelated events (SessionOpened) never trigger a DB write.
    #[tokio::test(flavor = "current_thread")]
    async fn unrelated_events_are_ignored() {
        let (_svc, repo) = setup().await;
        let (tx, rx) = mpsc::channel(64);

        let cid = "conv-1".to_owned();
        tokio::spawn(domain_event_consumer(cid, rx, repo.clone()));

        tx.send(AcpSessionEvent::SessionOpened).await.unwrap();
        sleep(Duration::from_millis(600)).await;

        let state = repo.load_runtime_state("conv-1").await.unwrap().unwrap();
        assert!(state.current_mode_id.is_none());
    }

    /// When the sender drops, consumer flushes and exits.
    #[tokio::test(flavor = "current_thread")]
    async fn flushes_and_exits_on_channel_close() {
        let (_svc, repo) = setup().await;
        let (tx, rx) = mpsc::channel(64);

        let cid = "conv-1".to_owned();
        tokio::spawn(domain_event_consumer(cid, rx, repo.clone()));

        tx.send(AcpSessionEvent::DesiredModeChanged { mode_id: "plan".into() })
            .await
            .unwrap();
        drop(tx);
        sleep(Duration::from_millis(50)).await;

        let state = repo.load_runtime_state("conv-1").await.unwrap().unwrap();
        assert_eq!(
            state.current_mode_id.as_deref(),
            Some("plan"),
            "pending update must flush on channel close"
        );
    }
}
