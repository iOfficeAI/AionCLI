use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use crate::acp_protocol::{AcpProtocol, PermissionDecision, PermissionRequest};

use crate::acp_runtime_snapshot::AcpRuntimeSnapshot;
use agent_client_protocol::schema::{
    AgentCapabilities, AvailableCommand, CancelNotification, ContentBlock, LoadSessionRequest,
    NewSessionRequest, PromptRequest, SessionConfigOption, SessionId, SessionModeState,
    SessionModelState, SetSessionConfigOptionRequest, SetSessionModeRequest,
    SetSessionModelRequest, UsageUpdate,
};

use crate::cli_process::CliAgentProcess;
use crate::stream_event::{AgentStreamEvent, permission_request_to_event_data};
use crate::types::{AcpBuildExtra, SendMessageData, SlashCommandItem};

use aionui_common::{
    AcpBackend, AgentKillReason, AgentType, AppError, CommandSpec, Confirmation,
    ConversationStatus, TimestampMs, now_ms,
};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc, oneshot};
use tracing::{debug, error, info};

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

fn confirm_option_id(data: &Value) -> Option<String> {
    match data {
        Value::String(v) => Some(v.clone()),
        Value::Object(map) => map
            .get("option_id")
            .or_else(|| map.get("optionId"))
            .or_else(|| map.get("value"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        _ => None,
    }
}

/// Internal state that changes at runtime.
struct AcpState {
    /// Current conversation status.
    status: Option<ConversationStatus>,
    /// Active session ID (set after session/new or session/load).
    session_id: Option<String>,
    /// Whether this session has sent at least one message.
    has_messages: bool,
}

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
    /// Whether the workspace was explicitly chosen by the user rather
    /// than auto-provisioned (e.g. the default
    /// `{data_dir}/conversations/{id}/` path). Determined at agent
    /// construction time — do NOT re-derive from the workspace string,
    /// which is fragile (user paths may happen to contain
    /// `"conversations"` or `"-temp-"`).
    is_custom_workspace: bool,
    /// ACP sub-backend.
    backend: AcpBackend,
    /// Build configuration (preset context, enabled/excluded skills, session mode, …).
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
    /// Pending ACP permission responders keyed by tool call ID.
    pending_permissions: StdMutex<HashMap<String, oneshot::Sender<PermissionDecision>>>,
    /// Runtime ACP session snapshot used by getters.
    runtime_snapshot: RwLock<AcpRuntimeSnapshot>,
    /// Whether a graceful shutdown is in progress.
    closing: std::sync::atomic::AtomicBool,
    /// Shared skill manager — used to discover skills for first-message injection.
    skill_manager: Arc<crate::skill_manager::AcpSkillManager>,
}

impl AcpAgentManager {
    /// Current session mode id. Falls back to the configured session mode,
    /// then to `"default"`. Reading a cached snapshot is infallible.
    pub async fn modes(&self) -> Option<SessionModeState> {
        let snapshot = self.runtime_snapshot.read().await;
        snapshot.modes().cloned()
    }

    async fn _set_modes(&self, mode: &str) -> Result<(), AppError> {
        let sid = self.require_session_id().await?;
        self.protocol
            .set_mode(SetSessionModeRequest::new(
                SessionId::new(sid),
                mode.to_owned(),
            ))
            .await
            .map_err(AppError::from)
            .map(|_| ())
    }

    /// Cached model info from the ACP backend, if any has been received.
    pub async fn model_info(&self) -> Option<SessionModelState> {
        let snapshot = self.runtime_snapshot.read().await;
        snapshot.model_info().cloned()
    }

    /// Set the model for the current session.
    pub async fn set_model_info(&self, model_id: &str) -> Result<(), AppError> {
        let sid = self.require_session_id().await?;

        self.protocol
            .set_model(SetSessionModelRequest::new(
                SessionId::new(sid),
                model_id.to_owned(),
            ))
            .await
            .map_err(AppError::from)?;

        // Update the snapshot immediately since SDK does not send a
        // CurrentModelUpdate notification for model changes.
        {
            let mut snapshot = self.runtime_snapshot.write().await;
            if let Some(info) = snapshot.model_info().cloned() {
                let updated = SessionModelState::new(model_id.to_owned(), info.available_models);
                snapshot.set_model_info(updated);
            }
        }

        Ok(())
    }

    /// Cached session configuration options.
    pub async fn config_options(&self) -> Vec<SessionConfigOption> {
        let snapshot = self.runtime_snapshot.read().await;
        snapshot
            .config_options()
            .map(<[SessionConfigOption]>::to_vec)
            .unwrap_or_default()
    }

    /// Set a session configuration option.
    pub async fn set_config_option(&self, config_id: &str, value: &str) -> Result<(), AppError> {
        let sid = self.require_session_id().await?;

        self.protocol
            .set_config_option(SetSessionConfigOptionRequest::new(
                SessionId::new(sid),
                config_id.to_owned(),
                value.to_owned(),
            ))
            .await
            .map_err(AppError::from)
            .map(|_| ())
    }

    /// Cached context usage info from the ACP backend.
    pub async fn usage(&self) -> Option<UsageUpdate> {
        let snapshot = self.runtime_snapshot.read().await;
        snapshot.context_usage().cloned()
    }

    /// Agent capabilities captured during the ACP initialize handshake.
    pub async fn agent_capabilities(&self) -> Option<AgentCapabilities> {
        let snapshot = self.runtime_snapshot.read().await;
        snapshot.agent_capabilities().cloned()
    }

    /// Cached available commands from the ACP backend.
    pub async fn available_commands(&self) -> Option<Vec<AvailableCommand>> {
        let snapshot = self.runtime_snapshot.read().await;
        snapshot.available_commands().map(|c| c.to_vec())
    }
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
        is_custom_workspace: bool,
        command_spec: CommandSpec,
        config: AcpBuildExtra,
        skill_manager: Arc<crate::skill_manager::AcpSkillManager>,
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

        let mut runtime_snapshot = AcpRuntimeSnapshot::default();
        if let Some(agent_capabilities) = protocol.agent_capabilities() {
            runtime_snapshot.set_agent_capabilities(agent_capabilities);
        }
        if let Some(auth_methods) = protocol.auth_methods() {
            runtime_snapshot.set_auth_methods(auth_methods);
        }

        let manager = Self {
            conversation_id,
            workspace,
            is_custom_workspace,
            backend,
            config,
            process: Arc::new(process),
            protocol,
            event_tx,
            state: RwLock::new(AcpState {
                status: None,
                session_id: None,
                has_messages: false,
            }),
            last_activity: AtomicI64::new(now_ms()),
            session_lock: Mutex::new(()),
            permission_rx: Mutex::new(permission_rx),
            pending_permissions: StdMutex::new(HashMap::new()),
            runtime_snapshot: RwLock::new(runtime_snapshot),
            closing: std::sync::atomic::AtomicBool::new(false),
            skill_manager,
        };

        Ok(manager)
    }

    /// Start the permission handler loop. Must be called after the manager
    /// is wrapped in Arc.
    ///
    /// This background task receives permission requests from the protocol
    /// layer, converts them to `Permission` events, and waits for user
    /// responses routed through the `confirm()` method.
    pub fn start_permission_handler(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut rx = this.permission_rx.lock().await;

            while let Some(perm_req) = rx.recv().await {
                this.last_activity.store(now_ms(), Ordering::Relaxed);

                let call_id = perm_req.request.tool_call.tool_call_id.to_string();

                let mut pending = this.pending_permissions.lock().unwrap();
                if let Some(previous) = pending.insert(call_id.clone(), perm_req.response_tx) {
                    let _ = previous.send(PermissionDecision::Cancelled);
                }
                drop(pending);

                let permission_event = permission_request_to_event_data(&perm_req.request);

                if this
                    .event_tx
                    .send(AgentStreamEvent::AcpPermission(permission_event))
                    .is_err()
                    && let Some(response_tx) =
                        this.pending_permissions.lock().unwrap().remove(&call_id)
                {
                    let _ = response_tx.send(PermissionDecision::Cancelled);
                }
            }
        });
    }

    /// Start the runtime snapshot tracker loop.
    pub fn start_runtime_snapshot_tracker(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut rx = this.event_tx.subscribe();
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let mut snapshot = this.runtime_snapshot.write().await;
                        snapshot.apply_event(&event);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        });
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

        let session_response = self
            .protocol
            .new_session(NewSessionRequest::new(&self.workspace))
            .await
            .map_err(AppError::from)?;

        let sid = session_response.session_id.to_string();

        // Populate the runtime snapshot from the session response
        {
            let mut snapshot = self.runtime_snapshot.write().await;
            if let Some(models) = session_response.models {
                snapshot.set_model_info(models);
            }
            if let Some(modes) = session_response.modes {
                snapshot.set_modes(modes);
            }
            if let Some(config_options) = session_response.config_options {
                snapshot.set_config_options(config_options);
            }
        }
        self.emit_snapshot_events().await;
        {
            let mut state = self.state.write().await;
            state.session_id = Some(sid.clone());
        }

        // Inject first-message prefix (preset context + skills index).
        // Backends with native skill discovery (e.g. Claude via .claude/skills/)
        // only need preset_context here; others get the full [Assistant Rules]
        // block with a skills index.
        let injected_content = crate::first_message_injector::inject_first_message_prefix(
            &data.content,
            &self.skill_manager,
            crate::first_message_injector::InjectionConfig {
                preset_context: self.config.preset_context.as_deref(),
                skills: &self.config.skills,
                native_skill_support: self.backend.native_skills_dirs().is_some(),
                // Whether the user chose this workspace — determined at
                // factory-time and stored on the manager. Do NOT derive
                // from `self.workspace`; path heuristics are fragile
                // (user paths may incidentally contain "conversations"
                // or "-temp-").
                custom_workspace: self.is_custom_workspace,
            },
        )
        .await;

        // Send the prompt
        self.protocol
            .prompt(PromptRequest::new(
                SessionId::new(sid.clone()),
                vec![ContentBlock::from(injected_content)],
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
            let resp = self
                .protocol
                .load_session(LoadSessionRequest::new(
                    SessionId::new(sid),
                    &self.workspace,
                ))
                .await
                .map_err(AppError::from)?;

            let mut snapshot = self.runtime_snapshot.write().await;
            if let Some(models) = resp.models {
                snapshot.set_model_info(models);
            }
            if let Some(modes) = resp.modes {
                snapshot.set_modes(modes);
            }
            if let Some(config_options) = resp.config_options {
                snapshot.set_config_options(config_options);
            }
        }

        self.emit_snapshot_events().await;

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

    /// Emit model/mode/config events from the current snapshot so the frontend
    /// receives the initial session state via WebSocket immediately after
    /// session creation or load.
    async fn emit_snapshot_events(&self) {
        use aionui_api_types::{ModelInfoEntry, ModelInfoPayload};

        let snapshot = self.runtime_snapshot.read().await;
        if let Some(models) = snapshot.model_info() {
            let current_id = models.current_model_id.to_string();
            let available: Vec<ModelInfoEntry> = models
                .available_models
                .iter()
                .map(|am| ModelInfoEntry {
                    id: am.model_id.to_string(),
                    label: am.name.clone(),
                })
                .collect();
            let current_label = available
                .iter()
                .find(|e| e.id == current_id)
                .map(|e| e.label.clone())
                .unwrap_or_else(|| current_id.clone());
            let payload = ModelInfoPayload {
                current_model_id: Some(current_id),
                current_model_label: Some(current_label),
                available_models: available,
            };
            if let Ok(v) = serde_json::to_value(&payload) {
                let _ = self.event_tx.send(AgentStreamEvent::AcpModelInfo(v));
            }
        }
        if let Some(modes) = snapshot.modes()
            && let Ok(v) = serde_json::to_value(modes)
        {
            let _ = self.event_tx.send(AgentStreamEvent::AcpModeInfo(v));
        }
        if let Some(config_options) = snapshot.config_options()
            && let Ok(v) = serde_json::to_value(config_options)
        {
            let _ = self.event_tx.send(AgentStreamEvent::AcpConfigOption(v));
        }
        if let Some(cmds) = snapshot.available_commands() {
            let _ = self.event_tx.send(AgentStreamEvent::AvailableCommands(
                crate::stream_event::AvailableCommandsEventData {
                    commands: cmds.to_vec(),
                },
            ));
        }
    }

    /// Return available slash commands from the cached runtime snapshot.
    pub async fn load_slash_commands(&self) -> Result<Vec<SlashCommandItem>, AppError> {
        let snapshot = self.runtime_snapshot.read().await;
        let items = snapshot
            .available_commands()
            .map(|cmds| {
                cmds.iter()
                    .map(|c| SlashCommandItem {
                        command: c.name.clone(),
                        description: c.description.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(items)
    }

    /// Current ACP session ID, if a session has been established.
    pub async fn session_id(&self) -> Option<String> {
        self.state.read().await.session_id.clone()
    }

    /// ACP sub-backend (Claude, Codex, …).
    pub fn backend(&self) -> AcpBackend {
        self.backend
    }

    /// Return the active session id or a `BadRequest` error.
    async fn require_session_id(&self) -> Result<String, AppError> {
        self.state
            .read()
            .await
            .session_id
            .clone()
            .ok_or_else(|| AppError::BadRequest("No active session".into()))
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
        for (_, responder) in self.pending_permissions.lock().unwrap().drain() {
            let _ = responder.send(PermissionDecision::Cancelled);
        }

        Ok(())
    }

    fn confirm(
        &self,
        _msg_id: &str,
        call_id: &str,
        data: serde_json::Value,
        _always_allow: bool,
    ) -> Result<(), AppError> {
        let option_id = confirm_option_id(&data).ok_or_else(|| {
            AppError::BadRequest("ACP confirmation requires an option_id string".into())
        })?;

        let responder = self
            .pending_permissions
            .lock()
            .unwrap()
            .remove(call_id)
            .ok_or_else(|| {
                AppError::BadRequest(format!("Pending ACP permission not found: {call_id}"))
            })?;

        responder
            .send(PermissionDecision::Selected { option_id })
            .map_err(|_| {
                AppError::BadRequest(format!("Pending ACP permission expired: {call_id}"))
            })?;

        debug!(conversation_id = %self.conversation_id, call_id, "ACP permission response forwarded");
        Ok(())
    }

    fn get_confirmations(&self) -> Vec<Confirmation> {
        Vec::new()
    }

    fn check_approval(&self, _action: &str, _command_type: Option<&str>) -> bool {
        false
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

        for (_, responder) in self.pending_permissions.lock().unwrap().drain() {
            let _ = responder.send(PermissionDecision::Cancelled);
        }

        Ok(())
    }

    async fn get_mode(&self) -> Result<aionui_api_types::AgentModeResponse, AppError> {
        Ok(aionui_api_types::AgentModeResponse {
            mode: self
                .modes()
                .await
                .map(|modes| modes.current_mode_id.to_string())
                .or_else(|| self.config.session_mode.clone())
                .unwrap_or_else(|| "default".to_owned()),
            initialized: self.session_id().await.is_some(),
        })
    }

    async fn set_mode(&self, mode: &str) -> Result<(), AppError> {
        let sid = self.require_session_id().await?;
        self.protocol
            .set_mode(SetSessionModeRequest::new(
                SessionId::new(sid),
                mode.to_owned(),
            ))
            .await
            .map_err(AppError::from)
            .map(|_| ())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn confirm_option_id_accepts_string_or_object() {
        assert_eq!(
            confirm_option_id(&Value::String("allow_once".into())).as_deref(),
            Some("allow_once")
        );
        assert_eq!(
            confirm_option_id(&json!({ "option_id": "reject_once" })).as_deref(),
            Some("reject_once")
        );
        assert_eq!(
            confirm_option_id(&json!({ "value": "allow_always" })).as_deref(),
            Some("allow_always")
        );
    }
}
