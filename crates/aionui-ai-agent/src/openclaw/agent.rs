use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use aionui_common::{AgentKillReason, AgentType, AppError, Confirmation, ConversationStatus, TimestampMs, now_ms};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::{debug, error, info, warn};

use crate::agent_manager::approval_key;
use crate::cli_process::CliAgentProcess;
use crate::stream_event::AgentStreamEvent;
use crate::types::SendMessageData;
use aionui_api_types::{OpenClawBuildExtra, OpenClawGatewayConfig};

use super::config::load_openclaw_config;
use super::connection::{AuthConfig, OpenClawConnection};
use super::device_identity::load_or_create_identity;
use super::event_mapper::{TextFallbackState, map_openclaw_event};
use super::protocol::{
    ChatAbortParams, ChatSendParams, SessionsResetParams, SessionsResetResponse, SessionsResolveParams,
    SessionsResolveResponse, normalize_ws_url,
};

use aionui_common::{CommandSpec, EnvVar};

pub const DEFAULT_GATEWAY_PORT: u16 = 18789;

const OPENCLAW_KILL_GRACE_MS: u64 = 1000;
const GATEWAY_READY_TIMEOUT: Duration = Duration::from_secs(10);
const GATEWAY_READY_POLL_INTERVAL: Duration = Duration::from_millis(200);
const STOP_FINISH_FALLBACK_TIMEOUT: Duration = Duration::from_secs(5);

struct OpenClawState {
    status: Option<ConversationStatus>,
    session_key: Option<String>,
    confirmations: Vec<Confirmation>,
    has_messages: bool,
    approval_memory: HashMap<String, bool>,
}

pub struct OpenClawAgentManager {
    conversation_id: String,
    workspace: String,
    config: OpenClawBuildExtra,
    gateway_process: Option<Arc<CliAgentProcess>>,
    connection: Arc<OpenClawConnection>,
    event_tx: broadcast::Sender<AgentStreamEvent>,
    state: Arc<RwLock<OpenClawState>>,
    last_activity: AtomicI64,
    text_state: Mutex<TextFallbackState>,
}

impl OpenClawAgentManager {
    pub async fn new(
        conversation_id: String,
        workspace: String,
        config: OpenClawBuildExtra,
        resume_session_key: Option<String>,
    ) -> Result<Self, AppError> {
        let file_config = load_openclaw_config();

        let host = config.gateway.host.as_deref().unwrap_or("127.0.0.1");
        let port = config
            .gateway
            .port
            .or_else(|| {
                file_config
                    .as_ref()
                    .and_then(|c| c.gateway.as_ref())
                    .and_then(|g| g.port)
            })
            .unwrap_or(DEFAULT_GATEWAY_PORT);

        let gateway_process = if !config.gateway.use_external_gateway {
            let cli_path = config
                .gateway
                .cli_path
                .as_deref()
                .ok_or_else(|| AppError::BadRequest("OpenClaw CLI path is required".into()))?;

            if !is_port_listening(host, port).await {
                let spawn_config = build_spawn_config(cli_path, &workspace, &config.gateway);
                let process = CliAgentProcess::spawn(spawn_config).await?;
                let process = Arc::new(process);

                wait_for_gateway_ready(host, port).await?;

                info!(
                    conversation_id = %conversation_id,
                    port = port,
                    "OpenClaw gateway subprocess ready"
                );

                Some(process)
            } else {
                debug!(port = port, "OpenClaw gateway already listening, skipping spawn");
                None
            }
        } else {
            None
        };

        let ws_url = normalize_ws_url(host, port);

        let identity = load_or_create_identity(None)?;

        let token = config
            .gateway
            .token
            .clone()
            .or_else(|| super::config::get_gateway_auth_token(file_config.as_ref()))
            .or_else(|| {
                super::device_auth_store::load_device_auth_token(&identity.device_id, "operator").map(|e| e.token)
            });
        let password = config
            .gateway
            .password
            .clone()
            .or_else(|| super::config::get_gateway_auth_password(file_config.as_ref()));

        let auth = if token.is_some() || password.is_some() {
            Some(AuthConfig { token, password })
        } else {
            None
        };

        let (connection, hello) = OpenClawConnection::connect(&ws_url, auth, &identity)
            .await
            .map_err(|e| {
                error!(
                    conversation_id = %conversation_id,
                    url = %ws_url,
                    error = %e,
                    "Failed to connect to OpenClaw gateway"
                );
                e
            })?;

        if let Some(ref auth_info) = hello.auth
            && let Some(ref device_token) = auth_info.device_token
        {
            super::device_auth_store::store_device_auth_token(
                &identity.device_id,
                auth_info.role.as_deref().unwrap_or("operator"),
                device_token,
                auth_info.scopes.as_deref().unwrap_or(&[]),
            );
        }

        info!(
            conversation_id = %conversation_id,
            url = %ws_url,
            "Connected to OpenClaw gateway via WebSocket"
        );

        let (event_tx, _) = broadcast::channel(256);

        let has_resume_key = resume_session_key.is_some();
        if has_resume_key {
            info!(
                conversation_id = %conversation_id,
                "Resuming OpenClaw session with stored session key"
            );
        }

        let manager = Self {
            conversation_id,
            workspace,
            config,
            gateway_process,
            connection: Arc::clone(&connection),
            event_tx: event_tx.clone(),
            state: Arc::new(RwLock::new(OpenClawState {
                status: None,
                session_key: resume_session_key,
                confirmations: Vec::new(),
                has_messages: has_resume_key,
                approval_memory: HashMap::new(),
            })),
            last_activity: AtomicI64::new(now_ms()),
            text_state: Mutex::new(TextFallbackState::new()),
        };

        Ok(manager)
    }

