use std::sync::Arc;

use aionui_common::{AgentKillReason, AgentType, AppError, ConversationStatus, TimestampMs, now_ms};
use async_trait::async_trait;
use dashmap::DashMap;
use futures_util::future::BoxFuture;
use tokio::sync::OnceCell;
use tracing::info;

use crate::agent_manager::AgentManagerHandle;
use crate::types::BuildTaskOptions;

/// Factory function that creates an [`AgentManagerHandle`] from build options.
///
/// Async so the factory can do real I/O (spawn a CLI process, negotiate the
/// ACP initialize handshake, etc.) without needing to `block_on` inside the
/// `IWorkerTaskManager` call site. Returning `BoxFuture` keeps the trait
/// object-safe for DI.
pub type AgentFactory =
    Arc<dyn Fn(BuildTaskOptions) -> BoxFuture<'static, Result<AgentManagerHandle, AppError>> + Send + Sync>;

/// Manages the lifecycle of active Agent tasks.
///
/// Each conversation has at most one active task (keyed by conversation ID).
/// The trait is object-safe for dependency injection.
#[async_trait]
pub trait IWorkerTaskManager: Send + Sync {
    /// Get an existing task by conversation ID.
    fn get_task(&self, conversation_id: &str) -> Option<AgentManagerHandle>;

    /// Get an existing task or build a new one if none exists.
    ///
    /// Concurrent callers with the same `conversation_id` block on a shared
    /// [`OnceCell`] so the factory runs at most once per conversation —
    /// avoiding the race where two concurrent HTTP requests (e.g.
    /// `/messages` + `/warmup`) would each spawn their own CLI process and
    /// ACP connection, with one of them leaking.
    async fn get_or_build_task(
        &self,
        conversation_id: &str,
        options: BuildTaskOptions,
    ) -> Result<AgentManagerHandle, AppError>;

    /// Kill and remove a task.
    fn kill(&self, conversation_id: &str, reason: Option<AgentKillReason>) -> Result<(), AppError>;

    /// Kill and remove all active tasks.
    fn clear(&self);

    /// Number of active tasks (useful for diagnostics).
    fn active_count(&self) -> usize;

    /// Collect tasks eligible for idle cleanup.
    ///
    /// Returns conversation IDs of tasks that:
    /// - have `status == Some(Finished)`
    /// - have been idle longer than `idle_threshold_ms`
    fn collect_idle(&self, idle_threshold_ms: TimestampMs) -> Vec<String>;
}

/// Per-conversation slot: an [`OnceCell`] that the first concurrent caller
/// initialises by running the factory, and that every subsequent caller
/// awaits. Failed initialisations leave the cell empty so the next caller
/// may retry; the slot itself is only removed on `kill` / `clear`.
type TaskSlot = Arc<OnceCell<AgentManagerHandle>>;

/// Default implementation of [`IWorkerTaskManager`] using a concurrent hash map.
pub struct WorkerTaskManagerImpl {
    tasks: DashMap<String, TaskSlot>,
    factory: AgentFactory,
}

impl WorkerTaskManagerImpl {
    pub fn new(factory: AgentFactory) -> Self {
        Self {
            tasks: DashMap::new(),
            factory,
        }
    }

    /// Look up a fully-initialised handle by conversation id.
    fn initialised_handle(&self, conversation_id: &str) -> Option<AgentManagerHandle> {
        self.tasks.get(conversation_id).and_then(|slot| slot.get().cloned())
    }
}

#[async_trait]
impl IWorkerTaskManager for WorkerTaskManagerImpl {
    fn get_task(&self, conversation_id: &str) -> Option<AgentManagerHandle> {
        self.initialised_handle(conversation_id)
    }

