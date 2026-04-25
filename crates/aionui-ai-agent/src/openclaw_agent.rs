use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use aionui_common::{
    AcpBackend, AgentKillReason, AgentType, AppError, Confirmation, ConversationStatus,
    TimestampMs, now_ms,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::{debug, error, info, warn};

use crate::agent_manager::IAgentManager;
use aionui_common::{CommandSpec, EnvVar};

use crate::cli_process::CliAgentProcess;
use crate::stream_event::AgentStreamEvent;
use crate::types::{OpenClawBuildExtra, OpenClawGatewayConfig, SendMessageData};

/// Default OpenClaw gateway port.
const DEFAULT_GATEWAY_PORT: u16 = 18789;

/// Grace period before force-killing an OpenClaw process (ms).
const OPENCLAW_KILL_GRACE_MS: u64 = 1000;

/// Internal mutable state for the OpenClaw agent.
struct OpenClawState {
    status: Option<ConversationStatus>,
    session_key: Option<String>,
    confirmations: Vec<Confirmation>,
    has_messages: bool,
    approval_memory: HashMap<String, bool>,
}

/// Manages an OpenClaw Gateway CLI agent subprocess.
///
/// OpenClaw agents connect to a local or external gateway via WebSocket.
/// The Rust implementation delegates to the OpenClaw CLI binary.
pub struct OpenClawAgentManager {
    conversation_id: String,
    workspace: String,
    backend: AcpBackend,
    config: OpenClawBuildExtra,
    process: Arc<CliAgentProcess>,
    event_tx: broadcast::Sender<AgentStreamEvent>,
    state: RwLock<OpenClawState>,
    last_activity: AtomicI64,
    raw_rx: Mutex<Option<broadcast::Receiver<Value>>>,
}

impl OpenClawAgentManager {
    /// Create a new OpenClaw agent by spawning the CLI subprocess.
    pub async fn new(
        conversation_id: String,
        workspace: String,
        config: OpenClawBuildExtra,
    ) -> Result<Self, AppError> {
        let cli_path = config
            .gateway
            .cli_path
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("OpenClaw CLI path is required".into()))?;

        let spawn_config = Self::build_spawn_config(cli_path, &workspace, &config.gateway);
        let process = CliAgentProcess::spawn(spawn_config).await?;

        let raw_rx = process
            .take_initial_receiver()
            .expect("Initial receiver should be available immediately after spawn");
        let (event_tx, _) = broadcast::channel(256);

        let backend = config.backend;

        Ok(Self {
            conversation_id,
            workspace,
            backend,
            config,
            process: Arc::new(process),
            event_tx,
            state: RwLock::new(OpenClawState {
                status: None,
                session_key: None,
                confirmations: Vec::new(),
                has_messages: false,
                approval_memory: HashMap::new(),
            }),
            last_activity: AtomicI64::new(now_ms()),
            raw_rx: Mutex::new(Some(raw_rx)),
        })
    }

    fn build_spawn_config(
        cli_path: &str,
        workspace: &str,
        gateway: &OpenClawGatewayConfig,
    ) -> CommandSpec {
        let host = gateway.host.as_deref().unwrap_or("127.0.0.1");
        let port = gateway.port.unwrap_or(DEFAULT_GATEWAY_PORT);

        let mut env = vec![
            EnvVar { name: "OPENCLAW_GATEWAY_HOST".into(), value: host.to_owned() },
            EnvVar { name: "OPENCLAW_GATEWAY_PORT".into(), value: port.to_string() },
        ];

        if let Some(ref token) = gateway.token {
            env.push(EnvVar { name: "OPENCLAW_GATEWAY_TOKEN".into(), value: token.clone() });
        }
        if let Some(ref password) = gateway.password {
            env.push(EnvVar { name: "OPENCLAW_GATEWAY_PASSWORD".into(), value: password.clone() });
        }

        CommandSpec {
            command: cli_path.into(),
            args: vec![],
            env,
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
                        "OpenClaw event relay already started"
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
                        "OpenClaw event relay lagged"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!(
                        conversation_id = %self.conversation_id,
                        "OpenClaw CLI event channel closed"
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
                    "Unrecognized OpenClaw event, skipping"
                );
                return;
            }
        };

        self.update_state_from_event(&stream_event).await;
        let _ = self.event_tx.send(stream_event);
    }

    async fn update_state_from_event(&self, event: &AgentStreamEvent) {
        match event {
            AgentStreamEvent::Start(data) => {
                let mut state = self.state.write().await;
                state.status = Some(ConversationStatus::Running);
                if let Some(ref sid) = data.session_id {
                    state.session_key = Some(sid.clone());
                }
            }
            AgentStreamEvent::Finish(data) => {
                let mut state = self.state.write().await;
                state.status = Some(ConversationStatus::Finished);
                if let Some(ref sid) = data.session_id {
                    state.session_key = Some(sid.clone());
                }
            }
            AgentStreamEvent::Error(_) => {
                let mut state = self.state.write().await;
                state.status = Some(ConversationStatus::Finished);
            }
            AgentStreamEvent::AcpPermission(data) => {
                if let Ok(conf) = serde_json::from_value::<Confirmation>(data.clone()) {
                    let mut guard = self.state.write().await;
                    if let Some(existing) = guard
                        .confirmations
                        .iter_mut()
                        .find(|c| c.call_id == conf.call_id)
                    {
                        *existing = conf;
                    } else {
                        guard.confirmations.push(conf);
                    }
                }
            }
            _ => {}
        }
    }

    /// Get diagnostic information about the OpenClaw runtime.
    pub async fn get_diagnostics(&self) -> Value {
        let state = self.state.read().await;
        let host = self.config.gateway.host.as_deref().unwrap_or("127.0.0.1");
        let port = self.config.gateway.port.unwrap_or(DEFAULT_GATEWAY_PORT);

        json!({
            "workspace": self.workspace,
            "backend": serde_json::to_value(self.backend).unwrap_or_default(),
            "agentName": self.config.agent_name,
            "cliPath": self.config.gateway.cli_path,
            "gatewayHost": host,
            "gatewayPort": port,
            "conversationId": self.conversation_id,
            "isConnected": self.process.is_running(),
            "hasActiveSession": state.session_key.is_some(),
            "sessionKey": state.session_key,
        })
    }
}