    pub fn start_event_relay(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.run_event_relay().await;
        });
    }

    async fn run_event_relay(self: Arc<Self>) {
        let mut event_rx = self.connection.subscribe_events();

        loop {
            match event_rx.recv().await {
                Ok(event_frame) => {
                    self.last_activity.store(now_ms(), Ordering::Relaxed);

                    let session_key = self.state.read().await.session_key.clone();

                    let stream_events = {
                        let mut text_state = self.text_state.lock().await;
                        map_openclaw_event(&event_frame, &mut text_state, session_key.as_deref())
                    };

                    for stream_event in stream_events {
                        self.update_state_from_event(&stream_event).await;
                        let _ = self.event_tx.send(stream_event);
                    }
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
                        "OpenClaw event channel closed"
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
                if let Some(conf) = data.as_confirmation() {
                    let mut guard = self.state.write().await;
                    if let Some(existing) = guard.confirmations.iter_mut().find(|c| c.call_id == conf.call_id) {
                        *existing = conf;
                    } else {
                        guard.confirmations.push(conf);
                    }
                }
            }
            _ => {}
        }
    }

    async fn do_send_message(&self, is_first: bool, data: SendMessageData) -> Result<(), AppError> {
        if is_first {
            self.resolve_session().await?;
        }

        let session_key = self
            .state
            .read()
            .await
            .session_key
            .clone()
            .ok_or_else(|| AppError::Internal("No active session key".into()))?;

        let params = ChatSendParams {
            session_key,
            message: data.content,
            idempotency_key: uuid::Uuid::new_v4().to_string(),
            attachments: if data.files.is_empty() {
                None
            } else {
                Some(data.files.into_iter().map(|f| json!(f)).collect())
            },
        };

        self.connection
            .request::<Value>("chat.send", serde_json::to_value(params).unwrap_or_default())
            .await?;

        Ok(())
    }

    /// Resolve gateway session: try to resume an existing session first,
    /// then fall back to creating a new one via sessions.reset.
    async fn resolve_session(&self) -> Result<(), AppError> {
        let resume_key = self.state.read().await.session_key.clone();

        if let Some(ref key) = resume_key {
            match self
                .connection
                .request::<SessionsResolveResponse>(
                    "sessions.resolve",
                    serde_json::to_value(SessionsResolveParams { key: key.clone() }).unwrap_or_default(),
                )
                .await
            {
                Ok(resp) => {
                    let mut state = self.state.write().await;
                    state.session_key = Some(resp.key.clone());
                    info!(
                        conversation_id = %self.conversation_id,
                        session_key = %resp.key,
                        "Resumed OpenClaw session via sessions.resolve"
                    );
                    return Ok(());
                }
                Err(e) => {
                    warn!(
                        conversation_id = %self.conversation_id,
                        error = %e,
                        "Failed to resume OpenClaw session, falling back to sessions.reset"
                    );
                }
            }
        }

        let resp: SessionsResetResponse = self
            .connection
            .request(
                "sessions.reset",
                serde_json::to_value(SessionsResetParams {
                    key: self.conversation_id.clone(),
                    reason: "new".into(),
                })
                .unwrap_or_default(),
            )
            .await?;

        if let Some(ref key) = resp.key {
            let mut state = self.state.write().await;
            state.session_key = Some(key.clone());
        }

        Ok(())
    }

    pub async fn get_diagnostics(&self) -> Value {
        let state = self.state.read().await;
        let host = self.config.gateway.host.as_deref().unwrap_or("127.0.0.1");
        let port = self.config.gateway.port.unwrap_or(DEFAULT_GATEWAY_PORT);

        json!({
            "workspace": self.workspace,
            "backend": serde_json::to_value(&self.config.backend).unwrap_or_default(),
            "agentName": self.config.agent_name,
            "cliPath": self.config.gateway.cli_path,
            "gatewayHost": host,
            "gatewayPort": port,
            "conversationId": self.conversation_id,
            "isConnected": self.connection.is_connected(),
            "hasActiveSession": state.session_key.is_some(),
            "sessionKey": state.session_key,
        })
    }
}

