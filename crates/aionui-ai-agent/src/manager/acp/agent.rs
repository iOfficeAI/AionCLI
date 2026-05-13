use crate::agent_runtime::AgentRuntime;
use crate::capability::cli_process::CliAgentProcess;
use crate::capability::first_message_injector::{InjectionConfig, inject_first_message_prefix};
use crate::capability::skill_manager::AcpSkillManager;
use crate::factory::acp_assembler::AcpSessionParams;
use crate::manager::acp::{AcpSession, AcpSessionEvent, PermissionRouter};
use crate::protocol::acp::AcpProtocol;
use crate::protocol::events::AgentStreamEvent;
use crate::registry::CatalogSender;
use crate::shared_kernel::{ModeId, ModelId, SessionId as DomainSessionId};
use crate::types::SendMessageData;
use agent_client_protocol::schema::{
    CancelNotification, SessionId, SessionModelState, SessionNotification, UsageUpdate,
};
use aionui_api_types::{AgentHandshake, SlashCommandItem};
use aionui_common::{
    AgentKillReason, AgentType, AppError, ConversationStatus, TimestampMs, normalize_keys_to_snake_case,
};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tracing::{error, info};

use super::mode_normalize::normalize_requested_mode;

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
pub(super) fn sdk_to_snake_value<T: serde::Serialize>(value: &T) -> Option<Value> {
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
    pub(super) params: Arc<AcpSessionParams>,

    /// Session aggregate root — owns desired/observed/advertised state.
    /// Single in-memory source of truth for session lifecycle, modes,
    /// models, config, and all runtime data previously split across
    /// `AcpRuntimeSnapshot` and `AcpState`.
    pub(super) session: RwLock<AcpSession>,

    /// Shared runtime holding status, last_activity, and the event
    /// broadcast channel. `pub(super)` so sibling modules (session_flow,
    /// event_tracker) can call `self.runtime.emit(...)` directly.
    ///
    /// Lifecycle: written by `IAgentTask::send_message` (Running →
    /// Finished/Error), `stop` (emit_finish), and `kill` (emit_error).
    /// `emit_finish` / `emit_error` are idempotent in the Finished
    /// absorbing state — multiple calls are safe.
    pub(super) runtime: AgentRuntime,

    /// ACP protocol handle (SDK connection).
    pub(super) protocol: AcpProtocol,

    /// Routes permission requests from the protocol layer to the user
    /// and back. Owns the receiver channel, pending map, and closing flag.
    pub(super) permission_router: Arc<PermissionRouter>,

    /// Shared skill manager — used to discover skills for first-message injection.
    pub(super) skill_manager: Arc<AcpSkillManager>,

    /// Domain event sender — session aggregate events are forwarded here
    /// for the persistence consumer (`AcpSessionSyncService`).
    pub(super) domain_event_tx: mpsc::Sender<AcpSessionEvent>,

    /// Underlying CLI process (for lifecycle management: kill, is_running).
    process: Arc<CliAgentProcess>,

    /// Mutex for serializing session operations (new/load/send).
    session_lock: Mutex<()>,
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
    pub async fn build(
        params: Arc<AcpSessionParams>,
        skill_manager: Arc<AcpSkillManager>,
        catalog_tx: &CatalogSender,
    ) -> Result<
        (
            Self,
            mpsc::Receiver<AcpSessionEvent>,
            mpsc::Receiver<SessionNotification>,
        ),
        AppError,
    > {
        let (this, domain_event_rx, notification_rx) = AcpAgentManager::new(params, skill_manager).await?;
        this.init(catalog_tx).await;
        Ok((this, domain_event_rx, notification_rx))
    }

    async fn new(
        params: Arc<AcpSessionParams>,
        skill_manager: Arc<AcpSkillManager>,
    ) -> Result<
        (
            Self,
            mpsc::Receiver<AcpSessionEvent>,
            mpsc::Receiver<SessionNotification>,
        ),
        AppError,
    > {
        let process = CliAgentProcess::spawn_for_sdk(params.command_spec.clone(), &params.data_dir).await?;
        let (stdin, stdout) = process
            .take_stdio()
            .await
            .ok_or_else(|| AppError::Internal("Failed to take stdio from CLI process".into()))?;

        // Dedicated channel for raw SDK SessionNotifications → session tracker.
        // This channel is separate from event_tx so the tracker never re-applies
        // events that were broadcast for the UI (e.g. from emit_snapshot_events).
        let (notification_tx, notification_rx) = mpsc::channel::<SessionNotification>(256);
        let (domain_event_tx, domain_event_rx) = mpsc::channel(256);
        let (permission_tx, permission_rx) = mpsc::channel(32);
        let runtime = AgentRuntime::new(params.conversation_id.clone(), params.workspace.path.clone(), 256);

        let protocol = AcpProtocol::connect(stdin, stdout, runtime.event_sender(), permission_tx, notification_tx)
            .await
            .map_err(|e| {
                error!(
                    conversation_id = %params.conversation_id,
                    error = %e,
                    "Failed to establish ACP protocol connection"
                );
                AppError::from(e)
            })?;
        let permission_router = Arc::new(PermissionRouter::new(permission_rx));

        let snapshot = params.session_snapshot.as_ref();

        // Prefer the last-persisted mode; for brand-new conversations
        // fall back to `AcpBuildExtra::session_mode` so the first turn
        // still honours the caller's choice.
        let (initial_mode, initial_model, initial_config) = (
            snapshot
                .and_then(|s| s.current_mode_id.as_ref())
                .map(|m| normalize_requested_mode(&params.metadata, m.as_str()))
                .or_else(|| {
                    params
                        .config
                        .session_mode
                        .as_ref()
                        .map(|m| normalize_requested_mode(&params.metadata, m))
                })
                .filter(|m| !m.is_empty())
                .map(ModeId::new),
            snapshot.and_then(|s| s.current_model_id.clone()),
            snapshot.map(|s| s.config_selections.clone()).unwrap_or_default(),
        );

        let session = AcpSession::new(initial_mode, initial_model, initial_config);

        let manager = Self {
            params,
            session: RwLock::new(session),
            runtime,
            process: Arc::new(process),
            protocol,
            session_lock: Mutex::new(()),
            permission_router,
            skill_manager,
            domain_event_tx,
        };
        Ok((manager, domain_event_rx, notification_rx))
    }

    async fn init(&self, catalog_tx: &CatalogSender) {
        let init_handshake = AgentHandshake {
            agent_capabilities: self.protocol.agent_capabilities().and_then(|c| sdk_to_snake_value(&c)),
            auth_methods: self.protocol.auth_methods().and_then(|m| sdk_to_snake_value(&m)),
            ..Default::default()
        };
        if init_handshake.agent_capabilities.is_some() || init_handshake.auth_methods.is_some() {
            catalog_tx.send_partial(self.params.metadata.id.clone(), init_handshake);
        }

        // Seed the observed/advertised layers (observed mode/model, cached
        // context_usage) from the persisted snapshot. Desired fields are
        // already populated via `AcpSession::new`.
        if let Some(snapshot) = self.params.session_snapshot.as_ref() {
            let mut session = self.session.write().await;
            session.preload_persisted(snapshot);
            // Preload did not come from the user this turn — drain so the
            // persistence consumer doesn't echo the DB back into itself.
            session.drain_events();
        }
        if let Some(agent_capabilities) = self.protocol.agent_capabilities() {
            let mut session = self.session.write().await;
            session.apply_advertised_capabilities(agent_capabilities);
        }
        if let Some(auth_methods) = self.protocol.auth_methods() {
            let mut session = self.session.write().await;
            session.apply_advertised_auth_methods(auth_methods);
        }
    }
}

