//! Connect-layer test fixtures shared across crates.
//!
//! Provides one `MockConnector` + `MockConnectorFactory` pair so any
//! future contract change to `IAgentConnector` only ripples through one
//! file. Consumed by `aionui-conversation`, `aionui-cron`,
//! `aionui-team`, and `aionui-app`.
//!
//! Gated behind `#[cfg(any(test, feature = "test-support"))]`:
//! production builds never see these types.

#![cfg(any(test, feature = "test-support"))]

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use aionui_api_types::{
    AgentModeResponse, GetModelInfoResponse, SideQuestionRequest, SideQuestionResponse, SlashCommandItem,
};
use aionui_common::{AgentKillReason, AgentType, AppError, Confirmation, TimestampMs};
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use tokio::sync::broadcast;

use crate::connector::{ConnectorError, ConnectorEvent, IAgentConnector, TurnSummary};
use crate::connector_factory::IAgentConnectorFactory;
use crate::protocol::events::{AgentStreamEvent, FinishEventData};
use crate::types::{BuildTaskOptions, SendMessageData};

/// Shared "fake" connector for unit / integration tests.
///
/// Built via [`MockConnectorBuilder`] so each call site can opt into the
/// extra behaviour it needs (scripted streams, pending confirmations,
/// approval memory, direct-confirm mode, custom workspace) without
/// growing a constructor zoo.
pub struct MockConnector {
    conversation_id: String,
    workspace: String,
    agent_type: AgentType,
    legacy_tx: broadcast::Sender<AgentStreamEvent>,
    connector_tx: broadcast::Sender<ConnectorEvent>,
    confirmations: Mutex<Vec<Confirmation>>,
    approval_memory: Mutex<HashMap<String, bool>>,
    allow_direct_confirm: bool,
    /// Pre-recorded event sequences fired in order on each `send_message`.
    /// Each `send_message` consumes one entry from the front; if the queue
    /// is empty the connector falls back to emitting a single
    /// `AgentStreamEvent::Finish`. Mirrors the old `ScriptedAgent`
    /// behaviour from `aionui-conversation/src/service_test.rs`.
    scripts: Mutex<VecDeque<Vec<AgentStreamEvent>>>,
    sent_contents: Mutex<Vec<String>>,
    /// Set to `true` by [`Self::cancel`] / [`Self::cancel_current_turn`].
    /// Tests can read this back via [`Self::was_cancelled`].
    cancelled: Mutex<bool>,
    set_mode_calls: Mutex<Vec<String>>,
    /// In-memory mode that survives `set_mode`/`get_mode` round trips.
    /// `None` means the connector reports `mode = "default"`,
    /// `initialized = false` (matches the legacy `IMockAgent::mode`
    /// default).
    mode: Mutex<Option<String>>,
}

impl MockConnector {
    pub fn builder(conversation_id: impl Into<String>) -> MockConnectorBuilder {
        MockConnectorBuilder::new(conversation_id)
    }

    /// Convenience: zero-config mock used by the bulk of the
    /// conversation-service tests where only the `IAgentConnectorFactory`
    /// surface is exercised.
    pub fn new(conversation_id: impl Into<String>) -> Self {
        MockConnectorBuilder::new(conversation_id).build()
    }

    pub fn sent_contents(&self) -> Vec<String> {
        self.sent_contents.lock().unwrap().clone()
    }

    pub fn was_cancelled(&self) -> bool {
        *self.cancelled.lock().unwrap()
    }

    pub fn set_mode_calls(&self) -> Vec<String> {
        self.set_mode_calls.lock().unwrap().clone()
    }

    pub fn legacy_sender(&self) -> broadcast::Sender<AgentStreamEvent> {
        self.legacy_tx.clone()
    }
}