#[async_trait::async_trait]
impl crate::agent_task::IAgentTask for OpenClawAgentManager {
    fn agent_type(&self) -> AgentType {
        AgentType::OpenclawGateway
    }

    fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    fn workspace(&self) -> &str {
        &self.workspace
    }

    fn status(&self) -> Option<ConversationStatus> {
        self.state.try_read().ok().and_then(|g| g.status)
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

        {
            let mut text_state = self.text_state.lock().await;
            text_state.reset_for_new_turn();
        }

        let result = self.do_send_message(is_first, data).await;
        if let Err(ref e) = result {
            error!(
                conversation_id = %self.conversation_id,
                error = %e,
                "OpenClaw send_message failed, emitting Error+Finish"
            );
            let _ = self
                .event_tx
                .send(AgentStreamEvent::Error(crate::stream_event::ErrorEventData {
                    message: format!("OpenClaw send failed: {e}"),
                    code: None,
                }));
            let _ = self
                .event_tx
                .send(AgentStreamEvent::Finish(crate::stream_event::FinishEventData {
                    session_id: None,
                }));
        }
        result
    }

    async fn stop(&self) -> Result<(), AppError> {
        let session_key = self.state.read().await.session_key.clone();
        if let Some(ref key) = session_key {
            let params = ChatAbortParams {
                session_key: key.clone(),
                run_id: None,
            };
            let _ = self
                .connection
                .request::<Value>("chat.abort", serde_json::to_value(params).unwrap_or_default())
                .await;
        }

        {
            let mut state = self.state.write().await;
            state.confirmations.clear();
        }

        let state = Arc::clone(&self.state);
        let event_tx = self.event_tx.clone();
        let conversation_id = self.conversation_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(STOP_FINISH_FALLBACK_TIMEOUT).await;
            let needs_fallback = state
                .read()
                .await
                .status
                .map(|s| s == ConversationStatus::Running)
                .unwrap_or(false);
            if needs_fallback {
                warn!(
                    conversation_id = %conversation_id,
                    "Gateway did not send abort event within timeout, emitting fallback Finish"
                );
                let _ = event_tx.send(AgentStreamEvent::Error(crate::stream_event::ErrorEventData {
                    message: "Stopped by user".into(),
                    code: None,
                }));
                let _ = event_tx.send(AgentStreamEvent::Finish(crate::stream_event::FinishEventData {
                    session_id: None,
                }));
            }
        });

        Ok(())
    }

    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        info!(
            conversation_id = %self.conversation_id,
            ?reason,
            "Killing OpenClaw agent"
        );

        let connection = Arc::clone(&self.connection);
        tokio::spawn(async move {
            connection.close().await;
        });

        if let Some(ref process) = self.gateway_process {
            let process = Arc::clone(process);
            let grace = Duration::from_millis(OPENCLAW_KILL_GRACE_MS);
            tokio::spawn(async move {
                if let Err(e) = process.kill(grace).await {
                    error!(error = %e, "Failed to kill OpenClaw gateway process");
                }
            });
        }

        Ok(())
    }
}

