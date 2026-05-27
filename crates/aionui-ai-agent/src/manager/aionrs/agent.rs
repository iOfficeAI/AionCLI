use std::path::PathBuf;
use std::sync::Arc;

use aion_agent::bootstrap::AgentBootstrap;
use aion_agent::engine::AgentEngine;
use aion_agent::output::OutputSink;
use aion_agent::session::Session;
use aion_config::config::{CliArgs, Config};
use aion_mcp::manager::McpManager;
use aion_protocol::commands::SessionMode;
use aion_protocol::{ToolApprovalManager, ToolApprovalResult};
use aionui_api_types::{AgentModeResponse, ConversationStatus};
use aionui_common::{AgentKillReason, AgentType, AppError, Confirmation, ErrorChain, TimestampMs, now_ms};
use serde_json::Value;
use tokio::sync::{Mutex, Notify, broadcast, oneshot};
use tracing::{debug, error, info};

use crate::agent_runtime::AgentRuntime;
use crate::capability::backend_output_sink::BackendOutputSink;
use crate::capability::backend_protocol_sink::BackendProtocolSink;
use crate::connector::{ChunkPayload, ConnectorError, ConnectorEvent, IAgentConnector, StopReason, TurnSummary};
use crate::protocol::events::AgentStreamEvent;
use crate::types::{AionrsResolvedConfig, SendMessageData};

pub struct AionrsAgentManager {
    runtime: AgentRuntime,
    engine: Mutex<AgentEngine>,
    /// Holds `Arc<McpManager>` instances alive for the duration of this agent's
    /// lifetime. The managers are not accessed after construction — they exist
    /// solely so their underlying MCP connections outlive the engine's event
    /// loop. Rust drops them here, in field-declaration order, after `engine`
    /// and `runtime` are dropped. See the explicit `Drop` impl below.
    #[allow(dead_code)] // intentional: lifetime-extension only; see Drop impl
    mcp_managers: Vec<Arc<McpManager>>,
    approval_manager: Arc<ToolApprovalManager>,
    confirmations: Arc<std::sync::RwLock<Vec<Confirmation>>>,
    /// Signalled by `cancel()` to abort an in-flight `engine.run()` via
    /// `tokio::select!` in `send_message()`.
    cancel_notify: Arc<Notify>,
    /// Receiver-side of the current turn's done-signal. Held in a
    /// `Mutex<Option<..>>` so `cancel_current_turn` can take ownership and
    /// await it. `None` means no turn is in flight. Writers: `begin_turn`
    /// (sets) and `take_turn_done_rx` (clears). Readers: `cancel_current_turn`
    /// awaits the receiver side; the sender is held inside a `TurnGuard` so
    /// drop on the turn's exit path signals completion.
    turn_done_rx: Mutex<Option<oneshot::Receiver<()>>>,
}

impl Drop for AionrsAgentManager {
    fn drop(&mut self) {
        // McpManagers are held alive by the `mcp_managers` field specifically
        // so they outlive the agent's event loop. No explicit cleanup is needed
        // here — the Arc drop path releases each McpManager's underlying MCP
        // connection. This impl exists to document the intentional Drop-order
        // semantics rather than as a lint escape hatch.
    }
}