impl AcpAgentManager {
    pub(crate) async fn mode(&self) -> Result<aionui_api_types::AgentModeResponse, AppError> {
        let desired = self
            .session
            .read()
            .await
            .desired_mode()
            .map(|mode| normalize_requested_mode(&self.params.metadata, mode))
            .filter(|mode| !mode.is_empty());
        Ok(aionui_api_types::AgentModeResponse {
            mode: self
                .session
                .read()
                .await
                .modes()
                .map(|modes| modes.current_mode_id.to_string())
                .or(desired)
                .unwrap_or_else(|| normalize_requested_mode(&self.params.metadata, "default")),
            initialized: self.session_id().await.is_some(),
        })
    }

    /// Cached model info from the ACP backend, if any has been received.
    pub(crate) async fn model(&self) -> Option<SessionModelState> {
        self.session.read().await.model_info().cloned()
    }

    /// Cached context usage info from the ACP backend.
    pub(crate) async fn usage(&self) -> Option<UsageUpdate> {
        self.session.read().await.context_usage().cloned()
    }

    /// Set the mode for the current session.
    pub(crate) async fn set_mode(&self, mode: &str) -> Result<(), AppError> {
        let normalized_mode = normalize_requested_mode(&self.params.metadata, mode);
        if normalized_mode.is_empty() {
            return Ok(());
        }
        let session_id = self.session.read().await.session_id().map(ToOwned::to_owned);

        // Write desired — the aggregate root's legitimate intent write-point.
        {
            let mut session = self.session.write().await;
            session.set_desired_mode(ModeId::new(&normalized_mode));
            self.commit_session_changes(&mut session).await;
        }

        // If a session is open, reconcile to the CLI. `reconcile_session`
        // is the sole call-site of `protocol.set_mode` and the sole
        // observed/advertised write-point — on success it calls
        // `apply_observed_mode`, which syncs both layers and emits
        // `ObservedModeSynced`. `get_mode()` reflects the change as soon
        // as the SDK call returns.
        if let Some(sid) = session_id {
            self.reconcile_session(&sid).await;
        }
        Ok(())
    }

