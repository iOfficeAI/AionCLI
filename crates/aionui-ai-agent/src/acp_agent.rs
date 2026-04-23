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

use crate::cli_process::{CliAgentProcess, CliSpawnConfig};
use crate::stream_event::AgentStreamEvent;
use crate::types::{AcpBuildExtra, AcpModelInfo, SendMessageData};

/// Grace period before force-killing an ACP process (ms).
const ACP_KILL_GRACE_MS: u64 = 500;

/// ACP protocol command types sent to the CLI subprocess via stdin.
#[allow(dead_code)]
mod protocol {
    pub const SESSION_NEW: &str = "session/new";
    pub const SESSION_LOAD: &str = "session/load";
    pub const SESSION_CANCEL: &str = "session/cancel";
    pub const SEND_MESSAGE: &str = "sendMessage";
    pub const CONFIRM_MESSAGE: &str = "confirmMessage";
    pub const ENABLE_YOLO: &str = "session/setMode";
    pub const GET_MODE: &str = "session/getMode";
    pub const SET_MODE: &str = "session/setMode";
    pub const GET_MODEL_INFO: &str = "session/getModelInfo";
    pub const SET_MODEL: &str = "session/setModel";
    pub const GET_CONFIG_OPTIONS: &str = "session/getConfigOptions";
    pub const SET_CONFIG_OPTION: &str = "session/setConfigOption";
    pub const GET_SLASH_COMMANDS: &str = "session/getSlashCommands";
}

/// Session resume strategy varies by ACP backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionResumeStrategy {
    /// Use `session/load` command (Codex).
    SessionLoad,
    /// Use `session/new` with `_meta.claudeCode.options.resume` (Claude/CodeBuddy).
    ClaudeResumeMeta,
    /// Use `session/new` with `resumeSessionId` (all others).
    ResumeSessionId,
}

impl SessionResumeStrategy {
    fn for_backend(backend: AcpBackend) -> Self {
        match backend {
            AcpBackend::Codex => Self::SessionLoad,
            AcpBackend::Claude | AcpBackend::Codebuddy => Self::ClaudeResumeMeta,
            _ => Self::ResumeSessionId,
        }
    }
}

/// YOLO mode value for each ACP backend.
/// Returns `None` for backends that don't support YOLO.
fn yolo_mode_value(backend: AcpBackend) -> Option<&'static str> {
    match backend {
        AcpBackend::Claude | AcpBackend::Codebuddy => Some("bypassPermissions"),
        AcpBackend::Qwen | AcpBackend::IFlow => Some("yolo"),
        _ => None,
    }
}

/// Internal state that changes at runtime.
struct AcpState {
    /// Current conversation status.
    status: Option<ConversationStatus>,
    /// Active session ID (set after session/new or session/load).
    session_id: Option<String>,
    /// Pending tool-call confirmations.
    confirmations: Vec<Confirmation>,
    /// Model info from ACP backend.
    model_info: Option<AcpModelInfo>,
    /// Whether this session has sent at least one message.
    has_messages: bool,
    /// Session-level approval memory (action key → always allowed).
    /// Cleared when the agent is killed, not persisted.
    approval_memory: HashMap<String, bool>,
}

/// Inject optional `files` and `injectSkills` into a JSON payload's `data` object.
fn inject_files_and_skills(payload: &mut Value, data: &SendMessageData) {
    if !data.files.is_empty() {
        payload["data"]["files"] = json!(data.files);
    }
    if !data.inject_skills.is_empty() {
        payload["data"]["injectSkills"] = json!(data.inject_skills);
    }
}

use crate::agent_manager::approval_key;

/// Manages a single ACP Agent instance.
///
/// ACP is the most complex agent type, supporting 20+ CLI sub-backends
/// (Claude, Qwen, CodeBuddy, Codex, etc.). Communication happens via
/// JSON-over-stdin/stdout with the underlying CLI process.
pub struct AcpAgentManager {
    /// Conversation this agent is bound to.
    conversation_id: String,
    /// Working directory.
    workspace: String,
    /// ACP sub-backend.
    backend: AcpBackend,
    /// Build configuration.
    config: AcpBuildExtra,
    /// Underlying CLI process.
    process: Arc<CliAgentProcess>,
    /// Typed event broadcast channel.
    event_tx: broadcast::Sender<AgentStreamEvent>,
    /// Mutable runtime state.
    state: RwLock<AcpState>,
    /// Timestamp of last activity (atomic for lock-free reads).
    last_activity: AtomicI64,
    /// Mutex for serializing session operations (new/load/send).
    session_lock: Mutex<()>,
    /// Pre-subscribed receiver from the CLI process, consumed by the relay.
    /// Wrapped in Mutex<Option<>> so it can be taken exactly once.
    raw_rx: Mutex<Option<broadcast::Receiver<serde_json::Value>>>,
}

impl AcpAgentManager {
    /// Create a new ACP agent manager by spawning a CLI subprocess.
    pub async fn new(
        conversation_id: String,
        workspace: String,
        config: AcpBuildExtra,
    ) -> Result<Self, AppError> {
        let backend = config
            .backend
            .ok_or_else(|| AppError::BadRequest("ACP backend is required".into()))?;
        let cli_path = config
            .cli_path
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("CLI path is required for ACP agent".into()))?;

        let spawn_config = Self::build_spawn_config(cli_path, &workspace, &config);
        let process = CliAgentProcess::spawn(spawn_config).await?;