impl AionrsAgentManager {
    pub async fn new(
        conversation_id: String,
        workspace: String,
        config_extra: AionrsResolvedConfig,
        resume_session: Option<Session>,
    ) -> Result<Self, AppError> {
        let runtime = AgentRuntime::new(conversation_id.clone(), workspace.clone(), 128);
        let sink: Arc<dyn OutputSink> = Arc::new(BackendOutputSink::new(runtime.event_sender()));

        let cli_args = CliArgs {
            provider: Some(config_extra.provider.clone()),
            api_key: Some(config_extra.api_key.clone()),
            base_url: config_extra.base_url.clone(),
            model: Some(config_extra.model.clone()),
            max_tokens: Some(config_extra.max_tokens),
            max_turns: config_extra.max_turns,
            system_prompt: config_extra.system_prompt.clone(),
            profile: None,
            auto_approve: config_extra.session_mode.as_deref() == Some("yolo"),
            project_dir: Some(PathBuf::from(&workspace)),
        };

        let mut config =
            Config::resolve(&cli_args).map_err(|e| AppError::Internal(format!("Config resolve failed: {e}")))?;

        // Backend-specific overrides
        config.bedrock = config_extra.bedrock_config;
        config.session.enabled = true;
        config.session.directory = config_extra.session_directory.to_string_lossy().into_owned();

        if let Some(field) = config_extra.compat_overrides.max_tokens_field {
            config.compat.max_tokens_field = Some(field);
        }
        if let Some(path) = config_extra.compat_overrides.api_path {
            config.compat.api_path = Some(path);
        }

        if !config_extra.extra_mcp_servers.is_empty() {
            config.mcp.servers.extend(config_extra.extra_mcp_servers.clone());
        }

        let is_resume = resume_session.is_some();
        let provider_label = config.provider_label.clone();

        let mut bootstrap = AgentBootstrap::new(config, &workspace, sink);
        if let Some(session) = resume_session {
            info!(
                conversation_id = %conversation_id,
                session_id = %session.id,
                message_count = session.messages.len(),
                "Resuming aionrs session"
            );
            bootstrap = bootstrap.resume(session);
        }

        let result = bootstrap
            .build()
            .await
            .map_err(|e| AppError::Internal(format!("Agent bootstrap failed: {e}")))?;

        let mut engine = result.engine;
        if !is_resume && let Err(e) = engine.init_session(&provider_label, &workspace, Some(&conversation_id)) {
            error!(
                conversation_id = %conversation_id,
                error = %ErrorChain(&*e),
                "Failed to init session, continuing without persistence"
            );
        }

        let approval_manager = Arc::new(ToolApprovalManager::new());

        if let Some(mode_str) = &config_extra.session_mode {
            let mode = parse_session_mode(mode_str);
            approval_manager.set_mode(mode);
            info!(
                conversation_id = %conversation_id,
                session_mode = mode_str,
                "Aionrs initial session mode applied"
            );
        }

        let confirmations = Arc::new(std::sync::RwLock::new(Vec::new()));
        let protocol_sink = BackendProtocolSink::new(runtime.event_sender(), confirmations.clone());
        engine.set_approval_manager(approval_manager.clone());
        engine.set_protocol_writer(Arc::new(protocol_sink));

        runtime.transition_to(ConversationStatus::Pending);

        Ok(Self {
            runtime,
            engine: Mutex::new(engine),
            mcp_managers: result.mcp_managers,
            approval_manager,
            confirmations,
            cancel_notify: Arc::new(Notify::new()),
            turn_done_rx: Mutex::new(None),
        })
    }
}

/// RAII guard that signals turn completion via a oneshot when dropped.
///
/// `_done_tx` is intentionally `Option<oneshot::Sender<()>>` so the field
/// can be moved out of `Some(..)` into `None` on drop without unsafe code.
/// The actual signalling happens through Drop on `oneshot::Sender`.
pub struct TurnGuard {
    _done_tx: Option<oneshot::Sender<()>>,
}

impl AionrsAgentManager {
    /// Register a new turn-done pair. Returns a guard that signals on drop.
    /// Returns `None` if a turn is already in flight (single-flight defence).
    pub(crate) fn begin_turn(&self) -> Option<TurnGuard> {
        let (done_tx, done_rx) = oneshot::channel();
        let mut slot = self.turn_done_rx.try_lock().ok()?;
        if slot.is_some() {
            return None;
        }
        *slot = Some(done_rx);
        Some(TurnGuard {
            _done_tx: Some(done_tx),
        })
    }

