use std::sync::Arc;
use std::time::Duration;

use aionui_api_types::ConversationStatus;
use aionui_common::{AgentKillReason, AgentType, AppError, Confirmation, ErrorChain, TimestampMs};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::{debug, error, info, warn};

use aionui_common::CommandSpec;

use crate::agent_runtime::AgentRuntime;
use crate::capability::cli_process::CliAgentProcess;
use crate::protocol::events::AgentStreamEvent;
use crate::types::SendMessageData;
use std::path::PathBuf;

/// Grace period before force-killing a Nanobot process (ms).
const NANOBOT_KILL_GRACE_MS: u64 = 500;

/// Internal mutable state for the Nanobot agent.
struct NanobotState {
    has_messages: bool,
}

/// Manages a Nanobot CLI agent subprocess.
///
/// Nanobot is the simplest agent type:
/// - CLI blocking mode (fire-and-forget)
/// - No YOLO mode support
/// - No confirmation system
/// - Single response stream only
pub struct NanobotAgentManager {
    runtime: AgentRuntime,
    process: Arc<CliAgentProcess>,
    state: RwLock<NanobotState>,
    raw_rx: Mutex<Option<broadcast::Receiver<Value>>>,
}

impl NanobotAgentManager {
    /// Create a new Nanobot agent by spawning the CLI subprocess.
    pub async fn new(conversation_id: String, workspace: String, cli_path: PathBuf) -> Result<Self, AppError> {
        let spawn_config = Self::build_spawn_config(cli_path, &workspace);
        let process = CliAgentProcess::spawn(spawn_config).await?;

        let raw_rx = process
            .take_initial_receiver()
            .expect("Initial receiver should be available immediately after spawn");
        let runtime = AgentRuntime::new(conversation_id, workspace, 256);

        Ok(Self {
            runtime,
            process: Arc::new(process),
            state: RwLock::new(NanobotState { has_messages: false }),
            raw_rx: Mutex::new(Some(raw_rx)),
        })
    }

    fn build_spawn_config(cli_path: PathBuf, workspace: &str) -> CommandSpec {
        CommandSpec {
            command: cli_path,
            args: vec![],
            env: vec![],
            cwd: Some(workspace.to_owned()),
        }
    }

    /// Start the event relay (call after wrapping in Arc).
    pub fn start_relay(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.run_event_relay().await;
        });
    }

    async fn run_event_relay(self: Arc<Self>) {
        let mut raw_rx = {
            let mut guard = self.raw_rx.lock().await;
            match guard.take() {
                Some(rx) => rx,
                None => {
                    warn!(
                        conversation_id = %self.runtime.conversation_id(),
                        "Nanobot event relay already started"
                    );
                    return;
                }
            }
        };

        loop {
            match raw_rx.recv().await {
                Ok(raw_json) => {
                    self.runtime.bump_activity();
                    self.handle_raw_event(raw_json).await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        conversation_id = %self.runtime.conversation_id(),
                        lagged = n,
                        "Nanobot event relay lagged"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!(
                        conversation_id = %self.runtime.conversation_id(),
                        "Nanobot CLI event channel closed"
                    );
                    break;
                }
            }
        }

        // Channel closed without a Finish/Error event from the subprocess;
        // ensure the status reaches a terminal state.
        if self.runtime.status() == Some(ConversationStatus::Running) {
            self.runtime.transition_to(ConversationStatus::Finished);
        }
    }

    async fn handle_raw_event(&self, raw: Value) {
        let stream_event = match serde_json::from_value::<AgentStreamEvent>(raw.clone()) {
            Ok(event) => event,
            Err(_) => {
                debug!(
                    conversation_id = %self.runtime.conversation_id(),
                    "Unrecognized Nanobot event, skipping"
                );
                return;
            }
        };

        self.update_state_from_event(&stream_event);
        self.runtime.emit(stream_event);
    }

    fn update_state_from_event(&self, event: &AgentStreamEvent) {
        match event {
            AgentStreamEvent::Start(_) => {
                self.runtime.transition_to(ConversationStatus::Running);
            }
            AgentStreamEvent::Finish(_) | AgentStreamEvent::Error(_) => {
                self.runtime.transition_to(ConversationStatus::Finished);
            }
            _ => {}
        }
    }
}

#[async_trait::async_trait]
impl crate::agent_task::IAgentTask for NanobotAgentManager {
    fn status(&self) -> Option<ConversationStatus> {
        self.runtime.status()
    }

    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError> {
        self.runtime.bump_activity();

        {
            let mut state = self.state.write().await;
            state.has_messages = true;
        }
        self.runtime.transition_to(ConversationStatus::Running);

        // Nanobot uses fire-and-forget: send the message, CLI blocks until complete
        let payload = json!({
            "type": "send.message",
            "data": {
                "content": data.content,
                "msgId": data.msg_id,
            }
        });

        self.process.send(&payload).await
    }

    async fn cancel(&self) -> Result<(), AppError> {
        let payload = json!({ "type": "stop.stream", "data": {} });
        self.process.send(&payload).await
    }

    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        info!(
            conversation_id = %self.runtime.conversation_id(),
            ?reason,
            "Killing Nanobot agent"
        );