use crate::agent_manager::approval_key;

#[async_trait::async_trait]
impl IAgentManager for OpenClawAgentManager {
    fn agent_type(&self) -> AgentType {
        AgentType::OpenclawGateway
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

        let is_first = {
            let mut state = self.state.write().await;
            let first = !state.has_messages;
            state.has_messages = true;
            state.status = Some(ConversationStatus::Running);
            first
        };

        let mut payload = json!({
            "type": if is_first { "session/new" } else { "sendMessage" },
            "data": {
                "message": data.content,
                "msgId": data.msg_id,
            }
        });

        if !is_first {
            let session_key = self.state.read().await.session_key.clone();
            if let Some(ref key) = session_key {
                payload["data"]["sessionKey"] = json!(key);
            }
        }

        if !data.files.is_empty() {
            payload["data"]["files"] = json!(data.files);
        }

        self.process.send(&payload).await
    }

    async fn stop(&self) -> Result<(), AppError> {
        let payload = json!({ "type": "session/cancel", "data": {} });
        self.process.send(&payload).await?;

        let mut state = self.state.write().await;
        state.confirmations.clear();
        Ok(())
    }

    fn confirm(
        &self,
        _msg_id: &str,
        call_id: &str,
        data: Value,
        always_allow: bool,
    ) -> Result<(), AppError> {
        if let Ok(mut state) = self.state.try_write() {
            if always_allow
                && let Some(conf) = state.confirmations.iter().find(|c| c.call_id == call_id)
            {
                let key = approval_key(conf.action.as_deref(), conf.command_type.as_deref());
                state.approval_memory.insert(key, true);
            }
            state.confirmations.retain(|c| c.call_id != call_id);
        }

        let process = Arc::clone(&self.process);
        let call_id = call_id.to_owned();
        tokio::spawn(async move {
            let payload = json!({
                "type": "confirmMessage",
                "data": { "callId": call_id, "data": data }
            });
            if let Err(e) = process.send(&payload).await {
                error!(error = %e, "Failed to send OpenClaw confirmation");
            }
        });

        Ok(())
    }