    /// Take the current turn's done-receiver, if any.
    pub(crate) async fn take_turn_done_rx(&self) -> Option<oneshot::Receiver<()>> {
        self.turn_done_rx.lock().await.take()
    }

    /// Begin a turn slot exposed for integration tests under the same
    /// `cfg(any(test, feature = "test-support"))` gate used by other test
    /// hooks in this crate. Allows downstream tests to simulate an
    /// in-flight turn without a real LLM provider.
    #[cfg(any(test, feature = "test-support"))]
    pub fn begin_turn_for_test(&self) -> Option<TurnGuard> {
        self.begin_turn()
    }

    /// Issue a connector-style cancel from integration tests. Mirrors
    /// `IAgentConnector::cancel_current_turn` semantics: signals
    /// `cancel_notify` then awaits the in-flight turn's done-receiver.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn cancel_for_test(&self) -> Result<(), AppError> {
        // Mirrors connector cancel_current_turn semantics.
        self.cancel_notify.notify_waiters();
        if let Some(rx) = self.take_turn_done_rx().await {
            let _ = rx.await;
        }
        Ok(())
    }

    /// Drives one turn through the engine. Caller MUST already hold a
    /// `TurnGuard` (acquired via `begin_turn`). Used by both
    /// `IAgentTask::send_message` and `IAgentConnector::run_turn` so the
    /// engine drive logic stays in one place.
    async fn run_turn_inner(&self, data: SendMessageData) -> Result<TurnSummary, ConnectorError> {
        let started_at = now_ms();
        self.runtime.bump_activity();
        self.runtime.reset_for_new_turn(ConversationStatus::Running);

        let mut engine = self.engine.lock().await;

        let result = tokio::select! {
            res = engine.run(&data.content, &data.msg_id) => Some(res),
            _ = self.cancel_notify.notified() => {
                info!(
                    conversation_id = %self.runtime.conversation_id(),
                    "Aionrs engine.run() cancelled by user"
                );
                engine.abort_current_turn("Tool execution canceled by user");
                None
            }
        };

        let elapsed_ms = now_ms() - started_at;
        self.runtime.bump_activity();

        match result {
            Some(Ok(_)) => {
                info!(
                    conversation_id = %self.runtime.conversation_id(),
                    elapsed_ms,
                    "Aionrs engine.run() completed, emitting Finish"
                );
                self.runtime.emit_finish(None);
                Ok(TurnSummary {
                    session_id: None,
                    stop_reason: Some(StopReason::EndTurn),
                })
            }
            Some(Err(e)) => {
                let error_msg = format!("Aionrs agent error: {e}");
                error!(
                    conversation_id = %self.runtime.conversation_id(),
                    elapsed_ms,
                    error = %ErrorChain(&e),
                    "Aionrs engine.run() failed, emitting Error+Finish"
                );
                self.runtime.emit_error(error_msg.clone());
                self.runtime.emit_finish(None);
                Err(ConnectorError::Protocol(error_msg))
            }
            None => {
                self.runtime.emit_error("Stopped by user");
                self.runtime.emit_finish(None);
                Ok(TurnSummary {
                    session_id: None,
                    stop_reason: Some(StopReason::Cancelled),
                })
            }
        }
    }
}

#[async_trait::async_trait]
impl IAgentConnector for AionrsAgentManager {
    fn agent_type(&self) -> AgentType {
        AgentType::Aionrs
    }
    fn conversation_id(&self) -> &str {
        self.runtime.conversation_id()
    }
    fn workspace(&self) -> &str {
        self.runtime.workspace()
    }
    fn last_activity_at(&self) -> TimestampMs {
        self.runtime.last_activity_at()
    }
    fn is_open(&self) -> bool {
        true
    }

    async fn open(&self) -> Result<(), ConnectorError> {
        Ok(())
    }

    fn close(&self, reason: Option<AgentKillReason>) {
        let _ = crate::agent_task::IAgentTask::kill(self, reason);
    }

