use crate::capability::cli_process::CliAgentProcess;
use crate::capability::first_message_injector::{InjectionConfig, inject_first_message_prefix};
use crate::capability::skill_manager::AcpSkillManager;
use crate::factory::acp_assembler::AcpSessionParams;
use crate::manager::acp::{AcpSession, AcpSessionEvent, PermissionRouter, PersistedSessionState};
use crate::protocol::acp::AcpProtocol;
use crate::protocol::events::{
    AgentStreamEvent, AvailableCommandsEventData, ErrorEventData, FinishEventData, SessionAssignedEventData,
    StartEventData,
};
use crate::registry::CatalogSender;
use crate::shared_kernel::{ModeId, ModelId, SessionId as DomainSessionId};
use crate::types::SendMessageData;
use agent_client_protocol::schema::{
    AgentCapabilities, AvailableCommand, CancelNotification, ContentBlock, LoadSessionRequest, PromptRequest,
    SessionConfigOption, SessionId, SessionModeState, SessionModelState, SetSessionConfigOptionRequest,
    SetSessionModeRequest, SetSessionModelRequest, UsageUpdate,
};
use aionui_api_types::{AgentHandshake, SlashCommandItem};
use aionui_common::{
    AgentKillReason, AgentType, AppError, Confirmation, ConversationStatus, TimestampMs, normalize_keys_to_snake_case,
    now_ms,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tracing::{error, info};

use super::mode_normalize::{agent_metadata_uses_claude_meta_resume, normalize_requested_mode};

/// Grace period before force-killing an ACP process (ms).
const ACP_KILL_GRACE_MS: u64 = 500;

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

/// Serialize an external value (typically an ACP SDK struct that emits
/// camelCase) and normalise every object key to snake_case before it
/// leaves the backend. All handshake columns, WebSocket payloads, and
/// HTTP responses share this rule — callers should go through this
/// helper instead of `serde_json::to_value` directly.
fn sdk_to_snake_value<T: serde::Serialize>(value: &T) -> Option<Value> {
    let mut v = serde_json::to_value(value).ok()?;
    normalize_keys_to_snake_case(&mut v);
    Some(v)
}

/// Manages a single ACP Agent instance.
///
/// ACP is the most complex agent type, supporting 20+ CLI sub-backends
/// (Claude, Qwen, CodeBuddy, Codex, etc.). Communication now happens via
/// the `agent-client-protocol` SDK's JSON-RPC transport, replacing the
/// previous hand-crafted JSON-over-stdin/stdout approach.
pub struct AcpAgentManager {
    /// Pre-computed, immutable session parameters assembled by the factory.
    params: Arc<AcpSessionParams>,
    /// Session aggregate root — owns desired/observed/advertised state.
    /// Single in-memory source of truth for session lifecycle, modes,
    /// models, config, and all runtime data previously split across
    /// `AcpRuntimeSnapshot` and `AcpState`.
    session: RwLock<AcpSession>,
    /// Standalone conversation status (not part of the session aggregate
    /// because it is a UI-level concern, not ACP protocol state).
    status: RwLock<Option<ConversationStatus>>,
    /// Underlying CLI process (for lifecycle management: kill, is_running).
    process: Arc<CliAgentProcess>,
    /// ACP protocol handle (SDK connection).
    protocol: AcpProtocol,
    /// Typed event broadcast channel.
    event_tx: broadcast::Sender<AgentStreamEvent>,
    /// Timestamp of last activity (atomic for lock-free reads). Shared
    /// with the `PermissionRouter` so permission arrivals update the
    /// activity timestamp without reverse-referencing the manager.
    last_activity: Arc<AtomicI64>,
    /// Mutex for serializing session operations (new/load/send).
    session_lock: Mutex<()>,
    /// Routes permission requests from the protocol layer to the user
    /// and back. Owns the receiver channel, pending map, and closing flag.
    permission_router: Arc<PermissionRouter>,
    /// Shared skill manager — used to discover skills for first-message injection.
    skill_manager: Arc<AcpSkillManager>,
    /// Domain event sender — session aggregate events are forwarded here
    /// for the persistence consumer (`AcpSessionSyncService`).
    domain_event_tx: mpsc::Sender<AcpSessionEvent>,
}

impl AcpAgentManager {
    /// Current session mode state. Reading a cached session is infallible.
    pub async fn modes(&self) -> Option<SessionModeState> {
        self.session.read().await.modes().cloned()
    }

    async fn desired_mode(&self) -> Option<String> {
        self.session
            .read()
            .await
            .desired_mode()
            .map(ToOwned::to_owned)
            .filter(|mode| !mode.is_empty())
    }

    async fn update_cached_mode(&self, mode: &str) {
        let mut session = self.session.write().await;
        session.apply_partial_mode_update(ModeId::new(mode));
    }

    /// Execute reconcile actions produced by `AcpSession::plan_reconcile`.
    ///
    /// Compares the aggregate's desired state against what the CLI has
    /// reported as current, then issues the minimal set of SDK calls
    /// (set_mode, set_config_option) to bring the CLI into alignment.
    /// Best-effort: individual failures are logged but do not abort.
    async fn reconcile_session(&self, session_id: &str) {
        use crate::manager::acp::ReconcileAction;

        let actions = {
            let session = self.session.read().await;
            session.plan_reconcile()
        };

        for action in actions {
            match action {
                ReconcileAction::SetMode { mode } => {
                    let normalized = normalize_requested_mode(&self.params.metadata, mode.as_str());
                    if normalized.is_empty() {
                        continue;
                    }
                    if let Err(e) = self
                        .protocol
                        .set_mode(SetSessionModeRequest::new(
                            SessionId::new(session_id),
                            normalized.clone(),
                        ))
                        .await
                    {
                        error!(
                            conversation_id = %self.params.conversation_id,
                            mode_id = %normalized,
                            error = %e,
                            "reconcile_session: set_mode failed"
                        );
                        continue;
                    }
                    self.update_cached_mode(&normalized).await;
                    let mut session = self.session.write().await;
                    session.apply_observed_mode(ModeId::new(normalized));
                }
                ReconcileAction::SetConfigOption { key, value } => {
                    if let Err(err) = self
                        .protocol
                        .set_config_option(SetSessionConfigOptionRequest::new(
                            SessionId::new(session_id),
                            key.as_str().to_owned(),
                            value.as_str().to_owned(),
                        ))
                        .await
                    {
                        info!(
                            config_id = %key,
                            desired = %value,
                            error = %err,
                            "reconcile_session: set_config_option failed; skipping"
                        );
                    }
                }
            }
        }
    }

    /// Cached model info from the ACP backend, if any has been received.
    pub async fn model_info(&self) -> Option<SessionModelState> {
        self.session.read().await.model_info().cloned()
    }

    /// Set the model for the current session.
    pub async fn set_model_info(&self, model_id: &str) -> Result<(), AppError> {
        let sid = self.require_session_id().await?;

        self.protocol
            .set_model(SetSessionModelRequest::new(SessionId::new(sid), model_id.to_owned()))
            .await
            .map_err(AppError::from)?;

        // Update the session immediately since SDK does not send a
        // CurrentModelUpdate notification for model changes.
        {
            let mut session = self.session.write().await;
            session.update_current_model(ModelId::new(model_id));
        }

        Ok(())
    }

    /// Cached session configuration options.
    pub async fn config_options(&self) -> Vec<SessionConfigOption> {
        self.session
            .read()
            .await
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
        self.session.read().await.context_usage().cloned()
    }

    /// Agent capabilities captured during the ACP initialize handshake.
    pub async fn agent_capabilities(&self) -> Option<AgentCapabilities> {
        self.session.read().await.agent_capabilities().cloned()
    }

    /// Cached available commands from the ACP backend.
    pub async fn available_commands(&self) -> Option<Vec<AvailableCommand>> {
        self.session.read().await.available_commands().map(|c| c.to_vec())
    }
}

impl AcpAgentManager {
    /// Create a new ACP agent manager by spawning a CLI subprocess and
    /// establishing an ACP protocol connection.
    ///
    /// `params` is the pre-computed, immutable session bundle assembled by
    /// `assemble_acp_params` in the factory layer. `catalog_tx` is the
    /// MPSC sender used for the one-shot initialize handshake write;
    /// session-driven fields flow through the `CatalogForwarder` the
    /// factory spawns after construction.
    pub async fn new(
        params: Arc<AcpSessionParams>,
        skill_manager: Arc<AcpSkillManager>,
        catalog_tx: &CatalogSender,
    ) -> Result<(Self, mpsc::Receiver<AcpSessionEvent>), AppError> {
        let process = CliAgentProcess::spawn_for_sdk(params.command_spec.clone()).await?;

        // Take raw stdio for the SDK transport
        let (stdin, stdout) = process
            .take_stdio()
            .await
            .ok_or_else(|| AppError::Internal("Failed to take stdio from CLI process".into()))?;

        let (event_tx, _) = broadcast::channel(256);
        let (permission_tx, permission_rx) = mpsc::channel(32);
        let (domain_event_tx, domain_event_rx) = mpsc::channel(256);

        // Connect via ACP SDK — executes initialize handshake
        let protocol = AcpProtocol::connect(stdin, stdout, event_tx.clone(), permission_tx)
            .await
            .map_err(|e| {
                error!(
                    conversation_id = %params.conversation_id,
                    error = %e,
                    "Failed to establish ACP protocol connection"
                );
                AppError::from(e)
            })?;

        // Push the static handshake payloads (agent_capabilities +
        // auth_methods) through the catalog sync channel. Session-driven
        // fields — modes, models, config_options, commands — flow
        // through the `CatalogForwarder` the factory spawns after
        // construction.
        let init_handshake = AgentHandshake {
            agent_capabilities: protocol.agent_capabilities().and_then(|c| sdk_to_snake_value(&c)),
            auth_methods: protocol.auth_methods().and_then(|m| sdk_to_snake_value(&m)),
            ..Default::default()
        };
        if init_handshake.agent_capabilities.is_some() || init_handshake.auth_methods.is_some() {
            catalog_tx.send_partial(params.metadata.id.clone(), init_handshake);
        }

        let initial_mode = params
            .config
            .session_mode
            .as_ref()
            .map(|m| normalize_requested_mode(&params.metadata, m))
            .filter(|m| !m.is_empty())
            .map(ModeId::new);
        let mut session = AcpSession::new(initial_mode, HashMap::new());
        if let Some(agent_capabilities) = protocol.agent_capabilities() {
            session.apply_advertised_capabilities(agent_capabilities);
        }
        if let Some(auth_methods) = protocol.auth_methods() {
            session.apply_advertised_auth_methods(auth_methods);
        }

        let permission_router = Arc::new(PermissionRouter::new(permission_rx));

        let manager = Self {
            params,
            session: RwLock::new(session),
            status: RwLock::new(None),
            process: Arc::new(process),
            protocol,
            event_tx,
            last_activity: Arc::new(AtomicI64::new(now_ms())),
            session_lock: Mutex::new(()),
            permission_router,
            skill_manager,
            domain_event_tx,
        };

        Ok((manager, domain_event_rx))
    }

    /// Start the permission handler loop. Must be called after the manager
    /// is wrapped in Arc. Delegates to `PermissionRouter::start`.
    pub fn start_permission_handler(self: &Arc<Self>) {
        self.permission_router
            .start(self.event_tx.clone(), Arc::clone(&self.last_activity));
    }

    /// Start the session event tracker loop.
    pub fn start_session_event_tracker(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut rx = this.event_tx.subscribe();
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        this.apply_event_to_session(&event).await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        });
    }

    /// Mirror a stream event into the `AcpSession` aggregate's observed/advertised
    /// layer and forward any resulting domain events to the persistence consumer.
    async fn apply_event_to_session(&self, event: &AgentStreamEvent) {
        match event {
            AgentStreamEvent::AcpModeInfo(value) => {
                if let Ok(update) = serde_json::from_value::<SessionModeState>(value.clone()) {
                    let mut s = self.session.write().await;
                    s.apply_advertised_modes(update);
                    self.commit_session_changes(&mut s).await;
                } else if let Some(current_id) = value.get("currentModeId").and_then(|v: &Value| v.as_str()) {
                    let mut s = self.session.write().await;
                    s.apply_observed_mode(ModeId::new(current_id));
                    self.commit_session_changes(&mut s).await;
                }
            }
            AgentStreamEvent::AcpModelInfo(value) => {
                if let Ok(update) = serde_json::from_value::<SessionModelState>(value.clone()) {
                    let mut s = self.session.write().await;
                    s.apply_advertised_models(update);
                    self.commit_session_changes(&mut s).await;
                }
            }
            AgentStreamEvent::AcpConfigOption(value) => {
                if let Ok(update) = serde_json::from_value::<Vec<SessionConfigOption>>(value.clone()) {
                    let mut s = self.session.write().await;
                    s.apply_advertised_config_options(update);
                    self.commit_session_changes(&mut s).await;
                }
            }
            AgentStreamEvent::AcpContextUsage(value) => {
                if let Ok(update) = serde_json::from_value::<UsageUpdate>(value.clone()) {
                    let mut s = self.session.write().await;
                    s.apply_context_usage(update);
                }
            }
            _ => {}
        }
    }

    /// Drain pending domain events from the session aggregate and
    /// forward them to the persistence consumer via the mpsc channel.
    async fn commit_session_changes(&self, session: &mut AcpSession) {
        for event in session.drain_events() {
            let _ = self.domain_event_tx.send(event).await;
        }
    }

    /// Seed the session aggregate with the user's last choices. Called
    /// by `ConversationService` on resume paths, before dispatching
    /// `send_message`. `None` fields are ignored — the CLI's
    /// `session/load` response fills in whatever the preload omits.
    pub async fn preload_snapshot(&self, state: PersistedSessionState) {
        let mut session = self.session.write().await;
        session.preload_persisted(&state);
        if let Some(mode) = &state.current_mode_id {
            let normalized = normalize_requested_mode(&self.params.metadata, mode.as_str());
            if !normalized.is_empty() {
                session.set_desired_mode(ModeId::new(normalized));
            }
        }
        for (key, value) in &state.config_selections {
            session.set_desired_config(key.clone(), value.clone());
        }
        // Preload events are discarded — the DB already has these values.
        session.drain_events();
    }

    /// Initialize or resume a session, then send the user message.
    ///
    /// Three paths:
    /// 1. **No session_id at all** → `session/new` + first prompt.
    /// 2. **Have session_id but this instance has not yet opened it with the
    ///    CLI** → `session/load` (or claude-meta-resume) + prompt. This
    ///    happens on the first turn after a task rebuild or after
    ///    `restore_session_id` seeded the id from the DB.
    /// 3. **Session already opened by this instance** → plain `prompt`. No
    ///    `session/load` — the CLI child process still owns the session in
    ///    memory, re-loading every turn would both waste a round-trip and
    ///    (on some backends) reset config options.
    async fn ensure_session_and_send(&self, data: &SendMessageData) -> Result<(), AppError> {
        let _lock = self.session_lock.lock().await;

        let (session_id, opened) = {
            let s = self.session.read().await;
            (s.session_id().map(ToOwned::to_owned), s.is_opened())
        };

        match (session_id.as_deref(), opened) {
            (None, _) => {
                // Path 1: first turn in a brand-new conversation.
                self.session_new_and_prompt(data).await?;
            }
            (Some(sid), false) => {
                // Path 2: we have a persisted id but this process has not
                // opened it with the CLI yet. Needs backend-appropriate
                // resume handshake before the prompt.
                self.session_resume_and_send(data, Some(sid)).await?;
            }
            (Some(sid), true) => {
                // Path 3: session is live with the CLI; just prompt.
                self.prompt_existing_session(data, Some(sid)).await?;
            }
        }

        {
            let mut s = self.session.write().await;
            s.mark_opened();
            self.commit_session_changes(&mut s).await;
        }
        *self.status.write().await = Some(ConversationStatus::Running);

        Ok(())
    }

    /// Create a new ACP session and send the first prompt.
    async fn session_new_and_prompt(&self, data: &SendMessageData) -> Result<(), AppError> {
        // Emit Start event
        let _ = self
            .event_tx
            .send(AgentStreamEvent::Start(StartEventData { session_id: None }));

        let req = self.params.new_session_request();
        tracing::info!(
            has_team_mcp = self.params.config.team_mcp_stdio_config.is_some(),
            has_guide_mcp = self.params.config.guide_mcp_config.is_some(),
            guide_mcp_port = self.params.config.guide_mcp_config.as_ref().map(|c| c.port),
            mcp_servers_count = req.mcp_servers.len(),
            "session_new_and_prompt: sending session/new"
        );
        let session_response = self.protocol.new_session(req).await.map_err(AppError::from)?;

        let sid = session_response.session_id.to_string();

        // Populate the session aggregate from the session response
        {
            let mut session = self.session.write().await;
            if let Some(models) = session_response.models {
                session.apply_advertised_models(models);
            }
            if let Some(modes) = session_response.modes {
                session.apply_advertised_modes(modes);
            }
            if let Some(config_options) = session_response.config_options {
                session.apply_advertised_config_options(config_options);
            }
            session.assign_session_id(DomainSessionId::new(sid.clone()));
            self.commit_session_changes(&mut session).await;
        }
        self.emit_snapshot_events().await;

        // Notify subscribers (e.g. session_sync consumer) so the new id is
        // persisted into `acp_session.session_id` — resume can then
        // choose `session/load` instead of a fresh `session/new`.
        let _ = self
            .event_tx
            .send(AgentStreamEvent::SessionAssigned(SessionAssignedEventData {
                session_id: sid.clone(),
            }));

        self.reconcile_session(&sid).await;

        let injected_content = inject_first_message_prefix(
            &data.content,
            &self.skill_manager,
            InjectionConfig {
                preset_context: self.params.preset_context.as_deref(),
                skills: &self.params.config.skills,
                native_skill_support: self.native_skill_support(),
                custom_workspace: self.params.workspace.is_custom,
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
        let _ = self
            .event_tx
            .send(AgentStreamEvent::Finish(FinishEventData { session_id: Some(sid) }));

        Ok(())
    }

    /// Resume an existing session and send a message.
    ///
    /// Assumes `preload_snapshot` has already been called by the
    /// caller (conversation service) on resume paths — the session
    /// aggregate may therefore already carry `current_mode_id` / `current_model_id`
    /// from `acp_session.session_config.runtime`. When the CLI's
    /// `session/load` response arrives, we merge it in but keep the
    /// preloaded `current_*` values because they reflect the user's
    /// last explicit choice; the CLI's own `current_*` is only used
    /// when the aggregate has nothing yet.
    async fn session_resume_and_send(&self, data: &SendMessageData, session_id: Option<&str>) -> Result<(), AppError> {
        if self.uses_claude_meta_resume() {
            // Claude backend: use session/new with _meta.claudeCode.options.resume
            // instead of session/load. This matches AionUi frontend behavior and
            // ensures mcpServers are re-injected on resume.
            if let Some(sid) = session_id {
                let mut meta = serde_json::Map::new();
                let mut claude_code = serde_json::Map::new();
                let mut options = serde_json::Map::new();
                options.insert("resume".into(), Value::String(sid.to_owned()));
                claude_code.insert("options".into(), Value::Object(options));
                meta.insert("claudeCode".into(), Value::Object(claude_code));

                let req = self.params.new_session_request().meta(meta);

                info!(
                    session_id = %sid,
                    has_team_mcp = self.params.config.team_mcp_stdio_config.is_some(),
                    has_guide_mcp = self.params.config.guide_mcp_config.is_some(),
                    guide_mcp_port = self.params.config.guide_mcp_config.as_ref().map(|c| c.port),
                    mcp_servers_count = req.mcp_servers.len(),
                    "session_resume: using session/new with claudeCode.options.resume"
                );

                let session_response = self.protocol.new_session(req).await.map_err(AppError::from)?;

                let new_sid = session_response.session_id.to_string();
                {
                    let mut session = self.session.write().await;
                    if let Some(models) = session_response.models {
                        session.apply_advertised_models(models);
                    }
                    if let Some(modes) = session_response.modes {
                        session.apply_advertised_modes(modes);
                    }
                    if let Some(config_options) = session_response.config_options {
                        session.apply_advertised_config_options(config_options);
                    }
                    session.assign_session_id(DomainSessionId::new(new_sid.clone()));
                    self.commit_session_changes(&mut session).await;
                }
                self.emit_snapshot_events().await;

                self.reconcile_session(&new_sid).await;

                return self.prompt_existing_session(data, Some(&new_sid)).await;
            }
        } else if self.supports_session_load()
            && let Some(sid) = session_id
        {
            // Non-Claude backends (e.g. Codex): use session/load
            let (preloaded_mode, preloaded_model) = {
                let session = self.session.read().await;
                (
                    session.modes().map(|m| m.current_mode_id.to_string()),
                    session.model_info().map(|m| m.current_model_id.to_string()),
                )
            };

            let mut load_req = LoadSessionRequest::new(SessionId::new(sid), &self.params.workspace.path);
            if !self.params.mcp_servers.is_empty() {
                load_req = load_req.mcp_servers(self.params.mcp_servers.clone());
            }
            let resp = self.protocol.load_session(load_req).await.map_err(AppError::from)?;

            let mut session = self.session.write().await;
            if let Some(mut models) = resp.models {
                if let Some(db_current) = preloaded_model {
                    models.current_model_id = db_current.into();
                }
                session.apply_advertised_models(models);
            }
            if let Some(mut modes) = resp.modes {
                if let Some(db_current) = preloaded_mode {
                    modes.current_mode_id = db_current.into();
                }
                session.apply_advertised_modes(modes);
            }
            if let Some(config_options) = resp.config_options {
                session.apply_advertised_config_options(config_options);
            }
            drop(session);
        }

        self.emit_snapshot_events().await;

        // Seed the session aggregate and reconcile.
        if let Some(sid) = session_id {
            {
                let mut session = self.session.write().await;
                session.assign_session_id(DomainSessionId::new(sid));
                self.commit_session_changes(&mut session).await;
            }
            self.reconcile_session(sid).await;
        }

        self.prompt_existing_session(data, session_id).await
    }

    /// Send a prompt to an already-established session.
    async fn prompt_existing_session(&self, data: &SendMessageData, session_id: Option<&str>) -> Result<(), AppError> {
        let sid = session_id.ok_or_else(|| AppError::Internal("Cannot prompt: no session ID available".into()))?;

        // Emit Start event
        let _ = self.event_tx.send(AgentStreamEvent::Start(StartEventData {
            session_id: Some(sid.to_owned()),
        }));

        self.protocol
            .prompt(PromptRequest::new(
                SessionId::new(sid),
                vec![ContentBlock::from(data.content.clone())],
            ))
            .await
            .map_err(AppError::from)?;

        // Emit Finish event
        let _ = self.event_tx.send(AgentStreamEvent::Finish(FinishEventData {
            session_id: Some(sid.to_owned()),
        }));

        Ok(())
    }

    /// Emit model/mode/config events from the session aggregate so the frontend
    /// receives the initial session state via WebSocket immediately after
    /// session creation or load.
    async fn emit_snapshot_events(&self) {
        use aionui_api_types::{ModelInfoEntry, ModelInfoPayload};

        let session = self.session.read().await;
        if let Some(models) = session.model_info() {
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
            // ModelInfoPayload is our own struct but go through the
            // normaliser for consistency with sibling events.
            if let Some(v) = sdk_to_snake_value(&payload) {
                let _ = self.event_tx.send(AgentStreamEvent::AcpModelInfo(v));
            }
        }
        if let Some(modes) = session.modes()
            && let Some(v) = sdk_to_snake_value(&modes)
        {
            let _ = self.event_tx.send(AgentStreamEvent::AcpModeInfo(v));
        }
        if let Some(config_options) = session.config_options()
            && let Some(v) = sdk_to_snake_value(&serde_json::json!({
                "config_options": config_options,
            }))
        {
            // Wrap in `{config_options: [...]}` to match the SDK
            // `ConfigOptionUpdate` shape used by the streaming path —
            // handshake blobs and downstream consumers see a uniform
            // structure regardless of origin.
            let _ = self.event_tx.send(AgentStreamEvent::AcpConfigOption(v));
        }
        if let Some(cmds) = session.available_commands() {
            let _ = self
                .event_tx
                .send(AgentStreamEvent::AvailableCommands(AvailableCommandsEventData {
                    commands: cmds.to_vec(),
                }));
        }
    }

    /// Return available slash commands from the session aggregate.
    pub async fn load_slash_commands(&self) -> Result<Vec<SlashCommandItem>, AppError> {
        let session = self.session.read().await;
        let items = session
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
        self.session.read().await.session_id().map(ToOwned::to_owned)
    }

    /// Restore a previously persisted session_id (e.g. from DB on task rebuild).
    /// Enables resume path on next send_message instead of creating a fresh session.
    ///
    /// Deliberately leaves `opened = false`: the CLI child process is
    /// brand new and still needs `session/load` (or claude-meta-resume) to
    /// re-attach to the persisted session before the next prompt. Subsequent
    /// turns — once the resume handshake has run — take the short path.
    pub async fn restore_session_id(&self, sid: String) {
        let mut session = self.session.write().await;
        session.assign_session_id(DomainSessionId::new(sid));
        // Discarded — the session_id already came from DB, no need to re-persist.
        session.drain_events();
    }

    /// Vendor label this session was spawned as (e.g. "claude"), if any.
    pub fn backend(&self) -> Option<&str> {
        self.params.metadata.backend.as_deref()
    }

    /// Agent metadata id this session was spawned from.
    pub fn agent_metadata_id(&self) -> &str {
        &self.params.metadata.id
    }

    /// Whether the configured agent supports side questions.
    pub fn supports_side_question(&self) -> bool {
        self.params.metadata.behavior_policy.supports_side_question
    }

    /// Whether the agent supports `session/load` — read from the ACP
    /// handshake's `agent_capabilities.load_session` bool. `false` until
    /// initialization completes; `false` for agents that advertise no
    /// load-session capability.
    ///
    /// The raw ACP wire field is `loadSession` (camelCase); we store
    /// the snake_case form because every handshake blob is normalised
    /// before being persisted (see `sdk_to_snake_value`).
    /// Whether this agent uses Claude-style meta resume (session/new with
    /// `_meta.claudeCode.options.resume`) instead of session/load.
    /// Matches AionUi frontend: `useClaudeMetaResume = backend === 'claude' || !!caps?._meta?.claudeCode`
    fn uses_claude_meta_resume(&self) -> bool {
        agent_metadata_uses_claude_meta_resume(&self.params.metadata)
    }

    fn supports_session_load(&self) -> bool {
        self.params
            .metadata
            .handshake
            .agent_capabilities
            .as_ref()
            .and_then(|caps: &Value| caps.get("load_session"))
            .and_then(|v: &Value| v.as_bool())
            .unwrap_or(false)
    }

    fn native_skill_support(&self) -> bool {
        self.params
            .metadata
            .native_skills_dirs
            .as_ref()
            .is_some_and(|v: &Vec<String>| !v.is_empty())
    }

    /// Return the active session id or a `BadRequest` error.
    async fn require_session_id(&self) -> Result<String, AppError> {
        self.session
            .read()
            .await
            .session_id()
            .map(ToOwned::to_owned)
            .ok_or_else(|| AppError::BadRequest("No active session".into()))
    }
}

#[async_trait::async_trait]
impl crate::agent_task::IAgentTask for AcpAgentManager {
    fn agent_type(&self) -> AgentType {
        AgentType::Acp
    }

    fn conversation_id(&self) -> &str {
        &self.params.conversation_id
    }

    fn workspace(&self) -> &str {
        &self.params.workspace.path
    }

    fn status(&self) -> Option<ConversationStatus> {
        // Use try_read to avoid blocking; fall back to None if locked
        match self.status.try_read() {
            Ok(guard) => *guard,
            Err(_) => None,
        }
    }

    fn last_activity_at(&self) -> TimestampMs {
        self.last_activity.load(Ordering::Relaxed)
    }

    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.event_tx.subscribe()
    }

    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError> {
        self.last_activity.store(now_ms(), Ordering::Relaxed);

        let result = self.ensure_session_and_send(&data).await;
        match &result {
            Ok(()) => {
                let _ = self.event_tx.send(AgentStreamEvent::Finish(FinishEventData::default()));
            }
            Err(err) => {
                let _ = self.event_tx.send(AgentStreamEvent::Error(ErrorEventData {
                    message: err.to_string(),
                    code: None,
                }));
            }
        }
        result
    }

    async fn stop(&self) -> Result<(), AppError> {
        let session_id = self.session.read().await.session_id().map(ToOwned::to_owned);
        if let Some(sid) = session_id {
            self.protocol.cancel(CancelNotification::new(SessionId::new(sid)));
        }
        self.permission_router.cancel_all();

        Ok(())
    }

    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        info!(
            conversation_id = %self.params.conversation_id,
            ?reason,
            "Killing ACP agent"
        );

        // Mark closing to prevent reconnect attempts
        self.permission_router.set_closing();

        // Cancel the current session if active
        if let Ok(session) = self.session.try_read()
            && let Some(sid) = session.session_id()
        {
            self.protocol.cancel(CancelNotification::new(SessionId::new(sid)));
        }

        let process = Arc::clone(&self.process);
        let grace = Duration::from_millis(ACP_KILL_GRACE_MS);

        tokio::spawn(async move {
            if let Err(e) = process.kill(grace).await {
                error!(error = %e, "Failed to kill ACP process");
            }
        });

        self.permission_router.cancel_all();

        Ok(())
    }
}

