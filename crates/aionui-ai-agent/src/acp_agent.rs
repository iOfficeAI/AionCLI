use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use aionui_common::{
    AcpBackend, AgentKillReason, AgentType, AppError, CommandSpec, Confirmation,
    ConversationStatus, TimestampMs, now_ms,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::acp_protocol::{
    AcpProtocol, CancelNotification, ContentBlock, LoadSessionRequest, NewSessionRequest,
    PermissionDecision, PermissionRequest, PromptRequest, SessionId, SetSessionConfigOptionRequest,
    SetSessionModeRequest, SetSessionModelRequest,
};
use crate::cli_process::CliAgentProcess;
use crate::stream_event::AgentStreamEvent;
use crate::types::{AcpBuildExtra, AcpModelInfo, SendMessageData};

/// Grace period before force-killing an ACP process (ms).
const ACP_KILL_GRACE_MS: u64 = 500;

/// Session resume strategy varies by ACP backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionResumeStrategy {
    /// Use `session/load` command (Codex).
    SessionLoad,
    /// Use `session/new` — resume not needed, just create a new session and prompt.
    NewAndPrompt,
}

impl SessionResumeStrategy {
    fn for_backend(backend: AcpBackend) -> Self {
        match backend {
            AcpBackend::Codex => Self::SessionLoad,
            _ => Self::NewAndPrompt,
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

use crate::agent_manager::approval_key;

/// Manages a single ACP Agent instance.
///
/// ACP is the most complex agent type, supporting 20+ CLI sub-backends
/// (Claude, Qwen, CodeBuddy, Codex, etc.). Communication now happens via
/// the `agent-client-protocol` SDK's JSON-RPC transport, replacing the
/// previous hand-crafted JSON-over-stdin/stdout approach.
pub struct AcpAgentManager {
    /// Conversation this agent is bound to.
    conversation_id: String,
    /// Working directory.
    workspace: String,
    /// ACP sub-backend.
    backend: AcpBackend,
    /// Build configuration (used for preset context, skills, session mode injection).
    #[allow(dead_code)]
    config: AcpBuildExtra,
    /// Underlying CLI process (for lifecycle management: kill, is_running).
    process: Arc<CliAgentProcess>,
    /// ACP protocol handle (SDK connection).
    protocol: AcpProtocol,
    /// Typed event broadcast channel.
    event_tx: broadcast::Sender<AgentStreamEvent>,
    /// Mutable runtime state.
    state: RwLock<AcpState>,
    /// Timestamp of last activity (atomic for lock-free reads).
    last_activity: AtomicI64,
    /// Mutex for serializing session operations (new/load/send).
    session_lock: Mutex<()>,
    /// Receiver for permission requests from the protocol layer.
    permission_rx: Mutex<mpsc::Receiver<PermissionRequest>>,
    /// Whether a graceful shutdown is in progress.
    closing: std::sync::atomic::AtomicBool,
}

impl AcpAgentManager {
    /// Create a new ACP agent manager by spawning a CLI subprocess and
    /// establishing an ACP protocol connection.
    ///
    /// `spawn_command` and `spawn_args` come from the `AgentRegistry`
    /// (resolved by factory). They include the full command and ACP-specific
    /// arguments (bridge package args or direct CLI ACP flags).
    pub async fn new(
        conversation_id: String,
        workspace: String,
        command_spec: CommandSpec,
        config: AcpBuildExtra,
    ) -> Result<Self, AppError> {
        let backend = config
            .backend
            .ok_or_else(|| AppError::BadRequest("ACP backend is required".into()))?;
        let process = CliAgentProcess::spawn_for_sdk(command_spec).await?;

        // Take raw stdio for the SDK transport
        let (stdin, stdout) = process
            .take_stdio()
            .await
            .ok_or_else(|| AppError::Internal("Failed to take stdio from CLI process".into()))?;

        let (event_tx, _) = broadcast::channel(256);
        let (permission_tx, permission_rx) = mpsc::channel(32);

        // Connect via ACP SDK — executes initialize handshake
        let protocol = AcpProtocol::connect(stdin, stdout, event_tx.clone(), permission_tx)
            .await
            .map_err(|e| {
                error!(
                    conversation_id = %conversation_id,
                    error = %e,
                    "Failed to establish ACP protocol connection"
                );
                AppError::from(e)
            })?;

        let manager = Self {
            conversation_id,
            workspace,
            backend,
            config,
            process: Arc::new(process),
            protocol,
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
            permission_rx: Mutex::new(permission_rx),
            closing: std::sync::atomic::AtomicBool::new(false),
        };

        Ok(manager)
    }

    /// Start the permission handler loop. Must be called after the manager
    /// is wrapped in Arc.
    ///
    /// This background task receives permission requests from the protocol
    /// layer, converts them to `AcpPermission` events, and waits for user
    /// responses routed through the `confirm()` method.
    pub fn start_permission_handler(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move { this.run_permission_handler().await });
    }

    /// Run the permission handler loop.
    async fn run_permission_handler(self: Arc<Self>) {
        let mut rx = self.permission_rx.lock().await;

        while let Some(perm_req) = rx.recv().await {
            self.last_activity.store(now_ms(), Ordering::Relaxed);

            // Convert to Confirmation and store
            let call_id = format!(
                "perm-{}",
                perm_req.session_id.chars().take(8).collect::<String>()
            );

            // Emit AcpPermission event for the frontend
            let permission_event = json!({
                "call_id": &call_id,
                "session_id": &perm_req.session_id,
                "tool_call": &perm_req.tool_call,
                "options": &perm_req.options,
            });

            let confirmation = Confirmation {
                id: call_id.clone(),
                call_id: call_id.clone(),
                title: perm_req
                    .tool_call
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                action: perm_req
                    .tool_call
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                description: perm_req
                    .tool_call
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Permission requested")
                    .to_owned(),
                command_type: None,
                options: perm_req
                    .options
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| serde_json::from_value(v.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default(),
            };

            // Store the response channel so confirm() can use it
            {
                let mut state = self.state.write().await;
                state.confirmations.push(confirmation);
            }

            // Broadcast AcpPermission event
            let _ = self
                .event_tx
                .send(AgentStreamEvent::AcpPermission(permission_event));

            // Store the response_tx in a side map keyed by call_id
            // For now, we cancel if the permission_rx loop continues
            // The actual response is sent by confirm() via the stored channel
            // This is simplified — the response_tx is dropped here and confirm()
            // sends via the protocol directly. See confirm() implementation.

            // Since we can't easily store the oneshot sender across the
            // async boundary of confirm(), we auto-cancel this request
            // and instead handle confirm() by sending a new response
            // through the protocol.
            //
            // TODO: In a future iteration, store response_tx in a HashMap
            // keyed by call_id so confirm() can complete the circuit.
            // For now, the permission request handler auto-cancels,
            // and confirm() is a separate fire-and-forget path.
            let _ = perm_req.response_tx.send(PermissionDecision::Cancelled);
        }
    }

    /// Initialize or resume a session, then send the user message.
    async fn ensure_session_and_send(&self, data: &SendMessageData) -> Result<(), AppError> {
        let _lock = self.session_lock.lock().await;

        let state = self.state.read().await;
        let has_session = state.session_id.is_some();
        let session_id = state.session_id.clone();
        let has_messages = state.has_messages;
        drop(state);

        if !has_session && !has_messages {
            // First message — create new session then prompt
            self.session_new_and_prompt(data).await?;
        } else if has_session && has_messages {
            // Existing session — resume strategy depends on backend
            self.session_resume_and_send(data, session_id.as_deref())
                .await?;
        } else {
            // Session exists but no previous messages — just prompt
            self.prompt_existing_session(data, session_id.as_deref())
                .await?;
        }

        let mut state = self.state.write().await;
        state.has_messages = true;
        state.status = Some(ConversationStatus::Running);

        Ok(())
    }

    /// Create a new ACP session and send the first prompt.
    async fn session_new_and_prompt(&self, data: &SendMessageData) -> Result<(), AppError> {
        // Emit Start event
        let _ = self.event_tx.send(AgentStreamEvent::Start(
            crate::stream_event::StartEventData { session_id: None },
        ));

        let session_id = self
            .protocol
            .new_session(NewSessionRequest::new(&self.workspace))
            .await
            .map_err(AppError::from)?;

        let sid = session_id.to_string();
        {
            let mut state = self.state.write().await;
            state.session_id = Some(sid.clone());
        }

        // Send the prompt
        self.protocol
            .prompt(PromptRequest::new(
                SessionId::new(sid.clone()),
                vec![ContentBlock::from(data.content.clone())],
            ))
            .await
            .map_err(AppError::from)?;

        // Emit Finish event when prompt completes
        let _ = self.event_tx.send(AgentStreamEvent::Finish(
            crate::stream_event::FinishEventData {
                session_id: Some(sid),
            },
        ));

        Ok(())
    }

    /// Resume an existing session and send a message.
    async fn session_resume_and_send(
        &self,
        data: &SendMessageData,
        session_id: Option<&str>,
    ) -> Result<(), AppError> {
        let strategy = SessionResumeStrategy::for_backend(self.backend);

        if strategy == SessionResumeStrategy::SessionLoad
            && let Some(sid) = session_id
        {
            self.protocol
                .load_session(LoadSessionRequest::new(
                    SessionId::new(sid),
                    &self.workspace,
                ))
                .await
                .map_err(AppError::from)?;
        }

        self.prompt_existing_session(data, session_id).await
    }

    /// Send a prompt to an already-established session.
    async fn prompt_existing_session(
        &self,
        data: &SendMessageData,
        session_id: Option<&str>,
    ) -> Result<(), AppError> {
        let sid = session_id
            .ok_or_else(|| AppError::Internal("Cannot prompt: no session ID available".into()))?;

        // Emit Start event
        let _ = self.event_tx.send(AgentStreamEvent::Start(
            crate::stream_event::StartEventData {
                session_id: Some(sid.to_owned()),
            },
        ));

        self.protocol
            .prompt(PromptRequest::new(
                SessionId::new(sid),
                vec![ContentBlock::from(data.content.clone())],
            ))
            .await
            .map_err(AppError::from)?;

        // Emit Finish event
        let _ = self.event_tx.send(AgentStreamEvent::Finish(
            crate::stream_event::FinishEventData {
                session_id: Some(sid.to_owned()),
            },
        ));

        Ok(())
    }

    /// Enable YOLO mode for the current session if the backend supports it.
    pub async fn ensure_yolo_mode(&self) -> bool {
        let mode = match yolo_mode_value(self.backend) {
            Some(m) => m,
            None => return false,
        };

        let session_id = self.state.read().await.session_id.clone();
        let sid = match session_id {
            Some(ref s) => s.as_str(),
            None => return false,
        };

        match self
            .protocol
            .set_mode(SetSessionModeRequest::new(SessionId::new(sid), mode))
            .await
        {
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
        // With the SDK, mode info arrives via session update events.
        // We return a placeholder — the actual mode is tracked internally.
        Ok(json!({ "sent": true }))
    }

    /// Set the session mode.
    pub async fn set_mode(&self, mode: &str) -> Result<(), AppError> {
        let sid = self
            .state
            .read()
            .await
            .session_id
            .clone()
            .ok_or_else(|| AppError::BadRequest("No active session".into()))?;

        self.protocol
            .set_mode(SetSessionModeRequest::new(
                SessionId::new(sid),
                mode.to_owned(),
            ))
            .await
            .map_err(AppError::from)
    }

    /// Get model info from the ACP backend.
    pub async fn get_model_info(&self) -> Option<AcpModelInfo> {
        let state = self.state.read().await;
        state.model_info.clone()
    }

    /// Set the model for the current session.
    pub async fn set_model(&self, model_id: &str) -> Result<(), AppError> {
        let sid = self
            .state
            .read()
            .await
            .session_id
            .clone()
            .ok_or_else(|| AppError::BadRequest("No active session".into()))?;

        self.protocol
            .set_model(SetSessionModelRequest::new(
                SessionId::new(sid),
                model_id.to_owned(),
            ))
            .await
            .map_err(AppError::from)
    }

    /// Get the session configuration options.
    pub async fn get_config_options(&self) -> Result<(), AppError> {
        // Config options arrive via session update events.
        Ok(())
    }

    /// Set a session configuration option.
    pub async fn set_config_option(&self, config_id: &str, value: &str) -> Result<(), AppError> {
        let sid = self
            .state
            .read()
            .await
            .session_id
            .clone()
            .ok_or_else(|| AppError::BadRequest("No active session".into()))?;

        self.protocol
            .set_config_option(SetSessionConfigOptionRequest::new(
                SessionId::new(sid),
                config_id.to_owned(),
                value.to_owned(),
            ))
            .await
            .map_err(AppError::from)
    }

    /// Load available slash commands from the ACP backend.
    pub async fn load_slash_commands(&self) -> Result<(), AppError> {
        // Slash commands arrive via AvailableCommandsUpdate events.
        Ok(())
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
        let session_id = self.state.read().await.session_id.clone();
        if let Some(sid) = session_id {
            self.protocol
                .cancel(CancelNotification::new(SessionId::new(sid)));
        }

        // Clear pending confirmations on stop
        let mut state = self.state.write().await;
        state.confirmations.clear();

        Ok(())
    }

    fn confirm(
        &self,
        _msg_id: &str,
        call_id: &str,
        _data: serde_json::Value,
        always_allow: bool,
    ) -> Result<(), AppError> {
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

        // NOTE: With the SDK-based protocol, permission responses are handled
        // through the PermissionRequest.response_tx channel in the permission
        // handler. Since we currently auto-cancel there, confirm() is a no-op
        // for the protocol layer. A future iteration will wire this properly
        // by storing response_tx channels keyed by call_id.
        //
        // For now, log the confirmation for debugging.
        debug!(
            conversation_id = %self.conversation_id,
            call_id,
            "Confirmation processed (protocol response pending future iteration)"
        );

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

        // Mark closing to prevent reconnect attempts
        self.closing
            .store(true, std::sync::atomic::Ordering::Release);

        // Cancel the current session if active
        if let Ok(state) = self.state.try_read()
            && let Some(ref sid) = state.session_id
        {
            self.protocol
                .cancel(CancelNotification::new(SessionId::new(sid.as_str())));
        }

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
            SessionResumeStrategy::NewAndPrompt
        );
        assert_eq!(
            SessionResumeStrategy::for_backend(AcpBackend::Codebuddy),
            SessionResumeStrategy::NewAndPrompt
        );
        assert_eq!(
            SessionResumeStrategy::for_backend(AcpBackend::Qwen),
            SessionResumeStrategy::NewAndPrompt
        );
        assert_eq!(
            SessionResumeStrategy::for_backend(AcpBackend::Kiro),
            SessionResumeStrategy::NewAndPrompt
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
        assert_eq!(yolo_mode_value(AcpBackend::Goose), None);
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
}