#[async_trait]
impl IAgentConnector for MockConnector {
    fn agent_type(&self) -> AgentType {
        self.agent_type
    }
    fn conversation_id(&self) -> &str {
        &self.conversation_id
    }
    fn workspace(&self) -> &str {
        &self.workspace
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
        *self.cancelled.lock().unwrap() = true;
        Ok(())
    }
    fn subscribe(&self) -> broadcast::Receiver<ConnectorEvent> {
        self.connector_tx.subscribe()
    }
    fn subscribe_legacy(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.legacy_tx.subscribe()
    }

    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError> {
        self.sent_contents.lock().unwrap().push(data.content);
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![AgentStreamEvent::Finish(FinishEventData::default())]);
        for event in script {
            let _ = self.legacy_tx.send(event);
        }
        Ok(())
    }

    async fn cancel(&self) -> Result<(), AppError> {
        *self.cancelled.lock().unwrap() = true;
        Ok(())
    }

    fn kill(&self, _reason: Option<AgentKillReason>) -> Result<(), AppError> {
        Ok(())
    }

    fn kill_and_wait(&self, _reason: Option<AgentKillReason>) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(std::future::ready(()))
    }

    fn get_confirmations(&self) -> Vec<Confirmation> {
        self.confirmations.lock().unwrap().clone()
    }

    fn confirm(
        &self,
        _msg_id: &str,
        call_id: &str,
        _data: serde_json::Value,
        always_allow: bool,
    ) -> Result<(), AppError> {
        let mut confs = self.confirmations.lock().unwrap();
        let existed = confs.iter().any(|c| c.call_id == call_id);
        if !existed && !self.allow_direct_confirm {
            return Err(AppError::NotFound(format!("Confirmation {call_id} not found")));
        }
        if always_allow && let Some(conf) = confs.iter().find(|c| c.call_id == call_id) {
            let key = match (conf.action.as_deref(), conf.command_type.as_deref()) {
                (Some(a), Some(ct)) => format!("{a}:{ct}"),
                (Some(a), None) => a.to_owned(),
                _ => String::new(),
            };
            self.approval_memory.lock().unwrap().insert(key, true);
        }
        confs.retain(|c| c.call_id != call_id);
        Ok(())
    }

    fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool {
        let key = match command_type {
            Some(ct) => format!("{action}:{ct}"),
            None => action.to_owned(),
        };
        self.approval_memory.lock().unwrap().get(&key).copied().unwrap_or(false)
    }

    fn get_session_key(&self) -> Option<String> {
        None
    }

    async fn get_mode(&self) -> Result<AgentModeResponse, AppError> {
        match self.mode.lock().unwrap().clone() {
            Some(mode) => Ok(AgentModeResponse {
                mode,
                initialized: true,
            }),
            None => Ok(AgentModeResponse {
                mode: "default".into(),
                initialized: false,
            }),
        }
    }

    async fn set_mode(&self, mode: &str) -> Result<(), AppError> {
        self.set_mode_calls.lock().unwrap().push(mode.to_owned());
        *self.mode.lock().unwrap() = Some(mode.to_owned());
        Ok(())
    }

    async fn get_model(&self) -> Result<GetModelInfoResponse, AppError> {
        Ok(GetModelInfoResponse { model_info: None })
    }

    async fn set_model(&self, _model_id: &str) -> Result<(), AppError> {
        Err(AppError::BadRequest(
            "Model switching is not supported for this mock".into(),
        ))
    }

    async fn get_usage(&self) -> Result<Option<serde_json::Value>, AppError> {
        Ok(None)
    }

    async fn get_slash_commands(&self) -> Result<Vec<SlashCommandItem>, AppError> {
        Ok(Vec::new())
    }

    async fn handle_side_question(&self, _req: SideQuestionRequest) -> Result<SideQuestionResponse, AppError> {
        Ok(SideQuestionResponse {
            status: "unsupported".into(),
            answer: None,
        })
    }

    async fn get_openclaw_runtime(&self) -> Result<serde_json::Value, AppError> {
        Ok(serde_json::Value::Null)
    }
}

/// Fluent builder for [`MockConnector`].
pub struct MockConnectorBuilder {
    conversation_id: String,
    workspace: String,
    agent_type: AgentType,
    confirmations: Vec<Confirmation>,
    allow_direct_confirm: bool,
    scripts: VecDeque<Vec<AgentStreamEvent>>,
    initial_mode: Option<String>,
}

impl MockConnectorBuilder {
    pub fn new(conversation_id: impl Into<String>) -> Self {
        Self {
            conversation_id: conversation_id.into(),
            workspace: "/tmp/test".into(),
            agent_type: AgentType::Acp,
            confirmations: Vec::new(),
            allow_direct_confirm: false,
            scripts: VecDeque::new(),
            initial_mode: None,
        }
    }

    pub fn workspace(mut self, ws: impl Into<String>) -> Self {
        self.workspace = ws.into();
        self
    }

    pub fn agent_type(mut self, ty: AgentType) -> Self {
        self.agent_type = ty;
        self
    }

    pub fn confirmations(mut self, confs: Vec<Confirmation>) -> Self {
        self.confirmations = confs;
        self
    }