    async fn run_turn(&self, msg: SendMessageData) -> Result<TurnSummary, ConnectorError> {
        let _turn_guard = self.begin_turn().ok_or(ConnectorError::Busy)?;
        self.run_turn_inner(msg).await
    }

    async fn cancel_current_turn(&self) -> Result<(), ConnectorError> {
        if let Ok(mut confs) = self.confirmations.write() {
            confs.clear();
        }
        self.cancel_notify.notify_waiters();
        if let Some(rx) = self.take_turn_done_rx().await {
            let _ = rx.await;
        }
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<ConnectorEvent> {
        // Bridge the token-level AgentStreamEvent channel into
        // ConnectorEvent::Chunk so turn-level subscribers see chunks
        // alongside lifecycle events.
        let (tx, rx) = broadcast::channel(64);
        let mut legacy = self.runtime.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = legacy.recv().await {
                let _ = tx.send(ConnectorEvent::Chunk(ChunkPayload { event: ev }));
            }
        });
        rx
    }

    fn subscribe_legacy(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.runtime.subscribe()
    }

    // ── Lifecycle / control surface ─────────────────────────────────────
    //
    // Delegates to the crate-private `IAgentTask` impl on `Self` or to
    // the inherent helpers below.

    fn status(&self) -> Option<ConversationStatus> {
        self.runtime.status()
    }

    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError> {
        crate::agent_task::IAgentTask::send_message(self, data).await
    }

    async fn cancel(&self) -> Result<(), AppError> {
        crate::agent_task::IAgentTask::cancel(self).await
    }

    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        crate::agent_task::IAgentTask::kill(self, reason)
    }

    fn kill_and_wait(
        &self,
        reason: Option<AgentKillReason>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        AionrsAgentManager::kill_and_wait(self, reason)
    }

    fn get_confirmations(&self) -> Vec<Confirmation> {
        AionrsAgentManager::get_confirmations(self)
    }

    fn confirm(
        &self,
        msg_id: &str,
        call_id: &str,
        data: serde_json::Value,
        always_allow: bool,
    ) -> Result<(), AppError> {
        AionrsAgentManager::confirm(self, msg_id, call_id, data, always_allow)
    }

    fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool {
        AionrsAgentManager::check_approval(self, action, command_type)
    }

    /// Aionrs does not expose a session key; mirrors the existing
    /// `AgentInstance::Aionrs(_)` arm in `get_session_key`.
    fn get_session_key(&self) -> Option<String> {
        None
    }

    async fn get_mode(&self) -> Result<aionui_api_types::AgentModeResponse, AppError> {
        AionrsAgentManager::mode(self).await
    }

    async fn set_mode(&self, mode: &str) -> Result<(), AppError> {
        AionrsAgentManager::set_mode(self, mode).await
    }

    /// Mirrors the existing `AgentInstance::Aionrs(_)` arm in
    /// `get_model` — Aionrs has no model picker.
    async fn get_model(&self) -> Result<aionui_api_types::GetModelInfoResponse, AppError> {
        Ok(aionui_api_types::GetModelInfoResponse { model_info: None })
    }

    /// Mirrors the existing `AgentInstance::Aionrs(_)` arm in
    /// `set_model` — model switching is not supported.
    async fn set_model(&self, model_id: &str) -> Result<(), AppError> {
        if model_id.trim().is_empty() {
            return Err(AppError::BadRequest("model_id must not be empty".into()));
        }
        Err(AppError::BadRequest(
            "Model switching is not supported for this agent type".into(),
        ))
    }

    async fn get_usage(&self) -> Result<Option<serde_json::Value>, AppError> {
        Ok(None)
    }

    async fn get_slash_commands(&self) -> Result<Vec<aionui_api_types::SlashCommandItem>, AppError> {
        AionrsAgentManager::get_slash_commands(self).await
    }

    async fn handle_side_question(
        &self,
        req: aionui_api_types::SideQuestionRequest,
    ) -> Result<aionui_api_types::SideQuestionResponse, AppError> {
        if req.question.trim().is_empty() {
            return Err(AppError::BadRequest("question must not be empty".into()));
        }
        Ok(aionui_api_types::SideQuestionResponse {
            status: "unsupported".into(),
            answer: None,
        })
    }

    async fn get_openclaw_runtime(&self) -> Result<serde_json::Value, AppError> {
        Ok(serde_json::Value::Null)
    }
}

