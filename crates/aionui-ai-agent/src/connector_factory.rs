//! Factory for connect-layer agent connectors.
//!
//! Phase 5: replaces `IWorkerTaskManager`. The factory is purely
//! connect-layer concerned: build options in, `Arc<dyn IAgentConnector>`
//! out, cached per `conversation_id`. All runtime-state tracking
//! (turn lifecycle, idle policy) now lives on the conv layer's
//! `ConvActor`; this factory does only one job — single-flight build
//! and cache.

use std::sync::Arc;

use aionui_common::{AgentKillReason, AppError, OnConversationDelete};
use async_trait::async_trait;
use dashmap::DashMap;
use futures_util::future::BoxFuture;
use tokio::sync::OnceCell;
use tracing::info;

use crate::connector::IAgentConnector;
use crate::types::BuildTaskOptions;

/// Factory function that builds an `Arc<dyn IAgentConnector>` from
/// build options. Async so the factory can spawn a CLI process and
/// negotiate the protocol handshake without `block_on`.
pub type ConnectorBuildFn =
    Arc<dyn Fn(BuildTaskOptions) -> BoxFuture<'static, Result<Arc<dyn IAgentConnector>, AppError>> + Send + Sync>;

/// Per-conversation slot: an [`OnceCell`] that the first concurrent
/// caller initialises by running the build closure, and that every
/// subsequent caller awaits. Failed initialisations leave the cell
/// empty so the next caller may retry; the slot itself is only removed
/// on `drop_connector` / `clear`.
type Slot = Arc<OnceCell<Arc<dyn IAgentConnector>>>;

/// Manages the lifecycle of connect-layer connectors keyed by
/// conversation id.
///
/// Object-safe so the conv layer / cron can hold an
/// `Arc<dyn IAgentConnectorFactory>` for DI.
#[async_trait]
pub trait IAgentConnectorFactory: Send + Sync {
    /// Get an existing connector by conversation id, or build one via
    /// the registered factory closure. Concurrent callers with the
    /// same `conversation_id` block on the same `OnceCell` so the
    /// closure runs at most once per conversation.
    async fn build_or_get(&self, opts: BuildTaskOptions) -> Result<Arc<dyn IAgentConnector>, AppError>;

    /// Look up a fully-initialised connector. Returns `None` when no
    /// build has succeeded for this conversation yet.
    fn get(&self, conversation_id: &str) -> Option<Arc<dyn IAgentConnector>>;

    /// Drop the slot for `conversation_id` and call `close` on any
    /// initialised connector. Idempotent.
    fn drop_connector(&self, conversation_id: &str, reason: Option<AgentKillReason>);

    /// Drop every slot and close every initialised connector.
    fn clear(&self);

    /// Number of connectors currently held by the factory. Useful for
    /// diagnostics; counts only fully-initialised slots.
    fn active_count(&self) -> usize;
}

/// Default in-memory implementation of [`IAgentConnectorFactory`].
pub struct ConnectorFactory {
    slots: DashMap<String, Slot>,
    build: ConnectorBuildFn,
}

impl ConnectorFactory {
    pub fn new(build: ConnectorBuildFn) -> Self {
        Self {
            slots: DashMap::new(),
            build,
        }
    }

    fn initialised(&self, conversation_id: &str) -> Option<Arc<dyn IAgentConnector>> {
        self.slots.get(conversation_id).and_then(|s| s.get().cloned())
    }
}

#[async_trait]
impl IAgentConnectorFactory for ConnectorFactory {
    async fn build_or_get(&self, opts: BuildTaskOptions) -> Result<Arc<dyn IAgentConnector>, AppError> {
        let key = opts.conversation_id.clone();
        // Atomically obtain the per-conversation slot. `DashMap::entry`
        // is synchronous and side-effect-free — only an empty OnceCell
        // is allocated on the miss path, so concurrent callers for the
        // same id all end up holding the same `Arc<OnceCell>`.
        let slot: Slot = self
            .slots
            .entry(key)
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();
        let build = self.build.clone();
        let connector = slot.get_or_try_init(|| async move { build(opts).await }).await?;
        Ok(connector.clone())
    }

    fn get(&self, conversation_id: &str) -> Option<Arc<dyn IAgentConnector>> {
        self.initialised(conversation_id)
    }

    fn drop_connector(&self, conversation_id: &str, reason: Option<AgentKillReason>) {
        if let Some((id, slot)) = self.slots.remove(conversation_id) {
            info!(conversation_id = %id, ?reason, "Dropping connector slot");
            if let Some(connector) = slot.get() {
                connector.close(reason);
            }
        }
    }