    /// Set the model for the current session.
    ///
    /// Mirrors `set_mode`: writes user intent into the aggregate's Desired
    /// layer, then delegates to `reconcile_session` for the SDK call.
    /// `reconcile_session` is the sole call-site of `protocol.set_model` —
    /// it also handles the observed sync since the CLI does not emit a
    /// CurrentModelUpdate notification after `session/set_model`.
    pub(crate) async fn set_model(&self, model_id: &str) -> Result<(), AppError> {
        let session_id = self.session.read().await.session_id().map(ToOwned::to_owned);

        {
            let mut session = self.session.write().await;
            session.set_desired_model(ModelId::new(model_id));
            self.commit_session_changes(&mut session).await;
        }

        if let Some(sid) = session_id {
            self.reconcile_session(&sid).await;
        } else {
            return Err(AppError::BadRequest("No active session".into()));
        }
        Ok(())
    }

    /// Return available slash commands from the session aggregate.
    pub(crate) async fn load_slash_commands(&self) -> Result<Vec<SlashCommandItem>, AppError> {
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
}

impl AcpAgentManager {
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
    ///
    /// Also consumes the first-message injection flag: resumed sessions already
    /// have the preset_context baked into the prior conversation history.
    pub async fn set_session_id(&self, sid: String) {
        let mut session = self.session.write().await;
        session.set_session_id(DomainSessionId::new(sid));
        session.take_needs_first_message_injection();
        // Discarded — the session_id already came from DB, no need to re-persist.
        session.drain_events();
    }

    /// Vendor label this session was spawned as (e.g. "claude"), if any.
    pub fn backend(&self) -> Option<&str> {
        self.params.metadata.backend.as_deref()
    }

    /// Agent metadata id this session was spawned from.
    pub fn agent_id(&self) -> &str {
        &self.params.metadata.id
    }

    /// Whether the configured agent supports side questions.
    pub fn supports_side_question(&self) -> bool {
        self.params.metadata.behavior_policy.supports_side_question
    }
}

impl AcpAgentManager {
    /// Ensure the ACP session is opened with the CLI. Does not send a
    /// prompt. Returns the session id that subsequent prompts should use
    /// (may differ from the input when claude-meta-resume rewrites it).
    ///
    /// Three paths mirror `ensure_session_and_send`:
    /// 1. No sid at all → `open_session_new`
    /// 2. Sid present but CLI has not opened it (fresh task) → `open_session_resume`
    /// 3. Already opened → noop, return the existing sid
    async fn ensure_session_opened(&self) -> Result<String, AppError> {
        let _lock = self.session_lock.lock().await;

        let (session_id, opened) = {
            let s = self.session.read().await;
            (s.session_id().map(ToOwned::to_owned), s.is_opened())
        };

        let sid = match (session_id, opened) {
            (None, _) => self.open_session_new().await?,
            (Some(sid), false) => self.open_session_resume(&sid).await?,
            (Some(sid), true) => sid,
        };

        {
            let mut s = self.session.write().await;
            s.mark_opened();
            self.commit_session_changes(&mut s).await;
        }
        Ok(sid)
    }