#[async_trait::async_trait]
impl crate::agent_task::IAgentTask for AionrsAgentManager {
    fn status(&self) -> Option<ConversationStatus> {
        self.runtime.status()
    }

    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError> {
        let _turn_guard = match self.begin_turn() {
            Some(g) => g,
            None => return Err(AppError::Conflict("Aionrs turn already in flight".into())),
        };
        info!(
            conversation_id = %self.runtime.conversation_id(),
            msg_id = %data.msg_id,
            "Aionrs send_message started"
        );

        let outcome = self.run_turn_inner(data).await;

        // Map TurnSummary → Ok(()), ConnectorError::Protocol → AppError::Internal.
        let mapped = match outcome {
            Ok(_) => Ok(()),
            Err(crate::connector::ConnectorError::Protocol(msg)) => Err(AppError::Internal(msg)),
            Err(crate::connector::ConnectorError::Busy) => {
                Err(AppError::Conflict("Aionrs turn already in flight".into()))
            }
            Err(e) => Err(AppError::Internal(format!("{e}"))),
        };

        drop(_turn_guard);
        mapped
    }

    async fn cancel(&self) -> Result<(), AppError> {
        info!(
            conversation_id = %self.runtime.conversation_id(),
            "Aionrs stop requested"
        );
        if let Ok(mut confs) = self.confirmations.write() {
            confs.clear();
        }
        // Signal the tokio::select! in send_message() to drop engine.run()
        self.cancel_notify.notify_waiters();
        Ok(())
    }

    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        info!(
            conversation_id = %self.runtime.conversation_id(),
            ?reason,
            "Killing Aionrs agent"
        );
        Ok(())
    }
}

impl AionrsAgentManager {
    pub fn kill_and_wait(
        &self,
        reason: Option<AgentKillReason>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let _ = crate::agent_task::IAgentTask::kill(self, reason);
        Box::pin(std::future::ready(()))
    }
}

/// Aionrs-specific operations reached through `AgentInstance::Aionrs(..)`
/// matches in the routes + services.
impl AionrsAgentManager {
    pub fn confirm(&self, _msg_id: &str, call_id: &str, data: Value, always_allow: bool) -> Result<(), AppError> {
        if let Ok(mut confs) = self.confirmations.write() {
            confs.retain(|c| c.call_id != call_id);
        }

        let value = data.get("value").and_then(|v| v.as_str()).unwrap_or("cancel");

        let is_cancel = value == "cancel";

        debug!(
            conversation_id = %self.runtime.conversation_id(),
            call_id,
            value,
            always_allow,
            "Aionrs confirm"
        );

        if is_cancel {
            self.approval_manager.resolve(
                call_id,
                ToolApprovalResult::Denied {
                    reason: "User denied the tool request".into(),
                },
            );
        } else {
            let scope = if always_allow {
                aion_protocol::commands::ApprovalScope::Always
            } else {
                aion_protocol::commands::ApprovalScope::Once
            };
            self.approval_manager.approve(call_id, scope);
        }
        Ok(())
    }

    pub fn get_confirmations(&self) -> Vec<Confirmation> {
        self.confirmations.read().map(|c| c.clone()).unwrap_or_default()
    }

    pub fn check_approval(&self, action: &str, _command_type: Option<&str>) -> bool {
        self.approval_manager.is_auto_approved(action)
    }

