use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use aionui_common::{
    AgentKillReason, AgentType, AppError, Confirmation, ConversationStatus, TimestampMs, now_ms,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::{debug, error, info, warn};

use crate::agent_manager::IAgentManager;
use aionui_common::CommandSpec;

use crate::cli_process::CliAgentProcess;
use crate::stream_event::AgentStreamEvent;
use crate::types::SendMessageData;

/// Grace period before force-killing a Nanobot process (ms).
const NANOBOT_KILL_GRACE_MS: u64 = 500;

/// Internal mutable state for the Nanobot agent.
struct NanobotState {
    status: Option<ConversationStatus>,
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
    conversation_id: String,
    workspace: String,
    process: Arc<CliAgentProcess>,
    event_tx: broadcast::Sender<AgentStreamEvent>,
    state: RwLock<NanobotState>,
    last_activity: AtomicI64,
    raw_rx: Mutex<Option<broadcast::Receiver<Value>>>,
}

impl NanobotAgentManager {
    /// Create a new Nanobot agent by spawning the CLI subprocess.
    pub async fn new(
        conversation_id: String,
        workspace: String,
        cli_path: String,
    ) -> Result<Self, AppError> {
        let spawn_config = Self::build_spawn_config(&cli_path, &workspace);
        let process = CliAgentProcess::spawn(spawn_config).await?;

        let raw_rx = process
            .take_initial_receiver()
            .expect("Initial receiver should be available immediately after spawn");
        let (event_tx, _) = broadcast::channel(256);

        Ok(Self {
            conversation_id,
            workspace,
            process: Arc::new(process),
            event_tx,
            state: RwLock::new(NanobotState {
                status: None,
                has_messages: false,
            }),
            last_activity: AtomicI64::new(now_ms()),
            raw_rx: Mutex::new(Some(raw_rx)),
        })
    }

    fn build_spawn_config(cli_path: &str, workspace: &str) -> CommandSpec {
        CommandSpec {
            command: cli_path.into(),
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
                        conversation_id = %self.conversation_id,
                        "Nanobot event relay already started"
                    );
                    return;
                }
            }
        };

        loop {
            match raw_rx.recv().await {
                Ok(raw_json) => {
                    self.last_activity.store(now_ms(), Ordering::Relaxed);
                    self.handle_raw_event(raw_json).await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        conversation_id = %self.conversation_id,
                        lagged = n,
                        "Nanobot event relay lagged"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!(
                        conversation_id = %self.conversation_id,
                        "Nanobot CLI event channel closed"
                    );
                    break;
                }
            }
        }

        let mut state = self.state.write().await;
        if state.status == Some(ConversationStatus::Running) {
            state.status = Some(ConversationStatus::Finished);
        }
    }

    async fn handle_raw_event(&self, raw: Value) {
        let stream_event = match serde_json::from_value::<AgentStreamEvent>(raw.clone()) {
            Ok(event) => event,
            Err(_) => {
                debug!(
                    conversation_id = %self.conversation_id,
                    "Unrecognized Nanobot event, skipping"
                );
                return;
            }
        };

        self.update_state_from_event(&stream_event).await;
        let _ = self.event_tx.send(stream_event);
    }

    async fn update_state_from_event(&self, event: &AgentStreamEvent) {
        match event {
            AgentStreamEvent::Start(_) => {
                let mut state = self.state.write().await;
                state.status = Some(ConversationStatus::Running);
            }
            AgentStreamEvent::Finish(_) | AgentStreamEvent::Error(_) => {
                let mut state = self.state.write().await;
                state.status = Some(ConversationStatus::Finished);
            }
            _ => {}
        }
    }
}

#[async_trait::async_trait]
impl IAgentManager for NanobotAgentManager {
    fn agent_type(&self) -> AgentType {
        AgentType::Nanobot
    }

    fn status(&self) -> Option<ConversationStatus> {
        self.state.try_read().ok().and_then(|g| g.status)
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

    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError> {
        self.last_activity.store(now_ms(), Ordering::Relaxed);

        {
            let mut state = self.state.write().await;
            state.has_messages = true;
            state.status = Some(ConversationStatus::Running);
        }

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

    async fn stop(&self) -> Result<(), AppError> {
        let payload = json!({ "type": "stop.stream", "data": {} });
        self.process.send(&payload).await
    }

    /// Nanobot does not support confirmations.
    fn confirm(
        &self,
        _msg_id: &str,
        _call_id: &str,
        _data: Value,
        _always_allow: bool,
    ) -> Result<(), AppError> {
        Err(AppError::BadRequest(
            "Nanobot does not support confirmations".into(),
        ))
    }

    /// Nanobot has no pending confirmations.
    fn get_confirmations(&self) -> Vec<Confirmation> {
        Vec::new()
    }

    /// Nanobot does not support YOLO / approval memory.
    fn check_approval(&self, _action: &str, _command_type: Option<&str>) -> bool {
        false
    }

    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        info!(
            conversation_id = %self.conversation_id,
            ?reason,
            "Killing Nanobot agent"
        );

        let process = Arc::clone(&self.process);
        let grace = Duration::from_millis(NANOBOT_KILL_GRACE_MS);
        tokio::spawn(async move {
            if let Err(e) = process.kill(grace).await {
                error!(error = %e, "Failed to kill Nanobot process");
            }
        });

        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_spawn_config_basic() {
        let config = NanobotAgentManager::build_spawn_config("/usr/bin/nanobot", "/project");
        assert_eq!(config.command.to_str().unwrap(), "/usr/bin/nanobot");
        assert_eq!(config.cwd, Some("/project".into()));
        assert!(config.args.is_empty());
        assert!(config.env.is_empty());
    }
}