    async fn get_or_build_task(
        &self,
        conversation_id: &str,
        options: BuildTaskOptions,
    ) -> Result<AgentManagerHandle, AppError> {
        // Atomically obtain the per-conversation slot. `DashMap::entry` is
        // synchronous and side-effect-free — only an empty OnceCell is
        // allocated on the miss path, so concurrent callers for the same id
        // all end up holding the same `Arc<OnceCell>`.
        let slot: TaskSlot = self
            .tasks
            .entry(conversation_id.to_owned())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();

        // `OnceCell::get_or_try_init` serialises concurrent initialisers:
        // the first caller to reach it runs the factory, every other caller
        // awaits the same future and ends up with the same handle. On
        // failure the cell stays empty so a later caller can retry.
        let factory = self.factory.clone();
        let handle = slot.get_or_try_init(|| async move { factory(options).await }).await?;
        Ok(Arc::clone(handle))
    }

    fn kill(&self, conversation_id: &str, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        if let Some((id, slot)) = self.tasks.remove(conversation_id) {
            info!(conversation_id = %id, ?reason, "Killing agent task");
            if let Some(agent) = slot.get() {
                agent.kill(reason)?;
            }
        }
        Ok(())
    }

    fn clear(&self) {
        let keys: Vec<String> = self.tasks.iter().map(|r| r.key().clone()).collect();
        for key in keys {
            if let Some((id, slot)) = self.tasks.remove(&key) {
                info!(conversation_id = %id, "Clearing agent task");
                if let Some(agent) = slot.get() {
                    let _ = agent.kill(None);
                }
            }
        }
    }

    fn active_count(&self) -> usize {
        self.tasks.iter().filter(|entry| entry.value().get().is_some()).count()
    }