    pub async fn mode(&self) -> Result<AgentModeResponse, AppError> {
        Ok(AgentModeResponse {
            mode: self.approval_manager.current_mode(),
            initialized: true,
        })
    }

    pub async fn set_mode(&self, mode: &str) -> Result<(), AppError> {
        let prev = self.approval_manager.current_mode();
        self.approval_manager.set_mode(parse_session_mode(mode));
        info!(
            conversation_id = %self.runtime.conversation_id(),
            from = prev,
            to = mode,
            "Aionrs session mode switched"
        );
        Ok(())
    }

    pub async fn get_slash_commands(&self) -> Result<Vec<aionui_api_types::SlashCommandItem>, AppError> {
        let engine = self.engine.lock().await;
        Ok(engine
            .slash_command_list()
            .into_iter()
            .map(|(command, description)| aionui_api_types::SlashCommandItem { command, description })
            .collect())
    }
}

fn parse_session_mode(s: &str) -> SessionMode {
    match s {
        "auto_edit" => SessionMode::AutoEdit,
        "yolo" => SessionMode::Yolo,
        _ => SessionMode::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IAgentConnector;

    fn make_test_config() -> AionrsResolvedConfig {
        AionrsResolvedConfig {
            provider: "anthropic".into(),
            api_key: "sk-test-key".into(),
            model: "claude-sonnet-4-20250514".into(),
            base_url: None,
            system_prompt: None,
            max_tokens: 4096,
            max_turns: None,
            compat_overrides: Default::default(),
            session_directory: std::env::temp_dir().join("aionrs-test-sessions"),
            session_mode: None,
            extra_mcp_servers: std::collections::HashMap::new(),
            bedrock_config: None,
        }
    }

    #[tokio::test]
    async fn aionrs_agent_returns_correct_type() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
            .await
            .unwrap();
        assert_eq!(IAgentConnector::agent_type(&agent), AgentType::Aionrs);
        assert_eq!(IAgentConnector::workspace(&agent), "/project");
        assert_eq!(IAgentConnector::conversation_id(&agent), "conv-1");
    }

    #[tokio::test]
    async fn aionrs_agent_initial_status_is_pending() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
            .await
            .unwrap();
        assert_eq!(IAgentConnector::status(&agent), Some(ConversationStatus::Pending));
    }

    #[tokio::test]
    async fn aionrs_agent_subscribe_returns_receiver() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
            .await
            .unwrap();
        let _rx = IAgentConnector::subscribe_legacy(&agent);
    }

    #[tokio::test]
    async fn aionrs_agent_kill_succeeds() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
            .await
            .unwrap();
        assert!(IAgentConnector::kill(&agent, None).is_ok());
        // kill() is a no-op for aionrs (no subprocess); status remains Pending.
        assert_eq!(IAgentConnector::status(&agent), Some(ConversationStatus::Pending));
    }

    #[tokio::test]
    async fn aionrs_agent_kill_with_reason_succeeds() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
            .await
            .unwrap();
        assert!(IAgentConnector::kill(&agent, Some(AgentKillReason::IdleTimeout)).is_ok());
    }

    #[tokio::test]
    async fn aionrs_agent_confirmations_initially_empty() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
            .await
            .unwrap();
        assert!(agent.get_confirmations().is_empty());
    }

    #[tokio::test]
    async fn aionrs_agent_check_approval_returns_false_by_default() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
            .await
            .unwrap();
        assert!(!agent.check_approval("any_action", None));
    }

    #[tokio::test]
    async fn stop_only_signals_in_flight_run() {
        let agent = AionrsAgentManager::new("conv-stop".into(), "/project".into(), make_test_config(), None)
            .await
            .unwrap();
        let mut rx = IAgentConnector::subscribe_legacy(&agent);

        IAgentConnector::cancel(&agent).await.unwrap();

        assert_eq!(IAgentConnector::status(&agent), Some(ConversationStatus::Pending));
        assert!(matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn iagent_connector_basics() {
        use crate::connector::IAgentConnector;

        let agent = AionrsAgentManager::new("conv-c".into(), "/project".into(), make_test_config(), None)
            .await
            .unwrap();
        let connector: &dyn IAgentConnector = &agent;
        assert_eq!(connector.agent_type(), AgentType::Aionrs);
        assert_eq!(connector.conversation_id(), "conv-c");
        assert_eq!(connector.workspace(), "/project");
        // open() is idempotent on aionrs (no separate handshake).
        assert!(connector.is_open());
        connector.open().await.unwrap();
    }

    #[tokio::test]
    async fn iagent_connector_concurrent_run_turn_serializes() {
        use crate::connector::{ConnectorError, IAgentConnector};
        use std::sync::Arc;

        let agent = Arc::new(
            AionrsAgentManager::new("conv-s".into(), "/project".into(), make_test_config(), None)
                .await
                .unwrap(),
        );

        // Hold a turn slot so the next run_turn observes Busy.
        let _guard = agent.begin_turn_for_test().unwrap();

        let connector: Arc<dyn IAgentConnector> = agent.clone();
        let result = connector
            .run_turn(SendMessageData {
                content: "hi".into(),
                msg_id: "m1".into(),
                files: vec![],
                inject_skills: vec![],
            })
            .await;
        assert!(matches!(result, Err(ConnectorError::Busy)));
    }

    #[tokio::test]
    async fn cancel_waits_for_in_flight_run_to_drop() {
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::sync::Notify;

        let agent = Arc::new(
            AionrsAgentManager::new("conv-cancel".into(), "/project".into(), make_test_config(), None)
                .await
                .unwrap(),
        );

        // Spawn a fake turn: register a turn_done pair manually, then sleep.
        // We can't drive a real engine.run() in unit tests (no provider), so we
        // exercise the lifecycle hook directly.
        let release = Arc::new(Notify::new());
        let release_for_task = release.clone();
        let agent_for_task = agent.clone();
        let turn = tokio::spawn(async move {
            let _guard = agent_for_task.begin_turn_for_test().expect("turn slot free");
            release_for_task.notified().await;
            // _guard is dropped here, signalling done_tx.
        });

        // Give the turn a moment to register.
        tokio::time::sleep(Duration::from_millis(20)).await;

        // cancel must NOT return until the spawned turn completes.
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled_flag = cancelled.clone();
        let agent_for_cancel = agent.clone();
        let cancel_task = tokio::spawn(async move {
            agent_for_cancel.cancel_for_test().await.unwrap();
            cancelled_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        // After 50ms, cancel should still be waiting.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !cancelled.load(std::sync::atomic::Ordering::SeqCst),
            "cancel returned before the in-flight turn dropped"
        );

        // Release the fake turn.
        release.notify_one();
        turn.await.unwrap();
        cancel_task.await.unwrap();
        assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn runtime_can_emit_error_and_finish() {
        let agent = AionrsAgentManager::new("conv-err".into(), "/project".into(), make_test_config(), None)
            .await
            .unwrap();
        let mut rx = IAgentConnector::subscribe_legacy(&agent);

        agent.runtime.emit_error("test error");
        // emit_error sets status to Finished, so emit_finish is a no-op here.
        // We emit directly for the Finish broadcast path test:
        agent
            .runtime
            .emit(AgentStreamEvent::Finish(crate::protocol::events::FinishEventData {
                session_id: None,
            }));

        match rx.try_recv().unwrap() {
            AgentStreamEvent::Error(data) => assert_eq!(data.message, "test error"),
            other => panic!("Expected Error, got {:?}", other),
        }
        match rx.try_recv().unwrap() {
            AgentStreamEvent::Finish(_) => {}
            other => panic!("Expected Finish, got {:?}", other),
        }
    }
}