    fn get_confirmations(&self) -> Vec<Confirmation> {
        self.state
            .try_read()
            .map(|g| g.confirmations.clone())
            .unwrap_or_default()
    }

    fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool {
        self.state
            .try_read()
            .map(|g| {
                let key = approval_key(Some(action), command_type);
                g.approval_memory.get(&key).copied().unwrap_or(false)
            })
            .unwrap_or(false)
    }

    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        info!(
            conversation_id = %self.conversation_id,
            ?reason,
            "Killing OpenClaw agent"
        );

        let process = Arc::clone(&self.process);
        let grace = Duration::from_millis(OPENCLAW_KILL_GRACE_MS);
        tokio::spawn(async move {
            if let Err(e) = process.kill(grace).await {
                error!(error = %e, "Failed to kill OpenClaw process");
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
    fn default_gateway_port_is_18789() {
        assert_eq!(DEFAULT_GATEWAY_PORT, 18789);
    }

    fn env_val<'a>(config: &'a CommandSpec, name: &str) -> Option<&'a str> {
        config.env.iter().find(|e| e.name == name).map(|e| e.value.as_str())
    }

    #[test]
    fn build_spawn_config_with_defaults() {
        let gateway = OpenClawGatewayConfig {
            host: None,
            port: None,
            token: None,
            password: None,
            use_external_gateway: false,
            cli_path: Some("/usr/bin/openclaw".into()),
        };
        let config =
            OpenClawAgentManager::build_spawn_config("/usr/bin/openclaw", "/proj", &gateway);
        assert_eq!(config.command.to_str().unwrap(), "/usr/bin/openclaw");
        assert_eq!(env_val(&config, "OPENCLAW_GATEWAY_HOST").unwrap(), "127.0.0.1");
        assert_eq!(env_val(&config, "OPENCLAW_GATEWAY_PORT").unwrap(), "18789");
        assert!(env_val(&config, "OPENCLAW_GATEWAY_TOKEN").is_none());
    }

    #[test]
    fn build_spawn_config_with_custom_gateway() {
        let gateway = OpenClawGatewayConfig {
            host: Some("remote.host".into()),
            port: Some(9999),
            token: Some("secret".into()),
            password: Some("pass".into()),
            use_external_gateway: true,
            cli_path: Some("/usr/bin/openclaw".into()),
        };
        let config =
            OpenClawAgentManager::build_spawn_config("/usr/bin/openclaw", "/proj", &gateway);
        assert_eq!(env_val(&config, "OPENCLAW_GATEWAY_HOST").unwrap(), "remote.host");
        assert_eq!(env_val(&config, "OPENCLAW_GATEWAY_PORT").unwrap(), "9999");
        assert_eq!(env_val(&config, "OPENCLAW_GATEWAY_TOKEN").unwrap(), "secret");
        assert_eq!(env_val(&config, "OPENCLAW_GATEWAY_PASSWORD").unwrap(), "pass");
    }

    #[test]
    fn approval_key_formats_correctly() {
        assert_eq!(approval_key(Some("edit"), Some("file")), "edit:file");
        assert_eq!(approval_key(Some("edit"), None), "edit");
        assert_eq!(approval_key(None, None), "");
    }
}