/// ACP-specific operations that used to live on `IAgentManager` and are
/// now reached through `AgentInstance::Acp(..)` matches in the routes +
/// services. Kept as inherent methods so the enum-match callsite reads
/// `m.get_mode()` with no trait import.
impl AcpAgentManager {
    /// Submit a permission response for a pending tool call. ACP confirms
    /// always carry an `option_id`; `always_allow` is consumed by the CLI
    /// and is not reflected in the local approval memory (the ACP CLI
    /// tracks its own).
    pub fn confirm(
        &self,
        _msg_id: &str,
        call_id: &str,
        data: serde_json::Value,
        _always_allow: bool,
    ) -> Result<(), AppError> {
        let option_id = confirm_option_id(&data)
            .ok_or_else(|| AppError::BadRequest("ACP confirmation requires an option_id string".into()))?;

        self.permission_router
            .confirm(call_id, option_id, &self.params.conversation_id)
    }

    /// ACP tracks pending permission prompts through the permission
    /// router, not through a surfaced confirmation list, so the enum-
    /// level helper returns empty when the variant is ACP.
    pub fn get_confirmations(&self) -> Vec<Confirmation> {
        Vec::new()
    }

    /// Approval memory is not tracked at the manager level for ACP —
    /// every tool request round-trips through the CLI.
    pub fn check_approval(&self, _action: &str, _command_type: Option<&str>) -> bool {
        false
    }