        // Take the pre-subscribed receiver (created before background tasks start)
        // to guarantee no events are lost.
        let raw_rx = process
            .take_initial_receiver()
            .expect("Initial receiver should be available immediately after spawn");
        let (event_tx, _) = broadcast::channel(256);

        let manager = Self {
            conversation_id,
            workspace,
            backend,
            config,
            process: Arc::new(process),
            event_tx,
            state: RwLock::new(AcpState {
                status: None,
                session_id: None,
                confirmations: Vec::new(),
                model_info: None,
                has_messages: false,
                approval_memory: HashMap::new(),
            }),
            last_activity: AtomicI64::new(now_ms()),
            session_lock: Mutex::new(()),
            raw_rx: Mutex::new(Some(raw_rx)),
        };

        Ok(manager)
    }

    /// Build the CLI spawn config from ACP build options.
    fn build_spawn_config(
        cli_path: &str,
        workspace: &str,
        config: &AcpBuildExtra,
    ) -> CliSpawnConfig {
        let mut args = Vec::new();

        // ACP CLI expects a specific set of arguments depending on the backend
        if let Some(ref agent_name) = config.agent_name {
            args.push("--agent".into());
            args.push(agent_name.clone());
        }

        if let Some(ref custom_id) = config.custom_agent_id {
            args.push("--custom-agent-id".into());
            args.push(custom_id.clone());
        }

        CliSpawnConfig {
            command: cli_path.to_owned(),
            args,
            env: std::collections::HashMap::new(),
            cwd: Some(workspace.to_owned()),
        }
    }

    /// Start the event relay. Must be called after the manager is wrapped in Arc.
    ///
    /// Takes the pre-subscribed receiver from construction, ensuring no events
    /// are lost between process spawn and relay start.
    pub fn start_relay(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.run_event_relay().await;
        });
    }

    /// Run the event relay loop: reads raw JSON from CLI stdout,
    /// parses into `AgentStreamEvent`, updates internal state, and
    /// broadcasts to subscribers.
    async fn run_event_relay(self: Arc<Self>) {
        // Take the pre-subscribed receiver (subscribed before any events were emitted)
        let mut raw_rx = {
            let mut guard = self.raw_rx.lock().await;
            match guard.take() {
                Some(rx) => rx,
                None => {
                    warn!(
                        conversation_id = %self.conversation_id,
                        "Event relay already started or receiver missing"
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
                        "Event relay lagged, some events dropped"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!(
                        conversation_id = %self.conversation_id,
                        "CLI process event channel closed, stopping relay"
                    );
                    break;
                }
            }
        }

        // Process ended — mark status as Finished
        let mut state = self.state.write().await;
        if state.status == Some(ConversationStatus::Running) {
            state.status = Some(ConversationStatus::Finished);
        }
    }

    /// Parse a raw JSON event from the CLI process and handle it.
    async fn handle_raw_event(&self, raw: Value) {
        let event_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");

        let stream_event = match Self::parse_stream_event(&raw) {
            Some(event) => event,
            None => {
                debug!(
                    conversation_id = %self.conversation_id,
                    event_type,
                    "Unrecognized ACP event type, skipping"
                );
                return;
            }
        };

        // Update internal state based on event type
        self.update_state_from_event(&stream_event).await;

        // Broadcast to subscribers (ignore send error if no subscribers)
        let _ = self.event_tx.send(stream_event);
    }

    /// Parse a raw JSON value into an `AgentStreamEvent`.
    ///
    /// ACP events come in the format `{ "type": "<event_type>", "data": {...} }`.
    fn parse_stream_event(raw: &Value) -> Option<AgentStreamEvent> {
        // Try direct deserialization first — our AgentStreamEvent uses
        // the same tagged format as ACP protocol
        serde_json::from_value::<AgentStreamEvent>(raw.clone()).ok()
    }

    /// Update internal state based on a parsed stream event.
    async fn update_state_from_event(&self, event: &AgentStreamEvent) {
        match event {
            AgentStreamEvent::Start(data) => {
                let mut state = self.state.write().await;
                state.status = Some(ConversationStatus::Running);
                if let Some(ref sid) = data.session_id {
                    state.session_id = Some(sid.clone());
                }
            }
            AgentStreamEvent::Finish(data) => {
                let mut state = self.state.write().await;
                state.status = Some(ConversationStatus::Finished);
                if let Some(ref sid) = data.session_id {
                    state.session_id = Some(sid.clone());
                }
            }
            AgentStreamEvent::Error(_) => {
                let mut state = self.state.write().await;
                state.status = Some(ConversationStatus::Finished);
            }
            AgentStreamEvent::AgentStatus(data) => {
                let mut state = self.state.write().await;
                if let Some(ref sid) = data.session_id {
                    state.session_id = Some(sid.clone());
                }
            }
            AgentStreamEvent::AcpModelInfo(data) => {
                if let Ok(info) = serde_json::from_value::<AcpModelInfo>(data.clone()) {
                    let mut state = self.state.write().await;
                    state.model_info = Some(info);
                }
            }
            AgentStreamEvent::AcpPermission(data) => {
                // Parse as Confirmation and add to pending list
                if let Ok(conf) = serde_json::from_value::<Confirmation>(data.clone()) {
                    self.add_confirmation(conf).await;
                } else {
                    debug!(
                        conversation_id = %self.conversation_id,
                        "Failed to parse AcpPermission as Confirmation"
                    );
                }
            }
            _ => {}
        }
    }

    /// Initialize or resume a session, then send the user message.
    async fn ensure_session_and_send(&self, data: &SendMessageData) -> Result<(), AppError> {
        let _lock = self.session_lock.lock().await;

        let state = self.state.read().await;
        let has_session = state.session_id.is_some();
        let has_messages = state.has_messages;
        drop(state);

        if !has_session && !has_messages {
            // First message — create new session
            self.session_new(data).await?;
        } else if has_session && has_messages {
            // Existing session — resume and send
            self.session_resume_and_send(data).await?;
        } else {
            // Session exists but no previous messages (warmup case)
            self.send_message_to_process(data).await?;
        }

        let mut state = self.state.write().await;
        state.has_messages = true;
        state.status = Some(ConversationStatus::Running);

        Ok(())
    }

    /// Create a new ACP session.
    async fn session_new(&self, data: &SendMessageData) -> Result<(), AppError> {
        let mut payload = json!({
            "type": protocol::SESSION_NEW,
            "data": {
                "message": data.content,
                "msgId": data.msg_id,
            }
        });

        // Inject files if present
        if !data.files.is_empty() {
            payload["data"]["files"] = serde_json::to_value(&data.files)
                .map_err(|e| AppError::Internal(format!("Failed to serialize files: {e}")))?;
        }

        // Inject skills if present
        if !data.inject_skills.is_empty() {
            payload["data"]["injectSkills"] = serde_json::to_value(&data.inject_skills)
                .map_err(|e| AppError::Internal(format!("Failed to serialize skills: {e}")))?;
        }

        // Inject preset context if configured
        if let Some(ref context) = self.config.preset_context {
            payload["data"]["presetContext"] = json!(context);
        }

        // Inject enabled skills from config
        if !self.config.enabled_skills.is_empty() {
            payload["data"]["enabledSkills"] = serde_json::to_value(&self.config.enabled_skills)
                .map_err(|e| {
                    AppError::Internal(format!("Failed to serialize enabled skills: {e}"))
                })?;
        }

        // Inject session mode if configured
        if let Some(ref mode) = self.config.session_mode {
            payload["data"]["sessionMode"] = json!(mode);
        }

        self.process.send(&payload).await
    }

    /// Resume an existing session and send a message.
    async fn session_resume_and_send(&self, data: &SendMessageData) -> Result<(), AppError> {
        let session_id = self.state.read().await.session_id.clone();
        let strategy = SessionResumeStrategy::for_backend(self.backend);

        // Codex: session/load first, then sendMessage
        if strategy == SessionResumeStrategy::SessionLoad {
            if let Some(ref sid) = session_id {
                let load = json!({ "type": protocol::SESSION_LOAD, "data": { "sessionId": sid } });
                self.process.send(&load).await?;
            }
            return self.send_message_to_process(data).await;
        }

        // Claude/CodeBuddy and others: session/new with resume info
        let mut payload = json!({
            "type": protocol::SESSION_NEW,
            "data": { "message": data.content, "msgId": data.msg_id }
        });

        match strategy {
            SessionResumeStrategy::ClaudeResumeMeta => {
                let mut opts = json!({ "resume": true });
                if let Some(ref sid) = session_id {
                    opts["sessionId"] = json!(sid);
                }
                payload["data"]["_meta"] = json!({ "claudeCode": { "options": opts } });
            }
            SessionResumeStrategy::ResumeSessionId => {
                if let Some(ref sid) = session_id {
                    payload["data"]["resumeSessionId"] = json!(sid);
                }
            }
            SessionResumeStrategy::SessionLoad => unreachable!(),
        }

        inject_files_and_skills(&mut payload, data);
        self.process.send(&payload).await
    }

    /// Send a message to the CLI process (for use after session is already established).
    async fn send_message_to_process(&self, data: &SendMessageData) -> Result<(), AppError> {
        let mut payload = json!({
            "type": protocol::SEND_MESSAGE,
            "data": { "content": data.content, "msgId": data.msg_id }
        });
        inject_files_and_skills(&mut payload, data);
        self.process.send(&payload).await
    }

    /// Enable YOLO mode for the current session if the backend supports it.
    pub async fn ensure_yolo_mode(&self) -> bool {
        let mode = match yolo_mode_value(self.backend) {
            Some(m) => m,
            None => return false,
        };

        let payload = json!({
            "type": protocol::ENABLE_YOLO,
            "data": {
                "mode": mode,
            }
        });

        match self.process.send(&payload).await {
            Ok(()) => {
                debug!(
                    conversation_id = %self.conversation_id,
                    backend = ?self.backend,
                    mode,
                    "YOLO mode enabled"
                );
                true
            }
            Err(e) => {
                warn!(
                    conversation_id = %self.conversation_id,
                    error = %e,
                    "Failed to enable YOLO mode"
                );
                false
            }
        }
    }

    // -- ACP-specific extended methods (beyond IAgentManager) --

    /// Get the current session mode.
    pub async fn get_mode(&self) -> Result<Value, AppError> {
        let payload = json!({ "type": protocol::GET_MODE, "data": {} });
        self.process.send(&payload).await?;
        // The response comes through the event stream.
        // Caller should subscribe and wait for the response event.
        Ok(json!({ "sent": true }))
    }

    /// Set the session mode.
    pub async fn set_mode(&self, mode: &str) -> Result<(), AppError> {
        let payload = json!({
            "type": protocol::SET_MODE,
            "data": { "mode": mode }
        });
        self.process.send(&payload).await
    }

    /// Get model info from the ACP backend.
    pub async fn get_model_info(&self) -> Option<AcpModelInfo> {
        let state = self.state.read().await;
        state.model_info.clone()
    }

    /// Set the model for the current session.
    pub async fn set_model(&self, model_id: &str) -> Result<(), AppError> {
        let payload = json!({
            "type": protocol::SET_MODEL,
            "data": { "modelId": model_id }
        });
        self.process.send(&payload).await
    }

    /// Get the session configuration options.
    pub async fn get_config_options(&self) -> Result<(), AppError> {
        let payload = json!({ "type": protocol::GET_CONFIG_OPTIONS, "data": {} });
        self.process.send(&payload).await
    }

    /// Set a session configuration option.
    pub async fn set_config_option(&self, config_id: &str, value: &str) -> Result<(), AppError> {
        let payload = json!({
            "type": protocol::SET_CONFIG_OPTION,
            "data": { "configId": config_id, "value": value }
        });
        self.process.send(&payload).await
    }

    /// Load available slash commands from the ACP backend.
    pub async fn load_slash_commands(&self) -> Result<(), AppError> {
        let payload = json!({ "type": protocol::GET_SLASH_COMMANDS, "data": {} });
        self.process.send(&payload).await
    }

    /// Get the session ID.
    pub async fn session_id(&self) -> Option<String> {
        let state = self.state.read().await;
        state.session_id.clone()
    }

    /// Get the ACP backend type.
    pub fn backend(&self) -> AcpBackend {
        self.backend
    }
}

