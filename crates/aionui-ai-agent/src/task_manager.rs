use std::sync::Arc;

use aionui_common::{AgentKillReason, AgentType, AppError, ConversationStatus, TimestampMs, now_ms};
use dashmap::DashMap;
use tracing::info;

use crate::agent_manager::AgentManagerHandle;
use crate::types::BuildTaskOptions;

/// Factory function that creates an [`AgentManagerHandle`] from build options.
///
/// This is provided at DI time by the application layer, which knows
/// how to construct each agent type (ACP, Gemini, etc.).
pub type AgentFactory =
    Arc<dyn Fn(BuildTaskOptions) -> Result<AgentManagerHandle, AppError> + Send + Sync>;

/// Manages the lifecycle of active Agent tasks.
///
/// Each conversation has at most one active task (keyed by conversation ID).
/// The trait is object-safe for dependency injection.
pub trait IWorkerTaskManager: Send + Sync {
    /// Get an existing task by conversation ID.
    fn get_task(&self, conversation_id: &str) -> Option<AgentManagerHandle>;

    /// Get an existing task or build a new one if none exists.
    fn get_or_build_task(
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

/// Default implementation of [`IWorkerTaskManager`] using a concurrent hash map.
pub struct WorkerTaskManagerImpl {
    tasks: DashMap<String, AgentManagerHandle>,
    factory: AgentFactory,
}

impl WorkerTaskManagerImpl {
    pub fn new(factory: AgentFactory) -> Self {
        Self {
            tasks: DashMap::new(),
            factory,
        }
    }
}

impl IWorkerTaskManager for WorkerTaskManagerImpl {
    fn get_task(&self, conversation_id: &str) -> Option<AgentManagerHandle> {
        self.tasks.get(conversation_id).map(|r| Arc::clone(&r))
    }

    fn get_or_build_task(
        &self,
        conversation_id: &str,
        options: BuildTaskOptions,
    ) -> Result<AgentManagerHandle, AppError> {
        // Fast path: task already exists
        if let Some(existing) = self.tasks.get(conversation_id) {
            return Ok(Arc::clone(&existing));
        }

        // Build a new task
        let handle = (self.factory)(options)?;

        // Use entry API to avoid TOCTOU race — if another thread inserted
        // between our get and this point, use the existing one.
        let entry = self
            .tasks
            .entry(conversation_id.to_owned())
            .or_insert(handle);
        Ok(Arc::clone(&entry))
    }

    fn kill(&self, conversation_id: &str, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        if let Some((id, agent)) = self.tasks.remove(conversation_id) {
            info!(conversation_id = %id, ?reason, "Killing agent task");
            agent.kill(reason)?;
        }
        Ok(())
    }

    fn clear(&self) {
        let keys: Vec<String> = self.tasks.iter().map(|r| r.key().clone()).collect();
        for key in keys {
            if let Some((id, agent)) = self.tasks.remove(&key) {
                info!(conversation_id = %id, "Clearing agent task");
                let _ = agent.kill(None);
            }
        }
    }

    fn active_count(&self) -> usize {
        self.tasks.len()
    }

    fn collect_idle(&self, idle_threshold_ms: TimestampMs) -> Vec<String> {
        let now = now_ms();
        self.tasks
            .iter()
            .filter(|entry| {
                let agent = entry.value();
                // Only ACP agents participate in idle cleanup per API Spec
                agent.agent_type() == AgentType::Acp
                    && agent.status() == Some(ConversationStatus::Finished)
                    && (now - agent.last_activity_at()) > idle_threshold_ms
            })
            .map(|entry| entry.key().clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_manager::IAgentManager;
    use crate::stream_event::AgentStreamEvent;
    use crate::types::SendMessageData;
    use aionui_common::{
        AgentKillReason, AgentType, Confirmation, ConversationStatus, ProviderWithModel,
    };
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
            Ok(Arc::new(MockAgent::new(&opts.conversation_id, None)) as AgentManagerHandle)
        });
        WorkerTaskManagerImpl::new(factory)
    }

    #[test]
    fn get_task_returns_none_when_empty() {
        let mgr = make_manager();
        assert!(mgr.get_task("nonexistent").is_none());
    }

    #[test]
    fn get_or_build_creates_task() {
        let mgr = make_manager();
        let handle = mgr.get_or_build_task("conv-1", make_options("conv-1")).unwrap();
        assert_eq!(handle.conversation_id(), "conv-1");
        assert_eq!(mgr.active_count(), 1);
    }

    #[test]
    fn get_or_build_returns_existing() {
        let mgr = make_manager();
        let h1 = mgr.get_or_build_task("conv-1", make_options("conv-1")).unwrap();
        let h2 = mgr.get_or_build_task("conv-1", make_options("conv-1")).unwrap();
        assert!(Arc::ptr_eq(&h1, &h2));
        assert_eq!(mgr.active_count(), 1);
    }

    #[test]
    fn get_task_finds_existing() {
        let mgr = make_manager();
        mgr.get_or_build_task("conv-1", make_options("conv-1")).unwrap();
        let handle = mgr.get_task("conv-1");
        assert!(handle.is_some());
        assert_eq!(handle.unwrap().conversation_id(), "conv-1");
    }

    #[test]
    fn kill_removes_task() {
        let mgr = make_manager();
        mgr.get_or_build_task("conv-1", make_options("conv-1")).unwrap();
        assert_eq!(mgr.active_count(), 1);

        mgr.kill("conv-1", Some(AgentKillReason::IdleTimeout)).unwrap();
        assert_eq!(mgr.active_count(), 0);
        assert!(mgr.get_task("conv-1").is_none());
    }

    #[test]
    fn kill_nonexistent_is_ok() {
        let mgr = make_manager();
        assert!(mgr.kill("nothing", None).is_ok());
    }

    #[test]
    fn clear_removes_all() {
        let mgr = make_manager();
        mgr.get_or_build_task("conv-1", make_options("conv-1")).unwrap();
        mgr.get_or_build_task("conv-2", make_options("conv-2")).unwrap();
        assert_eq!(mgr.active_count(), 2);

        mgr.clear();
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn collect_idle_finds_finished_and_stale_acp_tasks() {
        let mgr = WorkerTaskManagerImpl {
            tasks: DashMap::new(),
            factory: Arc::new(|_| unreachable!()),
        };

        // ACP + Finished + old activity → should be collected
        let stale = Arc::new(
            MockAgent::new("conv-stale", Some(ConversationStatus::Finished))
                .with_last_activity(now_ms() - 600_000), // 10 min ago
        );
        mgr.tasks.insert("conv-stale".into(), stale);

        // ACP + Finished + recent activity → should NOT be collected
        let recent = Arc::new(
            MockAgent::new("conv-recent", Some(ConversationStatus::Finished))
                .with_last_activity(now_ms()),
        );
        mgr.tasks.insert("conv-recent".into(), recent);

        // ACP + Running + old activity → should NOT be collected
        let running = Arc::new(
            MockAgent::new("conv-running", Some(ConversationStatus::Running))
                .with_last_activity(now_ms() - 600_000),
        );
        mgr.tasks.insert("conv-running".into(), running);

        // Non-ACP (Gemini) + Finished + old activity → should NOT be collected
        let gemini = Arc::new(
            MockAgent::new("conv-gemini", Some(ConversationStatus::Finished))
                .with_agent_type(AgentType::Gemini)
                .with_last_activity(now_ms() - 600_000),
        );
        mgr.tasks.insert("conv-gemini".into(), gemini);

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