    fn collect_idle(&self, idle_threshold_ms: TimestampMs) -> Vec<String> {
        let now = now_ms();
        self.tasks
            .iter()
            .filter_map(|entry| {
                let agent = entry.value().get()?;
                // Only ACP agents participate in idle cleanup per API Spec
                (agent.agent_type() == AgentType::Acp
                    && agent.status() == Some(ConversationStatus::Finished)
                    && (now - agent.last_activity_at()) > idle_threshold_ms)
                    .then(|| entry.key().clone())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_manager::IAgentManager;
    use crate::stream_event::AgentStreamEvent;
    use crate::types::SendMessageData;
    use aionui_common::{AgentKillReason, AgentType, Confirmation, ConversationStatus, ProviderWithModel};
    use futures_util::FutureExt;
    use std::sync::atomic::{AtomicI64, Ordering};
    use tokio::sync::broadcast;

    /// A minimal mock agent for testing task manager logic.
    struct MockAgent {
        agent_type: AgentType,
        conversation_id: String,
        workspace: String,
        status: Option<ConversationStatus>,
        last_activity: AtomicI64,
        event_tx: broadcast::Sender<AgentStreamEvent>,
    }

    impl MockAgent {
        fn new(conversation_id: &str, status: Option<ConversationStatus>) -> Self {
            let (event_tx, _) = broadcast::channel(16);
            Self {
                agent_type: AgentType::Acp,
                conversation_id: conversation_id.to_owned(),
                workspace: "/tmp/test".to_owned(),
                status,
                last_activity: AtomicI64::new(now_ms()),
                event_tx,
            }
        }

        fn with_agent_type(mut self, t: AgentType) -> Self {
            self.agent_type = t;
            self
        }

        fn with_last_activity(mut self, ts: TimestampMs) -> Self {
            self.last_activity = AtomicI64::new(ts);
            self
        }
    }

    #[async_trait::async_trait]
    impl IAgentManager for MockAgent {
        fn agent_type(&self) -> AgentType {
            self.agent_type
        }
        fn status(&self) -> Option<ConversationStatus> {
            self.status
        }
        fn workspace(&self) -> &str {
            &self.workspace
        }
        fn conversation_id(&self) -> &str {
            &self.conversation_id
        }
        fn last_activity_at(&self) -> TimestampMs {
            self.last_activity.load(Ordering::Relaxed)
        }
        fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
            self.event_tx.subscribe()
        }
        async fn send_message(&self, _data: SendMessageData) -> Result<(), AppError> {
            Ok(())
        }
        async fn stop(&self) -> Result<(), AppError> {
            Ok(())
        }
        fn confirm(
            &self,
            _msg_id: &str,
            _call_id: &str,
            _data: serde_json::Value,
            _always_allow: bool,
        ) -> Result<(), AppError> {
            Ok(())
        }
        fn get_confirmations(&self) -> Vec<Confirmation> {
            vec![]
        }
        fn check_approval(&self, _action: &str, _command_type: Option<&str>) -> bool {
            false
        }
        fn kill(&self, _reason: Option<AgentKillReason>) -> Result<(), AppError> {
            Ok(())
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn make_options(conversation_id: &str) -> BuildTaskOptions {
        BuildTaskOptions {
            agent_type: AgentType::Acp,
            workspace: "/tmp/test".into(),
            model: ProviderWithModel {
                provider_id: "p1".into(),
                model: "test".into(),
                use_model: None,
            },
            conversation_id: conversation_id.into(),
            extra: serde_json::Value::Null,
        }
    }

    fn make_manager() -> WorkerTaskManagerImpl {
        let factory: AgentFactory = Arc::new(|opts: BuildTaskOptions| {
            async move { Ok(Arc::new(MockAgent::new(&opts.conversation_id, None)) as AgentManagerHandle) }.boxed()
        });
        WorkerTaskManagerImpl::new(factory)
    }

    #[test]
    fn get_task_returns_none_when_empty() {
        let mgr = make_manager();
        assert!(mgr.get_task("nonexistent").is_none());
    }

    #[tokio::test]
    async fn get_or_build_creates_task() {
        let mgr = make_manager();
        let handle = mgr.get_or_build_task("conv-1", make_options("conv-1")).await.unwrap();
        assert_eq!(handle.conversation_id(), "conv-1");
        assert_eq!(mgr.active_count(), 1);
    }

    #[tokio::test]
    async fn get_or_build_returns_existing() {
        let mgr = make_manager();
        let h1 = mgr.get_or_build_task("conv-1", make_options("conv-1")).await.unwrap();
        let h2 = mgr.get_or_build_task("conv-1", make_options("conv-1")).await.unwrap();
        assert!(Arc::ptr_eq(&h1, &h2));
        assert_eq!(mgr.active_count(), 1);
    }

    #[tokio::test]
    async fn get_or_build_is_single_flight_under_concurrency() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_factory = Arc::clone(&calls);
        let factory: AgentFactory = Arc::new(move |opts: BuildTaskOptions| {
            let calls = Arc::clone(&calls_for_factory);
            async move {
                // Simulate a slow build (CLI spawn + initialize handshake).
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(MockAgent::new(&opts.conversation_id, None)) as AgentManagerHandle)
            }
            .boxed()
        });
        let mgr = Arc::new(WorkerTaskManagerImpl::new(factory));

        // Ten concurrent callers all racing on the same conversation id.
        let mut joins = Vec::new();
        for _ in 0..10 {
            let mgr = Arc::clone(&mgr);
            joins.push(tokio::spawn(async move {
                mgr.get_or_build_task("conv-race", make_options("conv-race")).await
            }));
        }
        let handles: Vec<_> = futures_util::future::join_all(joins)
            .await
            .into_iter()
            .map(|r| r.unwrap().unwrap())
            .collect();

        assert_eq!(calls.load(Ordering::SeqCst), 1, "factory must run only once");
        assert_eq!(mgr.active_count(), 1);
        for h in handles.iter().skip(1) {
            assert!(Arc::ptr_eq(&handles[0], h), "all callers see the same handle");
        }
    }

    #[tokio::test]
    async fn get_or_build_retries_after_failure() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let fail_next = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&fail_next);
        let factory: AgentFactory = Arc::new(move |opts: BuildTaskOptions| {
            let flag = Arc::clone(&flag);
            async move {
                if flag.swap(false, Ordering::SeqCst) {
                    Err(AppError::Internal("first call fails".into()))
                } else {
                    Ok(Arc::new(MockAgent::new(&opts.conversation_id, None)) as AgentManagerHandle)
                }
            }
            .boxed()
        });
        let mgr = WorkerTaskManagerImpl::new(factory);

        // First call fails, slot stays empty.
        assert!(mgr.get_or_build_task("conv-1", make_options("conv-1")).await.is_err());
        // Second call retries and succeeds.
        let h = mgr.get_or_build_task("conv-1", make_options("conv-1")).await.unwrap();
        assert_eq!(h.conversation_id(), "conv-1");
        assert_eq!(mgr.active_count(), 1);
    }