#[async_trait::async_trait]
impl crate::agent_manager::IAgentManager for AcpAgentManager {
    fn agent_type(&self) -> AgentType {
        AgentType::Acp
    }

    fn status(&self) -> Option<ConversationStatus> {
        // Use try_read to avoid blocking; fall back to None if locked
        match self.state.try_read() {
            Ok(guard) => guard.status,
            Err(_) => None,
        }
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
        self.ensure_session_and_send(&data).await
    }

    async fn stop(&self) -> Result<(), AppError> {
        let payload = json!({
            "type": protocol::SESSION_CANCEL,
            "data": {}
        });
        self.process.send(&payload).await?;

        // Clear pending confirmations on stop
        let mut state = self.state.write().await;
        state.confirmations.clear();

        Ok(())
    }

    fn confirm(
        &self,
        msg_id: &str,
        call_id: &str,
        data: serde_json::Value,
        always_allow: bool,
    ) -> Result<(), AppError> {
        let payload = json!({
            "type": protocol::CONFIRM_MESSAGE,
            "data": {
                "confirmKey": msg_id,
                "callId": call_id,
                "data": data,
            }
        });

        // Remove the confirmation from the pending list and optionally
        // record in approval memory.
        if let Ok(mut state) = self.state.try_write() {
            if always_allow {
                // Find the confirmation before removing it to read action/command_type
                if let Some(conf) = state.confirmations.iter().find(|c| c.call_id == call_id) {
                    let key = approval_key(conf.action.as_deref(), conf.command_type.as_deref());
                    state.approval_memory.insert(key, true);
                }
            }
            state.confirmations.retain(|c| c.call_id != call_id);
        }

        // Send confirmation to the CLI process.
        // Use tokio::spawn to avoid blocking the current thread.
        let process = Arc::clone(&self.process);
        tokio::spawn(async move {
            if let Err(e) = process.send(&payload).await {
                error!(error = %e, "Failed to send confirmation to ACP process");
            }
        });

        Ok(())
    }