    /// Initialize or resume a session, then send the user message.
    ///
    /// The first real user message is augmented with `[Assistant Rules]` /
    /// skill-index injection via a one-shot flag in `AcpSession`. This is
    /// independent of whether `warmup` already opened the session, avoiding
    /// a race where warmup marks the session opened before the first message
    /// arrives.
    async fn ensure_session_and_send(&self, data: &SendMessageData) -> Result<(), AppError> {
        let sid = self.ensure_session_opened().await?;
        self.runtime.transition_to(ConversationStatus::Running);

        let needs_injection = {
            let mut s = self.session.write().await;
            s.take_needs_first_message_injection()
        };

        if needs_injection {
            let injected_content = inject_first_message_prefix(
                &data.content,
                &self.skill_manager,
                InjectionConfig {
                    preset_context: self.params.preset_context.as_deref(),
                    skills: &self.params.config.skills,
                    custom_workspace: self.params.workspace.is_custom,
                    native_skill_support: self
                        .params
                        .metadata
                        .native_skills_dirs
                        .as_ref()
                        .is_some_and(|v: &Vec<String>| !v.is_empty()),
                },
            )
            .await;
            let injected = SendMessageData {
                content: injected_content,
                ..data.clone()
            };
            self.prompt_existing_session(&injected, Some(&sid)).await
        } else {
            self.prompt_existing_session(data, Some(&sid)).await
        }
    }

    /// Pre-open the ACP session without sending a prompt. Called by the
    /// factory after `AcpAgentManager::build` so `POST /warmup` returns
    /// only after the session is ready to accept `set_mode` / `set_model`
    /// / `prompt`. Idempotent — if already opened, returns immediately.
    pub async fn warmup_session(&self) -> Result<(), AppError> {
        self.ensure_session_opened().await.map(|_sid| ())
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
        self.runtime.status()
    }

    fn last_activity_at(&self) -> TimestampMs {
        self.runtime.last_activity_at()
    }

    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.runtime.subscribe()
    }

    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError> {
        self.runtime.bump_activity();

        let result = self.ensure_session_and_send(&data).await;
        match &result {
            Ok(()) => {
                // ACP pattern: Finish with session_id = None (default).
                // If ACP later wants to include the session_id in Finish,
                // read it from `self.session.read().await.session_id()`.
                self.runtime.emit_finish(None);
            }
            Err(err) => {
                self.runtime.emit_error(err.to_string());
            }
        }
        result
    }

    async fn cancel(&self) -> Result<(), AppError> {
        let session_id = self.session.read().await.session_id().map(ToOwned::to_owned);
        if let Some(sid) = session_id {
            self.protocol.cancel(CancelNotification::new(SessionId::new(sid)));
        }
        self.permission_router.cancel_all();

        // m1 fix: mark task as Finished on explicit stop, so external
        // status() observers see a consistent terminal state. Idempotent —
        // if send_message already emitted Finish, this is a no-op.
        self.runtime.emit_finish(None);
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

        // m1 fix: emit error with the kill reason so the status goes to
        // Finished and subscribers see a terminal event. Idempotent.
        let message = match reason {
            Some(AgentKillReason::IdleTimeout) => "Agent killed: idle timeout".to_owned(),
            Some(AgentKillReason::TeamMcpRebuild) => "Agent killed: team MCP rebuild".to_owned(),
            Some(AgentKillReason::TeamDeleted) => "Agent killed: team deleted".to_owned(),
            None => "Agent killed".to_owned(),
        };
        self.runtime.emit_error(message);

        Ok(())
    }
}

impl AcpAgentManager {
    pub fn kill_and_wait(
        &self,
        reason: Option<AgentKillReason>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let _ = crate::agent_task::IAgentTask::kill(self, reason);
        let process = Arc::clone(&self.process);
        let grace = Duration::from_millis(ACP_KILL_GRACE_MS);
        Box::pin(async move {
            let _ = process.kill(grace).await;
        })
    }

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
}