    pub fn allow_direct_confirm(mut self) -> Self {
        self.allow_direct_confirm = true;
        self
    }

    /// Queue up a script — one entry per `send_message` call.
    pub fn script(mut self, events: Vec<AgentStreamEvent>) -> Self {
        self.scripts.push_back(events);
        self
    }

    /// Initial session mode reported by `get_mode`. `None` keeps the
    /// default `mode = "default"`, `initialized = false` shape.
    pub fn initial_mode(mut self, mode: impl Into<String>) -> Self {
        self.initial_mode = Some(mode.into());
        self
    }

    pub fn build(self) -> MockConnector {
        let (legacy_tx, _) = broadcast::channel(64);
        let (connector_tx, _) = broadcast::channel(64);
        MockConnector {
            conversation_id: self.conversation_id,
            workspace: self.workspace,
            agent_type: self.agent_type,
            legacy_tx,
            connector_tx,
            confirmations: Mutex::new(self.confirmations),
            approval_memory: Mutex::new(HashMap::new()),
            allow_direct_confirm: self.allow_direct_confirm,
            scripts: Mutex::new(self.scripts),
            sent_contents: Mutex::new(Vec::new()),
            cancelled: Mutex::new(false),
            set_mode_calls: Mutex::new(Vec::new()),
            mode: Mutex::new(self.initial_mode),
        }
    }

    pub fn build_arc(self) -> Arc<MockConnector> {
        Arc::new(self.build())
    }
}

/// Per-conversation factory closure: lets a test customise the connector
/// produced by `build_or_get` for a given conversation id. Returning
/// `Err` from this closure surfaces as a `build_or_get` failure.
pub type MockConnectorBuildFn =
    Arc<dyn Fn(BuildTaskOptions) -> BoxFuture<'static, Result<Arc<dyn IAgentConnector>, AppError>> + Send + Sync>;

/// In-memory factory for use in tests.
///
/// Two ways to populate it:
/// - Pre-insert a connector via [`Self::insert`] — `build_or_get` returns
///   it without invoking the build closure.
/// - Register a build closure via [`MockConnectorFactoryBuilder::build_fn`] —
///   used when the test wants to assert what `build_or_get` did with the
///   incoming `BuildTaskOptions`.
pub struct MockConnectorFactory {
    /// Initialised connectors keyed by conversation id.
    connectors: Mutex<HashMap<String, Arc<dyn IAgentConnector>>>,
    /// Build closure invoked when `build_or_get` is called for an
    /// unseen conversation id.
    build_fn: MockConnectorBuildFn,
    /// Counts every `build_or_get` call (whether or not the build
    /// closure ran). Useful for assertions.
    build_calls: Mutex<u64>,
    /// Records every `drop_connector` call.
    drop_calls: Mutex<Vec<(String, Option<AgentKillReason>)>>,
}