    fn clear(&self) {
        let keys: Vec<String> = self.slots.iter().map(|r| r.key().clone()).collect();
        for key in keys {
            if let Some((id, slot)) = self.slots.remove(&key) {
                info!(conversation_id = %id, "Clearing connector slot");
                if let Some(connector) = slot.get() {
                    connector.close(None);
                }
            }
        }
    }

    fn active_count(&self) -> usize {
        self.slots.iter().filter(|entry| entry.value().get().is_some()).count()
    }
}

/// Wired up by `aionui-app` so deleting a conversation tears down its
/// connector. Without this hook, agent subprocesses keep streaming
/// events for a `conversation_id` whose DB row is already gone (Sentry
/// ELECTRON-1BD).
#[async_trait]
impl OnConversationDelete for ConnectorFactory {
    async fn on_conversation_deleted(&self, conversation_id: &str) {
        self.drop_connector(conversation_id, Some(AgentKillReason::ConversationDeleted));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_api_types::ConversationStatus;
    use aionui_common::{AgentType, Confirmation, ProviderWithModel, TimestampMs};
    use futures_util::FutureExt;
    use tokio::sync::broadcast;

    use crate::connector::{ConnectorError, ConnectorEvent, IAgentConnector, TurnSummary};
    use crate::protocol::events::AgentStreamEvent;
    use crate::types::SendMessageData;

    /// Minimal `IAgentConnector` implementation for unit tests.
    struct StubConnector {
        conversation_id: String,
    }

    #[async_trait]
    impl IAgentConnector for StubConnector {
        fn agent_type(&self) -> AgentType {
            AgentType::Acp
        }
        fn conversation_id(&self) -> &str {
            &self.conversation_id
        }
        fn workspace(&self) -> &str {
            "/tmp/test"
        }
        fn last_activity_at(&self) -> TimestampMs {
            0
        }
        fn is_open(&self) -> bool {
            true
        }
        async fn open(&self) -> Result<(), ConnectorError> {
            Ok(())
        }
        fn close(&self, _reason: Option<AgentKillReason>) {}
        async fn run_turn(&self, _msg: SendMessageData) -> Result<TurnSummary, ConnectorError> {
            Ok(TurnSummary::default())
        }
        async fn cancel_current_turn(&self) -> Result<(), ConnectorError> {
            Ok(())
        }
        fn subscribe(&self) -> broadcast::Receiver<ConnectorEvent> {
            let (tx, rx) = broadcast::channel(1);
            drop(tx);
            rx
        }
        fn subscribe_legacy(&self) -> broadcast::Receiver<AgentStreamEvent> {
            let (tx, rx) = broadcast::channel(1);
            drop(tx);
            rx
        }

        // Task-manager surface (Phase 5 additive): trivial stubs since
        // the only callers under test are the slot-management tests.
        fn status(&self) -> Option<ConversationStatus> {
            None
        }
        async fn send_message(&self, _data: SendMessageData) -> Result<(), AppError> {
            Ok(())
        }
        async fn cancel(&self) -> Result<(), AppError> {
            Ok(())
        }
        fn kill(&self, _reason: Option<AgentKillReason>) -> Result<(), AppError> {
            Ok(())
        }
        fn kill_and_wait(
            &self,
            _reason: Option<AgentKillReason>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
            Box::pin(std::future::ready(()))
        }
        fn get_confirmations(&self) -> Vec<Confirmation> {
            Vec::new()
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
        fn check_approval(&self, _action: &str, _command_type: Option<&str>) -> bool {
            false
        }
        fn get_session_key(&self) -> Option<String> {
            None
        }
        async fn get_mode(&self) -> Result<aionui_api_types::AgentModeResponse, AppError> {
            Ok(aionui_api_types::AgentModeResponse {
                mode: "default".into(),
                initialized: false,
            })
        }
        async fn set_mode(&self, _mode: &str) -> Result<(), AppError> {
            Ok(())
        }
        async fn get_model(&self) -> Result<aionui_api_types::GetModelInfoResponse, AppError> {
            Ok(aionui_api_types::GetModelInfoResponse { model_info: None })
        }
        async fn set_model(&self, _model_id: &str) -> Result<(), AppError> {
            Ok(())
        }
        async fn get_usage(&self) -> Result<Option<serde_json::Value>, AppError> {
            Ok(None)
        }
        async fn get_slash_commands(&self) -> Result<Vec<aionui_api_types::SlashCommandItem>, AppError> {
            Ok(Vec::new())
        }
        async fn handle_side_question(
            &self,
            _req: aionui_api_types::SideQuestionRequest,
        ) -> Result<aionui_api_types::SideQuestionResponse, AppError> {
            Ok(aionui_api_types::SideQuestionResponse {
                status: "unsupported".into(),
                answer: None,
            })
        }
        async fn get_openclaw_runtime(&self) -> Result<serde_json::Value, AppError> {
            Ok(serde_json::Value::Null)
        }
    }

    fn build_fn() -> ConnectorBuildFn {
        Arc::new(|opts: BuildTaskOptions| {
            async move {
                let connector: Arc<dyn IAgentConnector> = Arc::new(StubConnector {
                    conversation_id: opts.conversation_id,
                });
                Ok(connector)
            }
            .boxed()
        })
    }

    fn opts(id: &str) -> BuildTaskOptions {
        BuildTaskOptions {
            agent_type: AgentType::Acp,
            workspace: "/tmp/test".into(),
            model: ProviderWithModel {
                provider_id: "p1".into(),
                model: "test".into(),
                use_model: None,
            },
            conversation_id: id.into(),
            extra: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn build_or_get_returns_same_arc_for_same_conv_id() {
        let factory = ConnectorFactory::new(build_fn());
        let c1 = factory.build_or_get(opts("conv-1")).await.unwrap();
        let c2 = factory.build_or_get(opts("conv-1")).await.unwrap();
        assert!(Arc::ptr_eq(&c1, &c2));
        assert_eq!(factory.active_count(), 1);
    }

    #[tokio::test]
    async fn drop_connector_evicts_slot() {
        let factory = ConnectorFactory::new(build_fn());
        let c1 = factory.build_or_get(opts("conv-1")).await.unwrap();
        factory.drop_connector("conv-1", None);
        let c2 = factory.build_or_get(opts("conv-1")).await.unwrap();
        assert!(!Arc::ptr_eq(&c1, &c2));
    }

    #[tokio::test]
    async fn build_or_get_is_single_flight_under_concurrency() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_factory = Arc::clone(&calls);
        let build: ConnectorBuildFn = Arc::new(move |opts: BuildTaskOptions| {
            let calls = Arc::clone(&calls_for_factory);
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                calls.fetch_add(1, Ordering::SeqCst);
                let connector: Arc<dyn IAgentConnector> = Arc::new(StubConnector {
                    conversation_id: opts.conversation_id,
                });
                Ok(connector)
            }
            .boxed()
        });
        let factory = Arc::new(ConnectorFactory::new(build));

        let mut joins = Vec::new();
        for _ in 0..10 {
            let f = factory.clone();
            joins.push(tokio::spawn(async move { f.build_or_get(opts("conv-race")).await }));
        }
        let handles: Vec<_> = futures_util::future::join_all(joins)
            .await
            .into_iter()
            .map(|r| r.unwrap().unwrap())
            .collect();

        assert_eq!(calls.load(Ordering::SeqCst), 1, "build closure must run only once");
        assert_eq!(factory.active_count(), 1);
        for h in handles.iter().skip(1) {
            assert!(Arc::ptr_eq(&handles[0], h));
        }
    }

    #[tokio::test]
    async fn build_or_get_retries_after_failure() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let fail_next = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&fail_next);
        let build: ConnectorBuildFn = Arc::new(move |opts: BuildTaskOptions| {
            let flag = Arc::clone(&flag);
            async move {
                if flag.swap(false, Ordering::SeqCst) {
                    Err(AppError::Internal("first call fails".into()))
                } else {
                    let connector: Arc<dyn IAgentConnector> = Arc::new(StubConnector {
                        conversation_id: opts.conversation_id,
                    });
                    Ok(connector)
                }
            }
            .boxed()
        });
        let factory = ConnectorFactory::new(build);

        assert!(factory.build_or_get(opts("conv-1")).await.is_err());
        let c = factory.build_or_get(opts("conv-1")).await.unwrap();
        assert_eq!(c.conversation_id(), "conv-1");
        assert_eq!(factory.active_count(), 1);
    }

    #[tokio::test]
    async fn clear_removes_all_slots() {
        let factory = ConnectorFactory::new(build_fn());
        factory.build_or_get(opts("conv-1")).await.unwrap();
        factory.build_or_get(opts("conv-2")).await.unwrap();
        assert_eq!(factory.active_count(), 2);
        factory.clear();
        assert_eq!(factory.active_count(), 0);
    }

    #[tokio::test]
    async fn on_conversation_deleted_drops_connector() {
        let factory = ConnectorFactory::new(build_fn());
        factory.build_or_get(opts("conv-1")).await.unwrap();
        OnConversationDelete::on_conversation_deleted(&factory, "conv-1").await;
        assert_eq!(factory.active_count(), 0);
    }
}