    #[tokio::test]
    async fn get_task_finds_existing() {
        let mgr = make_manager();
        mgr.get_or_build_task("conv-1", make_options("conv-1")).await.unwrap();
        let handle = mgr.get_task("conv-1");
        assert!(handle.is_some());
        assert_eq!(handle.unwrap().conversation_id(), "conv-1");
    }

    #[tokio::test]
    async fn kill_removes_task() {
        let mgr = make_manager();
        mgr.get_or_build_task("conv-1", make_options("conv-1")).await.unwrap();
        assert_eq!(mgr.active_count(), 1);

        mgr.kill("conv-1", Some(AgentKillReason::IdleTimeout)).unwrap();
        assert_eq!(mgr.active_count(), 0);
        assert!(mgr.get_task("conv-1").is_none());
    }

    #[test]
    fn kill_nonexistent_is_ok() {
        let factory: AgentFactory = Arc::new(|_| async { unreachable!() }.boxed());
        let mgr = WorkerTaskManagerImpl::new(factory);
        assert!(mgr.kill("nothing", None).is_ok());
    }

    #[tokio::test]
    async fn clear_removes_all() {
        let mgr = make_manager();
        mgr.get_or_build_task("conv-1", make_options("conv-1")).await.unwrap();
        mgr.get_or_build_task("conv-2", make_options("conv-2")).await.unwrap();
        assert_eq!(mgr.active_count(), 2);

        mgr.clear();
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn collect_idle_finds_finished_and_stale_acp_tasks() {
        let factory: AgentFactory = Arc::new(|_| async { unreachable!() }.boxed());
        let mgr = WorkerTaskManagerImpl::new(factory);

        // Helper: insert a pre-initialised slot bypassing the async factory path.
        let insert = |id: &str, agent: Arc<dyn IAgentManager>| {
            let cell: OnceCell<AgentManagerHandle> = OnceCell::new();
            cell.set(agent).ok();
            mgr.tasks.insert(id.into(), Arc::new(cell));
        };

        // ACP + Finished + old activity → should be collected
        let stale = Arc::new(
            MockAgent::new("conv-stale", Some(ConversationStatus::Finished)).with_last_activity(now_ms() - 600_000), // 10 min ago
        );
        insert("conv-stale", stale);

        // ACP + Finished + recent activity → should NOT be collected
        let recent =
            Arc::new(MockAgent::new("conv-recent", Some(ConversationStatus::Finished)).with_last_activity(now_ms()));
        insert("conv-recent", recent);

        // ACP + Running + old activity → should NOT be collected
        let running = Arc::new(
            MockAgent::new("conv-running", Some(ConversationStatus::Running)).with_last_activity(now_ms() - 600_000),
        );
        insert("conv-running", running);

        // Non-ACP (Nanobot) + Finished + old activity → should NOT be collected
        let nanobot = Arc::new(
            MockAgent::new("conv-nanobot", Some(ConversationStatus::Finished))
                .with_agent_type(AgentType::Nanobot)
                .with_last_activity(now_ms() - 600_000),
        );
        insert("conv-nanobot", nanobot);

        let idle = mgr.collect_idle(300_000); // 5-min threshold
        assert_eq!(idle.len(), 1);
        assert_eq!(idle[0], "conv-stale");
    }

    #[test]
    fn collect_idle_empty_when_no_tasks() {
        let mgr = make_manager();
        let idle = mgr.collect_idle(300_000);
        assert!(idle.is_empty());
    }
}