/// OpenClaw-specific operations reached through `AgentInstance::OpenClaw(..)`
/// matches in the routes + services (e.g. `persist_session_key` uses
/// `get_session_key`, and `get_openclaw_runtime` calls `get_diagnostics`).
impl OpenClawAgentManager {
    pub fn confirm(&self, _msg_id: &str, call_id: &str, _data: Value, always_allow: bool) -> Result<(), AppError> {
        if let Ok(mut state) = self.state.try_write() {
            if always_allow && let Some(conf) = state.confirmations.iter().find(|c| c.call_id == call_id) {
                let key = approval_key(conf.action.as_deref(), conf.command_type.as_deref());
                state.approval_memory.insert(key, true);
            }
            state.confirmations.retain(|c| c.call_id != call_id);
        }

        let connection = Arc::clone(&self.connection);
        let call_id = call_id.to_owned();
        let option_id = if always_allow { "allow_always" } else { "allow_once" };
        let option_id = option_id.to_owned();
        tokio::spawn(async move {
            let params = json!({
                "requestId": call_id,
                "optionId": option_id,
            });
            if let Err(e) = connection.request::<Value>("exec.approval.respond", params).await {
                warn!(error = %e, "Failed to send OpenClaw approval response");
            }
        });

        Ok(())
    }

    pub fn get_confirmations(&self) -> Vec<Confirmation> {
        self.state
            .try_read()
            .map(|g| g.confirmations.clone())
            .unwrap_or_default()
    }

    pub fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool {
        self.state
            .try_read()
            .map(|g| {
                let key = approval_key(Some(action), command_type);
                g.approval_memory.get(&key).copied().unwrap_or(false)
            })
            .unwrap_or(false)
    }

    pub fn get_session_key(&self) -> Option<String> {
        self.state.try_read().ok().and_then(|g| g.session_key.clone())
    }
}

fn build_spawn_config(cli_path: &str, workspace: &str, gateway: &OpenClawGatewayConfig) -> CommandSpec {
    let host = gateway.host.as_deref().unwrap_or("127.0.0.1");
    let port = gateway.port.unwrap_or(DEFAULT_GATEWAY_PORT);

    let mut env = vec![
        EnvVar {
            name: "OPENCLAW_GATEWAY_HOST".into(),
            value: host.to_owned(),
        },
        EnvVar {
            name: "OPENCLAW_GATEWAY_PORT".into(),
            value: port.to_string(),
        },
    ];

    if let Some(ref token) = gateway.token {
        env.push(EnvVar {
            name: "OPENCLAW_GATEWAY_TOKEN".into(),
            value: token.clone(),
        });
    }
    if let Some(ref password) = gateway.password {
        env.push(EnvVar {
            name: "OPENCLAW_GATEWAY_PASSWORD".into(),
            value: password.clone(),
        });
    }

    CommandSpec {
        command: cli_path.into(),
        args: vec!["gateway".into(), "--port".into(), port.to_string()],
        env,
        cwd: Some(workspace.to_owned()),
    }
}

async fn is_port_listening(host: &str, port: u16) -> bool {
    tokio::net::TcpStream::connect((host, port)).await.is_ok()
}

async fn wait_for_gateway_ready(host: &str, port: u16) -> Result<(), AppError> {
    let start = tokio::time::Instant::now();
    while start.elapsed() < GATEWAY_READY_TIMEOUT {
        if is_port_listening(host, port).await {
            return Ok(());
        }
        tokio::time::sleep(GATEWAY_READY_POLL_INTERVAL).await;
    }
    Err(AppError::Internal(format!(
        "OpenClaw gateway did not become ready on {host}:{port} within {}s",
        GATEWAY_READY_TIMEOUT.as_secs()
    )))
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
        let config = build_spawn_config("/usr/bin/openclaw", "/proj", &gateway);
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
        let config = build_spawn_config("/usr/bin/openclaw", "/proj", &gateway);
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