    fn get_confirmations(&self) -> Vec<Confirmation> {
        match self.state.try_read() {
            Ok(guard) => guard.confirmations.clone(),
            Err(_) => Vec::new(),
        }
    }

    fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool {
        match self.state.try_read() {
            Ok(guard) => {
                let key = approval_key(Some(action), command_type);
                guard.approval_memory.get(&key).copied().unwrap_or(false)
            }
            Err(_) => false,
        }
    }

    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        info!(
            conversation_id = %self.conversation_id,
            ?reason,
            "Killing ACP agent"
        );

        let process = Arc::clone(&self.process);
        let grace = Duration::from_millis(ACP_KILL_GRACE_MS);

        tokio::spawn(async move {
            if let Err(e) = process.kill(grace).await {
                error!(error = %e, "Failed to kill ACP process");
            }
        });

        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl AcpAgentManager {
    /// Add a confirmation to the pending list.
    ///
    /// Replaces an existing confirmation with the same `call_id`, or appends.
    pub async fn add_confirmation(&self, confirmation: Confirmation) {
        let mut guard = self.state.write().await;
        if let Some(existing) = guard
            .confirmations
            .iter_mut()
            .find(|c| c.call_id == confirmation.call_id)
        {
            *existing = confirmation;
        } else {
            guard.confirmations.push(confirmation);
        }
    }

    /// Remove a confirmation by `call_id`.
    pub async fn remove_confirmation(&self, call_id: &str) -> Option<Confirmation> {
        let mut guard = self.state.write().await;
        let pos = guard
            .confirmations
            .iter()
            .position(|c| c.call_id == call_id);
        pos.map(|i| guard.confirmations.remove(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_resume_strategy_for_backends() {
        assert_eq!(
            SessionResumeStrategy::for_backend(AcpBackend::Codex),
            SessionResumeStrategy::SessionLoad
        );
        assert_eq!(
            SessionResumeStrategy::for_backend(AcpBackend::Claude),
            SessionResumeStrategy::ClaudeResumeMeta
        );
        assert_eq!(
            SessionResumeStrategy::for_backend(AcpBackend::Codebuddy),
            SessionResumeStrategy::ClaudeResumeMeta
        );
        assert_eq!(
            SessionResumeStrategy::for_backend(AcpBackend::Qwen),
            SessionResumeStrategy::ResumeSessionId
        );
        assert_eq!(
            SessionResumeStrategy::for_backend(AcpBackend::Kiro),
            SessionResumeStrategy::ResumeSessionId
        );
    }

    #[test]
    fn yolo_mode_for_backends() {
        assert_eq!(
            yolo_mode_value(AcpBackend::Claude),
            Some("bypassPermissions")
        );
        assert_eq!(
            yolo_mode_value(AcpBackend::Codebuddy),
            Some("bypassPermissions")
        );
        assert_eq!(yolo_mode_value(AcpBackend::Qwen), Some("yolo"));
        assert_eq!(yolo_mode_value(AcpBackend::IFlow), Some("yolo"));
        assert_eq!(yolo_mode_value(AcpBackend::Kiro), None);
        assert_eq!(yolo_mode_value(AcpBackend::Gemini), None);
        assert_eq!(yolo_mode_value(AcpBackend::Custom), None);
    }

    #[test]
    fn build_spawn_config_basic() {
        let config = AcpBuildExtra {
            agent_id: None,
            backend: Some(AcpBackend::Claude),
            cli_path: Some("/usr/bin/claude".into()),
            custom_workspace: false,
            agent_name: None,
            custom_agent_id: None,
            preset_context: None,
            enabled_skills: vec![],
            preset_assistant_id: None,
            session_mode: None,
            cron_job_id: None,
        };
        let spawn = AcpAgentManager::build_spawn_config("/usr/bin/claude", "/project", &config);
        assert_eq!(spawn.command, "/usr/bin/claude");
        assert!(spawn.args.is_empty());
        assert_eq!(spawn.cwd, Some("/project".into()));
    }

    #[test]
    fn build_spawn_config_with_agent_name() {
        let config = AcpBuildExtra {
            agent_id: None,
            backend: Some(AcpBackend::Claude),
            cli_path: Some("/usr/bin/claude".into()),
            custom_workspace: false,
            agent_name: Some("security-reviewer".into()),
            custom_agent_id: None,
            preset_context: None,
            enabled_skills: vec![],
            preset_assistant_id: None,
            session_mode: None,
            cron_job_id: None,
        };
        let spawn = AcpAgentManager::build_spawn_config("/usr/bin/claude", "/project", &config);
        assert_eq!(spawn.args, vec!["--agent", "security-reviewer"]);
    }

    #[test]
    fn build_spawn_config_with_custom_agent_id() {
        let config = AcpBuildExtra {
            agent_id: None,
            backend: Some(AcpBackend::Custom),
            cli_path: Some("/usr/bin/custom-agent".into()),
            custom_workspace: true,
            agent_name: None,
            custom_agent_id: Some("my-agent-123".into()),
            preset_context: None,
            enabled_skills: vec![],
            preset_assistant_id: None,
            session_mode: None,
            cron_job_id: None,
        };
        let spawn =
            AcpAgentManager::build_spawn_config("/usr/bin/custom-agent", "/custom/path", &config);
        assert_eq!(spawn.args, vec!["--custom-agent-id", "my-agent-123"]);
    }

    #[test]
    fn parse_stream_event_text() {
        let raw = json!({
            "type": "text",
            "data": { "content": "Hello world" }
        });
        let event = AcpAgentManager::parse_stream_event(&raw);
        assert!(event.is_some());
        if let Some(AgentStreamEvent::Text(data)) = event {
            assert_eq!(data.content, "Hello world");
        } else {
            panic!("Expected Text event");
        }
    }

    #[test]
    fn parse_stream_event_finish() {
        let raw = json!({
            "type": "finish",
            "data": { "session_id": "sess-123" }
        });
        let event = AcpAgentManager::parse_stream_event(&raw);
        assert!(event.is_some());
        if let Some(AgentStreamEvent::Finish(data)) = event {
            assert_eq!(data.session_id, Some("sess-123".into()));
        } else {
            panic!("Expected Finish event");
        }
    }

    #[test]
    fn parse_stream_event_error() {
        let raw = json!({
            "type": "error",
            "data": { "message": "timeout", "code": "E001" }
        });
        let event = AcpAgentManager::parse_stream_event(&raw);
        assert!(event.is_some());
        if let Some(AgentStreamEvent::Error(data)) = event {
            assert_eq!(data.message, "timeout");
            assert_eq!(data.code, Some("E001".into()));
        } else {
            panic!("Expected Error event");
        }
    }

    #[test]
    fn parse_stream_event_start() {
        let raw = json!({
            "type": "start",
            "data": { "session_id": "sess-abc" }
        });
        let event = AcpAgentManager::parse_stream_event(&raw);
        assert!(event.is_some());
        if let Some(AgentStreamEvent::Start(data)) = event {
            assert_eq!(data.session_id, Some("sess-abc".into()));
        } else {
            panic!("Expected Start event");
        }
    }

    #[test]
    fn parse_stream_event_thinking() {
        let raw = json!({
            "type": "thinking",
            "data": {
                "content": "Analyzing code...",
                "subject": "security",
                "duration": 2000,
                "status": "in_progress"
            }
        });
        let event = AcpAgentManager::parse_stream_event(&raw);
        assert!(event.is_some());
        if let Some(AgentStreamEvent::Thinking(data)) = event {
            assert_eq!(data.content, "Analyzing code...");
            assert_eq!(data.duration, Some(2000));
        } else {
            panic!("Expected Thinking event");
        }
    }

    #[test]
    fn parse_stream_event_agent_status() {
        let raw = json!({
            "type": "agent_status",
            "data": {
                "backend": "claude",
                "status": "running",
                "agent_name": "default",
                "session_id": "sess-xyz"
            }
        });
        let event = AcpAgentManager::parse_stream_event(&raw);
        assert!(event.is_some());
        if let Some(AgentStreamEvent::AgentStatus(data)) = event {
            assert_eq!(data.backend, "claude");
            assert_eq!(data.session_id, Some("sess-xyz".into()));
        } else {
            panic!("Expected AgentStatus event");
        }
    }

    #[test]
    fn parse_stream_event_unknown_type_returns_none() {
        let raw = json!({
            "type": "unknown_event_type",
            "data": {}
        });
        let event = AcpAgentManager::parse_stream_event(&raw);
        assert!(event.is_none());
    }

    #[test]
    fn parse_stream_event_acp_permission() {
        let raw = json!({
            "type": "acp_permission",
            "data": {
                "call_id": "call-1",
                "action": "edit_file",
                "description": "Edit main.rs"
            }
        });
        let event = AcpAgentManager::parse_stream_event(&raw);
        assert!(event.is_some());
        matches!(event.unwrap(), AgentStreamEvent::AcpPermission(_));
    }

    #[test]
    fn parse_stream_event_tool_call() {
        let raw = json!({
            "type": "tool_call",
            "data": {
                "call_id": "tc-1",
                "name": "read_file",
                "args": { "path": "/tmp/test.rs" },
                "status": "running"
            }
        });
        let event = AcpAgentManager::parse_stream_event(&raw);
        assert!(event.is_some());
        if let Some(AgentStreamEvent::ToolCall(data)) = event {
            assert_eq!(data.call_id, "tc-1");
            assert_eq!(data.name, "read_file");
        } else {
            panic!("Expected ToolCall event");
        }
    }

    #[test]
    fn parse_stream_event_plan() {
        let raw = json!({
            "type": "plan",
            "data": {
                "session_id": "sess-1",
                "entries": [{ "step": 1, "description": "Read file" }]
            }
        });
        let event = AcpAgentManager::parse_stream_event(&raw);
        assert!(event.is_some());
        if let Some(AgentStreamEvent::Plan(data)) = event {
            assert_eq!(data.session_id, Some("sess-1".into()));
            assert_eq!(data.entries.len(), 1);
        } else {
            panic!("Expected Plan event");
        }
    }

    #[tokio::test]
    async fn add_confirmation_inserts_new() {
        let holder = make_confirmation_holder();

        let confirmation = Confirmation {
            id: "c1".into(),
            call_id: "call-1".into(),
            title: Some("Test".into()),
            action: Some("edit_file".into()),
            description: "Edit main.rs".into(),
            command_type: None,
            options: vec![],
        };
        holder.add_confirmation(confirmation).await;

        let guard = holder.state.read().await;
        assert_eq!(guard.confirmations.len(), 1);
        assert_eq!(guard.confirmations[0].call_id, "call-1");
    }

    #[tokio::test]
    async fn add_confirmation_replaces_existing() {
        let holder = ConfirmationHolder {
            state: RwLock::new(AcpState {
                status: None,
                session_id: None,
                confirmations: vec![Confirmation {
                    id: "c1".into(),
                    call_id: "call-1".into(),
                    title: Some("Old".into()),
                    action: None,
                    description: "Old desc".into(),
                    command_type: None,
                    options: vec![],
                }],
                model_info: None,
                has_messages: false,
                approval_memory: HashMap::new(),
            }),
        };

        let updated = Confirmation {
            id: "c1-v2".into(),
            call_id: "call-1".into(),
            title: Some("Updated".into()),
            action: Some("edit_file".into()),
            description: "New desc".into(),
            command_type: None,
            options: vec![],
        };
        holder.add_confirmation(updated).await;

        let guard = holder.state.read().await;
        assert_eq!(guard.confirmations.len(), 1);
        assert_eq!(guard.confirmations[0].title, Some("Updated".into()));
        assert_eq!(guard.confirmations[0].description, "New desc");
    }

    #[tokio::test]
    async fn remove_confirmation_by_call_id() {
        let holder = ConfirmationHolder {
            state: RwLock::new(AcpState {
                status: None,
                session_id: None,
                confirmations: vec![
                    Confirmation {
                        id: "c1".into(),
                        call_id: "call-1".into(),
                        title: Some("First".into()),
                        action: None,
                        description: "desc1".into(),
                        command_type: None,
                        options: vec![],
                    },
                    Confirmation {
                        id: "c2".into(),
                        call_id: "call-2".into(),
                        title: Some("Second".into()),
                        action: None,
                        description: "desc2".into(),
                        command_type: None,
                        options: vec![],
                    },
                ],
                model_info: None,
                has_messages: false,
                approval_memory: HashMap::new(),
            }),
        };

        let removed = holder.remove_confirmation("call-1").await;
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().call_id, "call-1");

        let guard = holder.state.read().await;
        assert_eq!(guard.confirmations.len(), 1);
        assert_eq!(guard.confirmations[0].call_id, "call-2");
    }

    #[tokio::test]
    async fn remove_confirmation_nonexistent_returns_none() {
        let holder = make_confirmation_holder();
        let removed = holder.remove_confirmation("nonexistent").await;
        assert!(removed.is_none());
    }

    #[tokio::test]
    async fn update_state_from_start_event() {
        let manager = make_test_state();
        let event = AgentStreamEvent::Start(crate::stream_event::StartEventData {
            session_id: Some("sess-abc".into()),
        });
        manager.update_state_from_event(&event).await;

        let state = manager.state.read().await;
        assert_eq!(state.status, Some(ConversationStatus::Running));
        assert_eq!(state.session_id, Some("sess-abc".into()));
    }

    #[tokio::test]
    async fn update_state_from_finish_event() {
        let manager = make_test_state();
        // First set to running
        {
            let mut state = manager.state.write().await;
            state.status = Some(ConversationStatus::Running);
        }

        let event = AgentStreamEvent::Finish(crate::stream_event::FinishEventData {
            session_id: Some("sess-abc".into()),
        });
        manager.update_state_from_event(&event).await;

        let state = manager.state.read().await;
        assert_eq!(state.status, Some(ConversationStatus::Finished));
        assert_eq!(state.session_id, Some("sess-abc".into()));
    }

    #[tokio::test]
    async fn update_state_from_error_event() {
        let manager = make_test_state();
        {
            let mut state = manager.state.write().await;
            state.status = Some(ConversationStatus::Running);
        }

        let event = AgentStreamEvent::Error(crate::stream_event::ErrorEventData {
            message: "timeout".into(),
            code: None,
        });
        manager.update_state_from_event(&event).await;

        let state = manager.state.read().await;
        assert_eq!(state.status, Some(ConversationStatus::Finished));
    }

    #[tokio::test]
    async fn update_state_from_model_info_event() {
        let manager = make_test_state();
        let event = AgentStreamEvent::AcpModelInfo(json!({
            "model_id": "claude-sonnet-4",
            "model_name": "Claude Sonnet 4",
            "provider": "anthropic"
        }));
        manager.update_state_from_event(&event).await;

        let state = manager.state.read().await;
        let info = state.model_info.as_ref().unwrap();
        assert_eq!(info.model_id, "claude-sonnet-4");
        assert_eq!(info.model_name, Some("Claude Sonnet 4".into()));
    }

    // ── approval_key tests ────────────────────────────────────────────

    #[test]
    fn approval_key_with_action_and_command_type() {
        assert_eq!(
            approval_key(Some("edit_file"), Some("bash")),
            "edit_file:bash"
        );
    }

    #[test]
    fn approval_key_with_action_only() {
        assert_eq!(approval_key(Some("edit_file"), None), "edit_file");
    }

    #[test]
    fn approval_key_with_no_action() {
        assert_eq!(approval_key(None, Some("bash")), "");
        assert_eq!(approval_key(None, None), "");
    }

    // ── approval memory tests ────────────────────────────────────────

    #[test]
    fn confirm_with_always_allow_stores_approval() {
        let state = RwLock::new(AcpState {
            status: None,
            session_id: None,
            confirmations: vec![Confirmation {
                id: "c1".into(),
                call_id: "call-1".into(),
                title: Some("Allow edit".into()),
                action: Some("edit_file".into()),
                description: "Edit main.rs".into(),
                command_type: Some("bash".into()),
                options: vec![],
            }],
            model_info: None,
            has_messages: false,
            approval_memory: HashMap::new(),
        });

        // Simulate the confirm logic with always_allow=true
        {
            let mut guard = state.try_write().unwrap();
            let call_id = "call-1";
            if let Some(conf) = guard.confirmations.iter().find(|c| c.call_id == call_id) {
                let key = approval_key(conf.action.as_deref(), conf.command_type.as_deref());
                guard.approval_memory.insert(key, true);
            }
            guard.confirmations.retain(|c| c.call_id != call_id);
        }

        let guard = state.try_read().unwrap();
        assert!(guard.confirmations.is_empty());
        assert_eq!(guard.approval_memory.get("edit_file:bash"), Some(&true));
    }

    #[test]
    fn confirm_without_always_allow_does_not_store_approval() {
        let state = RwLock::new(AcpState {
            status: None,
            session_id: None,
            confirmations: vec![Confirmation {
                id: "c1".into(),
                call_id: "call-1".into(),
                title: Some("Allow edit".into()),
                action: Some("edit_file".into()),
                description: "Edit main.rs".into(),
                command_type: None,
                options: vec![],
            }],
            model_info: None,
            has_messages: false,
            approval_memory: HashMap::new(),
        });

        // Simulate the confirm logic with always_allow=false
        {
            let mut guard = state.try_write().unwrap();
            // always_allow is false, so we skip the approval_memory insert
            guard.confirmations.retain(|c| c.call_id != "call-1");
        }

        let guard = state.try_read().unwrap();
        assert!(guard.confirmations.is_empty());
        assert!(guard.approval_memory.is_empty());
    }

    #[test]
    fn check_approval_returns_true_after_always_allow() {
        let state = RwLock::new(AcpState {
            status: None,
            session_id: None,
            confirmations: Vec::new(),
            model_info: None,
            has_messages: false,
            approval_memory: HashMap::from([
                ("edit_file:bash".into(), true),
                ("read_file".into(), true),
            ]),
        });

        let guard = state.try_read().unwrap();
        let key1 = approval_key(Some("edit_file"), Some("bash"));
        assert!(guard.approval_memory.get(&key1).copied().unwrap_or(false));

        let key2 = approval_key(Some("read_file"), None);
        assert!(guard.approval_memory.get(&key2).copied().unwrap_or(false));

        let key3 = approval_key(Some("delete_file"), None);
        assert!(!guard.approval_memory.get(&key3).copied().unwrap_or(false));
    }

    // ── AcpPermission event → add confirmation test ──────────────────

    #[tokio::test]
    async fn update_state_from_acp_permission_event() {
        let manager = make_test_state_with_permission();
        let event = AgentStreamEvent::AcpPermission(json!({
            "id": "c1",
            "call_id": "call-1",
            "title": "Allow file edit",
            "action": "edit_file",
            "description": "Edit main.rs",
            "command_type": "bash",
            "options": []
        }));
        manager.update_state_from_event(&event).await;

        let state = manager.state.read().await;
        assert_eq!(state.confirmations.len(), 1);
        assert_eq!(state.confirmations[0].call_id, "call-1");
        assert_eq!(state.confirmations[0].action, Some("edit_file".into()));
        assert_eq!(state.confirmations[0].command_type, Some("bash".into()));
    }

    #[tokio::test]
    async fn update_state_from_acp_permission_invalid_data() {
        let manager = make_test_state_with_permission();
        let event = AgentStreamEvent::AcpPermission(json!({
            "invalid": "data"
        }));
        manager.update_state_from_event(&event).await;

        let state = manager.state.read().await;
        assert!(state.confirmations.is_empty());
    }

    /// Helper for testing state update logic without spawning a real process.
    struct TestStateHolder {
        state: RwLock<AcpState>,
    }

    impl TestStateHolder {
        async fn update_state_from_event(&self, event: &AgentStreamEvent) {
            match event {
                AgentStreamEvent::Start(data) => {
                    let mut state = self.state.write().await;
                    state.status = Some(ConversationStatus::Running);
                    if let Some(ref sid) = data.session_id {
                        state.session_id = Some(sid.clone());
                    }
                }
                AgentStreamEvent::Finish(data) => {
                    let mut state = self.state.write().await;
                    state.status = Some(ConversationStatus::Finished);
                    if let Some(ref sid) = data.session_id {
                        state.session_id = Some(sid.clone());
                    }
                }
                AgentStreamEvent::Error(_) => {
                    let mut state = self.state.write().await;
                    state.status = Some(ConversationStatus::Finished);
                }
                AgentStreamEvent::AcpModelInfo(data) => {
                    if let Ok(info) = serde_json::from_value::<AcpModelInfo>(data.clone()) {
                        let mut state = self.state.write().await;
                        state.model_info = Some(info);
                    }
                }
                _ => {}
            }
        }
    }

    fn make_test_state() -> TestStateHolder {
        TestStateHolder {
            state: RwLock::new(AcpState {
                status: None,
                session_id: None,
                confirmations: Vec::new(),
                model_info: None,
                has_messages: false,
                approval_memory: HashMap::new(),
            }),
        }
    }

    /// Helper that also handles AcpPermission events (mirrors AcpAgentManager logic).
    struct TestStateHolderWithPermission {
        state: RwLock<AcpState>,
    }

    impl TestStateHolderWithPermission {
        async fn update_state_from_event(&self, event: &AgentStreamEvent) {
            match event {
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
    }

    fn make_test_state_with_permission() -> TestStateHolderWithPermission {
        TestStateHolderWithPermission {
            state: RwLock::new(AcpState {
                status: None,
                session_id: None,
                confirmations: Vec::new(),
                model_info: None,
                has_messages: false,
                approval_memory: HashMap::new(),
            }),
        }
    }

    /// Helper for testing confirmation management without a real process.
    struct ConfirmationHolder {
        state: RwLock<AcpState>,
    }

    impl ConfirmationHolder {
        async fn add_confirmation(&self, confirmation: Confirmation) {
            let mut guard = self.state.write().await;
            if let Some(existing) = guard
                .confirmations
                .iter_mut()
                .find(|c| c.call_id == confirmation.call_id)
            {
                *existing = confirmation;
            } else {
                guard.confirmations.push(confirmation);
            }
        }

        async fn remove_confirmation(&self, call_id: &str) -> Option<Confirmation> {
            let mut guard = self.state.write().await;
            let pos = guard
                .confirmations
                .iter()
                .position(|c| c.call_id == call_id);
            pos.map(|i| guard.confirmations.remove(i))
        }
    }

    fn make_confirmation_holder() -> ConfirmationHolder {
        ConfirmationHolder {
            state: RwLock::new(AcpState {
                status: None,
                session_id: None,
                confirmations: Vec::new(),
                model_info: None,
                has_messages: false,
                approval_memory: HashMap::new(),
            }),
        }
    }
}