    pub async fn get_mode(&self) -> Result<aionui_api_types::AgentModeResponse, AppError> {
        let desired = self
            .desired_mode()
            .await
            .map(|mode| normalize_requested_mode(&self.params.metadata, &mode))
            .filter(|mode| !mode.is_empty());
        Ok(aionui_api_types::AgentModeResponse {
            mode: self
                .modes()
                .await
                .map(|modes| modes.current_mode_id.to_string())
                .or(desired)
                .unwrap_or_else(|| normalize_requested_mode(&self.params.metadata, "default")),
            initialized: self.session_id().await.is_some(),
        })
    }

    pub async fn set_mode(&self, mode: &str) -> Result<(), AppError> {
        let normalized_mode = normalize_requested_mode(&self.params.metadata, mode);
        if normalized_mode.is_empty() {
            return Ok(());
        }
        let session_id = self.session.read().await.session_id().map(ToOwned::to_owned);

        if let Some(sid) = session_id {
            self.protocol
                .set_mode(SetSessionModeRequest::new(SessionId::new(sid), normalized_mode.clone()))
                .await
                .map_err(AppError::from)?;
            self.update_cached_mode(&normalized_mode).await;
            let mut session = self.session.write().await;
            session.apply_observed_mode(ModeId::new(&normalized_mode));
        }

        let mut session = self.session.write().await;
        session.set_desired_mode(ModeId::new(normalized_mode));
        self.commit_session_changes(&mut session).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