impl MockConnectorFactory {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::with_default_build_fn())
    }

    fn with_default_build_fn() -> Self {
        let build_fn: MockConnectorBuildFn = Arc::new(|opts: BuildTaskOptions| {
            Box::pin(async move {
                let connector: Arc<dyn IAgentConnector> = Arc::new(MockConnector::new(opts.conversation_id));
                Ok(connector)
            })
        });
        Self {
            connectors: Mutex::new(HashMap::new()),
            build_fn,
            build_calls: Mutex::new(0),
            drop_calls: Mutex::new(Vec::new()),
        }
    }

    pub fn builder() -> MockConnectorFactoryBuilder {
        MockConnectorFactoryBuilder::default()
    }

    /// Insert a pre-built connector for `conversation_id`. Subsequent
    /// `build_or_get` / `get` calls return this exact `Arc`.
    pub fn insert(&self, conversation_id: impl Into<String>, connector: Arc<dyn IAgentConnector>) {
        self.connectors
            .lock()
            .unwrap()
            .insert(conversation_id.into(), connector);
    }

    pub fn build_call_count(&self) -> u64 {
        *self.build_calls.lock().unwrap()
    }

    pub fn drop_calls(&self) -> Vec<(String, Option<AgentKillReason>)> {
        self.drop_calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl IAgentConnectorFactory for MockConnectorFactory {
    async fn build_or_get(&self, opts: BuildTaskOptions) -> Result<Arc<dyn IAgentConnector>, AppError> {
        *self.build_calls.lock().unwrap() += 1;
        if let Some(existing) = self.connectors.lock().unwrap().get(&opts.conversation_id).cloned() {
            return Ok(existing);
        }
        let conv_id = opts.conversation_id.clone();
        let connector = (self.build_fn)(opts).await?;
        self.connectors.lock().unwrap().insert(conv_id, connector.clone());
        Ok(connector)
    }

    fn get(&self, conversation_id: &str) -> Option<Arc<dyn IAgentConnector>> {
        self.connectors.lock().unwrap().get(conversation_id).cloned()
    }

    fn drop_connector(&self, conversation_id: &str, reason: Option<AgentKillReason>) {
        self.drop_calls
            .lock()
            .unwrap()
            .push((conversation_id.to_owned(), reason));
        self.connectors.lock().unwrap().remove(conversation_id);
    }

    fn clear(&self) {
        self.connectors.lock().unwrap().clear();
    }

    fn active_count(&self) -> usize {
        self.connectors.lock().unwrap().len()
    }
}

/// Fluent builder for [`MockConnectorFactory`].
#[derive(Default)]
pub struct MockConnectorFactoryBuilder {
    build_fn: Option<MockConnectorBuildFn>,
}

impl MockConnectorFactoryBuilder {
    /// Override the closure used to construct connectors on a cache miss.
    pub fn build_fn(mut self, f: MockConnectorBuildFn) -> Self {
        self.build_fn = Some(f);
        self
    }

    /// Convenience: build connectors with the given fixed workspace,
    /// otherwise default `MockConnector::new`.
    pub fn fixed_workspace(self, workspace: impl Into<String>) -> Self {
        let ws: String = workspace.into();
        let f: MockConnectorBuildFn = Arc::new(move |opts: BuildTaskOptions| {
            let ws = ws.clone();
            Box::pin(async move {
                let connector: Arc<dyn IAgentConnector> =
                    MockConnector::builder(opts.conversation_id).workspace(ws).build_arc();
                Ok(connector)
            })
        });
        self.build_fn(f)
    }

    pub fn build(self) -> Arc<MockConnectorFactory> {
        let factory = match self.build_fn {
            Some(build_fn) => MockConnectorFactory {
                connectors: Mutex::new(HashMap::new()),
                build_fn,
                build_calls: Mutex::new(0),
                drop_calls: Mutex::new(Vec::new()),
            },
            None => MockConnectorFactory::with_default_build_fn(),
        };
        Arc::new(factory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn factory_caches_connector_by_conversation_id() {
        let f = MockConnectorFactory::builder().build();
        let opts = BuildTaskOptions {
            agent_type: AgentType::Acp,
            workspace: "/tmp/x".into(),
            model: aionui_common::ProviderWithModel {
                provider_id: "p".into(),
                model: "m".into(),
                use_model: None,
            },
            conversation_id: "conv-1".into(),
            extra: serde_json::Value::Null,
        };
        let c1 = f.build_or_get(opts.clone()).await.unwrap();
        let c2 = f.build_or_get(opts).await.unwrap();
        assert!(Arc::ptr_eq(&c1, &c2));
        assert_eq!(f.active_count(), 1);
        assert_eq!(f.build_call_count(), 2);
    }

    #[tokio::test]
    async fn drop_connector_evicts_slot() {
        let f = MockConnectorFactory::builder().build();
        let opts = BuildTaskOptions {
            agent_type: AgentType::Acp,
            workspace: "/tmp/x".into(),
            model: aionui_common::ProviderWithModel {
                provider_id: "p".into(),
                model: "m".into(),
                use_model: None,
            },
            conversation_id: "conv-1".into(),
            extra: serde_json::Value::Null,
        };
        f.build_or_get(opts).await.unwrap();
        f.drop_connector("conv-1", None);
        assert!(f.get("conv-1").is_none());
        assert_eq!(f.drop_calls().len(), 1);
    }

    #[tokio::test]
    async fn insert_skips_build_fn() {
        let f = MockConnectorFactory::builder().build();
        let pre: Arc<dyn IAgentConnector> = MockConnector::builder("conv-1").build_arc();
        f.insert("conv-1", pre.clone());
        assert!(Arc::ptr_eq(&pre, &f.get("conv-1").unwrap()));
    }

    #[tokio::test]
    async fn mock_connector_records_sent_contents() {
        let mc = MockConnector::new("conv-1");
        mc.send_message(SendMessageData {
            content: "hello".into(),
            msg_id: "msg-1".into(),
            files: Vec::new(),
            inject_skills: Vec::new(),
        })
        .await
        .unwrap();
        assert_eq!(mc.sent_contents(), vec!["hello".to_string()]);
    }
}