        let process = Arc::clone(&self.process);
        let grace = Duration::from_millis(NANOBOT_KILL_GRACE_MS);
        tokio::spawn(async move {
            if let Err(e) = process.kill(grace).await {
                error!(error = %ErrorChain(&e), "Failed to kill Nanobot process");
            }
        });

        Ok(())
    }
}

impl NanobotAgentManager {
    pub fn kill_and_wait(
        &self,
        reason: Option<AgentKillReason>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let _ = crate::agent_task::IAgentTask::kill(self, reason);
        let process = Arc::clone(&self.process);
        let grace = Duration::from_millis(NANOBOT_KILL_GRACE_MS);
        Box::pin(async move {
            let _ = process.kill(grace).await;
        })
    }
}

/// Nanobot-specific operations reached through `AgentInstance::Nanobot(..)`.
/// Nanobot does not track tool confirmations or approval memory, so these
/// are trivial stubs matching the semantics of the removed `IAgentManager`
/// default impls.
impl NanobotAgentManager {
    pub fn confirm(&self, _msg_id: &str, _call_id: &str, _data: Value, _always_allow: bool) -> Result<(), AppError> {
        Err(AppError::BadRequest("Nanobot does not support confirmations".into()))
    }

    pub fn get_confirmations(&self) -> Vec<Confirmation> {
        Vec::new()
    }

    pub fn check_approval(&self, _action: &str, _command_type: Option<&str>) -> bool {
        false
    }
}

// ── IAgentConnector impl ────────────────────────────────────────────────
//
// Delegates to the crate-private `IAgentTask` impl on `Self` or to the
// inherent helpers above.
#[async_trait::async_trait]
impl crate::connector::IAgentConnector for NanobotAgentManager {
    fn agent_type(&self) -> AgentType {
        AgentType::Nanobot
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

    /// Nanobot is a fire-and-forget CLI: as long as the manager exists
    /// the subprocess is alive. Mirrors the pattern used by Aionrs.
    fn is_open(&self) -> bool {
        true
    }

    async fn open(&self) -> Result<(), crate::connector::ConnectorError> {
        // Nanobot opens implicitly on construction; nothing to do here.
        Ok(())
    }

    fn close(&self, reason: Option<AgentKillReason>) {
        let _ = crate::agent_task::IAgentTask::kill(self, reason);
    }

    async fn run_turn(
        &self,
        msg: SendMessageData,
    ) -> Result<crate::connector::TurnSummary, crate::connector::ConnectorError> {
        match crate::agent_task::IAgentTask::send_message(self, msg).await {
            Ok(()) => Ok(crate::connector::TurnSummary {
                session_id: None,
                stop_reason: Some(crate::connector::StopReason::EndTurn),
            }),
            Err(AppError::Conflict(_)) => Err(crate::connector::ConnectorError::Busy),
            Err(e) => Err(crate::connector::ConnectorError::Protocol(format!("{e}"))),
        }
    }

    async fn cancel_current_turn(&self) -> Result<(), crate::connector::ConnectorError> {
        match crate::agent_task::IAgentTask::cancel(self).await {
            Ok(()) => Ok(()),
            Err(AppError::Conflict(_)) => Ok(()), // No turn in flight.
            Err(e) => Err(crate::connector::ConnectorError::Protocol(format!("{e}"))),
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<crate::connector::ConnectorEvent> {
        let (tx, rx) = broadcast::channel(64);
        let mut legacy = self.runtime.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = legacy.recv().await {
                let _ = tx.send(crate::connector::ConnectorEvent::Chunk(
                    crate::connector::ChunkPayload { event: ev },
                ));
            }
        });
        rx
    }

    fn subscribe_legacy(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.runtime.subscribe()
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
        NanobotAgentManager::kill_and_wait(self, reason)
    }

    fn get_confirmations(&self) -> Vec<Confirmation> {
        NanobotAgentManager::get_confirmations(self)
    }

    fn confirm(
        &self,
        msg_id: &str,
        call_id: &str,
        data: serde_json::Value,
        always_allow: bool,
    ) -> Result<(), AppError> {
        NanobotAgentManager::confirm(self, msg_id, call_id, data, always_allow)
    }

    fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool {
        NanobotAgentManager::check_approval(self, action, command_type)
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
        Err(AppError::BadRequest(
            "Mode switching is not supported for this agent type".into(),
        ))
    }

    async fn get_model(&self) -> Result<aionui_api_types::GetModelInfoResponse, AppError> {
        Ok(aionui_api_types::GetModelInfoResponse { model_info: None })
    }

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
        Ok(Vec::new())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_spawn_config_basic() {
        let config = NanobotAgentManager::build_spawn_config(PathBuf::from("/usr/bin/nanobot"), "/project");
        assert_eq!(config.command.to_str().unwrap(), "/usr/bin/nanobot");
        assert_eq!(config.cwd, Some("/project".into()));
        assert!(config.args.is_empty());
        assert!(config.env.is_empty());
    }
}
