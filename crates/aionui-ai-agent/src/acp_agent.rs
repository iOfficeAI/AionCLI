use crate::IAgentManager;
use crate::acp_protocol::{AcpProtocol, PermissionDecision, PermissionRequest};
use crate::acp_runtime_snapshot::AcpRuntimeSnapshot;
use crate::acp_runtime_snapshot::PersistedSessionState;
use crate::agent_registry::CatalogSender;
use crate::cli_process::CliAgentProcess;
use crate::first_message_injector::{InjectionConfig, inject_first_message_prefix};
use crate::skill_manager::AcpSkillManager;
use crate::stream_event::{
    AgentStreamEvent, AvailableCommandsEventData, FinishEventData, SessionAssignedEventData, StartEventData,
    permission_request_to_event_data,
};
use crate::team_guide_prompt;
use crate::types::{AcpBuildExtra, AgentStreamChunk, SendMessageData, SlashCommandItem};
use agent_client_protocol::schema::{
    AgentCapabilities, AvailableCommand, CancelNotification, ContentBlock, EnvVariable, LoadSessionRequest, McpServer,
    McpServerStdio, NewSessionRequest, PromptRequest, SessionConfigKind, SessionConfigOption, SessionId,
    SessionModeState, SessionModelState, SetSessionConfigOptionRequest, SetSessionModeRequest, SetSessionModelRequest,
    UsageUpdate,
};
use aionui_api_types::{AgentHandshake, AgentMetadata};
use aionui_api_types::{GuideMcpConfig, TeamMcpStdioConfig};
use aionui_common::{
    AgentKillReason, AgentType, AppError, CommandSpec, Confirmation, ConversationStatus, TimestampMs,
    normalize_keys_to_snake_case, now_ms,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc, oneshot};
use tracing::{debug, error, info};

/// Grace period before force-killing an ACP process (ms).
const ACP_KILL_GRACE_MS: u64 = 500;

fn normalize_requested_mode(metadata: &AgentMetadata, mode: &str) -> String {
    let trimmed = mode.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // AionUi persists the legacy aliases `yolo` / `yoloNoSandbox` while
    // ACP backends expect their native mode id (e.g. `full-access` for
    // Codex). Resolution is data-driven: the mapping lives on each
    // catalog row's top-level `yolo_id` column. Backends without a
    // `yolo_id` have no equivalent, so the alias passes through
    // unchanged and `session/set_mode` gets the caller's original
    // value.
    if matches!(trimmed, "yolo" | "yoloNoSandbox")
        && let Some(native) = metadata.yolo_id.as_deref()
    {
        return native.to_owned();
    }

    // Codex has legacy `default`/`autoEdit` aliases that map to its
    // native `auto` mode. Keep the mapping data-driven by keying on the
    // vendor backend label rather than re-introducing an AcpBackend
    // enum variant.
    if matches!(metadata.backend.as_deref(), Some("codex")) && matches!(trimmed, "default" | "autoEdit") {
        return "auto".to_owned();
    }

    trimmed.to_owned()
}

/// Whether the agent described by `metadata` uses Claude-style meta resume
/// (`session/new` with `_meta.claudeCode.options.resume`) instead of the
/// generic `session/load` path.
///
/// Mirrors the AionUi frontend rule
/// `useClaudeMetaResume = backend === 'claude' || !!caps?._meta?.claudeCode`.
///
/// Handshake blobs persisted by the backend are normalised to snake_case
/// (see `sdk_to_snake_value`), so the lookup prefers `claude_code` and
/// falls back to `claudeCode` for any blob that bypassed normalisation.
fn agent_metadata_uses_claude_meta_resume(metadata: &AgentMetadata) -> bool {
    if metadata.backend.as_deref() == Some("claude") {
        return true;
    }
    metadata
        .handshake
        .agent_capabilities
        .as_ref()
        .and_then(|caps| caps.get("_meta"))
        .and_then(|meta| meta.get("claude_code").or_else(|| meta.get("claudeCode")))
        .is_some()
}

/// Extract the `current_value` string from a `SessionConfigKind` —
/// currently only the Select kind has a string value we can replay
/// through `session/set_session_config_option`. Boolean toggles and
/// other future kinds return `None` and are skipped by the replay
/// path.
fn extract_config_current_value(kind: &SessionConfigKind) -> Option<String> {
    match kind {
        SessionConfigKind::Select(sel) => Some(sel.current_value.to_string()),
        _ => None,
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

/// Compose the first-message preset context for a solo ACP agent, optionally
/// prepending the Team Guide prompt (W5-D28b).
///
/// The guide is injected iff **both** conditions hold:
/// 1. The session is not already part of a team (`team_mcp_stdio_config`
///    absent on [`AcpBuildExtra`]) — in-team agents receive a different
///    coordination prompt via their team MCP bridge, and must not be told to
///    propose a fresh team from inside one.
/// 2. The backend is on the solo-guide whitelist (see
///    [`team_guide_prompt::is_solo_team_guide_backend`]). Unknown backends
///    receive no guide, matching AionUi's `opts.backend`-gated solo
///    injection.
///
/// When the guide is added it is appended after any pre-existing
/// `preset_context` so user-configured rules still take precedence at the
/// top of the `[Assistant Rules]` block assembled by
/// [`crate::first_message_injector::inject_first_message_prefix`].
fn compose_preset_context_with_team_guide(
    base_preset_context: Option<&str>,
    backend: Option<&str>,
    has_team_session: bool,
) -> Option<String> {
    let base = base_preset_context
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    if has_team_session {
        return base;
    }
    let backend_key = backend.unwrap_or_default();
    if !team_guide_prompt::is_solo_team_guide_backend(backend_key) {
        return base;
    }

    let guide = team_guide_prompt::build_solo_team_guide_prompt(backend_key);
    match base {
        Some(ctx) => Some(format!("{ctx}\n\n{guide}")),
        None => Some(guide),
    }
}

/// Backends for which solo conversations receive the Guide MCP server
/// (W5 team-capable whitelist). Duplicated locally rather than imported
/// from `aionui_team::guide::capability` because `aionui-team` already
/// depends on this crate (see `team_mcp_server` below for the same
/// rationale). The two lists must stay in sync until one side is hoisted
/// into a shared crate.
const TEAM_CAPABLE_BACKENDS: &[&str] = &["claude", "codex", "gemini", "aionrs"];

/// Build a `NewSessionRequest` for `session/new`, injecting the team MCP
/// stdio server when `config.team_mcp_stdio_config` is present, or the
/// Guide MCP server when the conversation is solo but the backend is
/// team-capable.
///
/// The two injections are mutually exclusive: a team session already has
/// its own MCP server and does not need the Guide. A solo session on a
/// non-team-capable backend gets neither, matching the legacy single-chat
/// payload byte-for-byte.
///
/// The team MCP server injected here uses HTTP transport (see
/// `team_mcp_server` below) — the `backend_binary_path` / `mcp-bridge`
/// stdio shape described in phase1 interface-contracts §3 is used only
/// by the bridge subprocess in `aionui-team`, not by this path.
///
/// W5-D28c ships the guard only — the Guide MCP server itself (config
/// type, port/token resolution) lands with D26a/D27, after which the
/// `// TODO(D26a)` branch here will push an `McpServer::Http` onto
/// `mcp_servers` instead of falling through.
fn build_new_session_request(workspace: &str, config: &AcpBuildExtra) -> NewSessionRequest {
    let req = NewSessionRequest::new(workspace);
    if let Some(cfg) = config.team_mcp_stdio_config.as_ref() {
        return req.mcp_servers(vec![team_mcp_server(cfg)]);
    }
    // Solo: inject Guide MCP stdio server for team-capable backends
    if let Some(guide_cfg) = config.guide_mcp_config.as_ref()
        && config
            .backend
            .as_deref()
            .is_some_and(|b| TEAM_CAPABLE_BACKENDS.contains(&b))
    {
        return req.mcp_servers(vec![guide_mcp_server(guide_cfg, config)]);
    }
    req
}

/// Translate a `TeamMcpStdioConfig` into the ACP SDK wire type expected by
/// `NewSessionRequest::mcp_servers`.
///
/// Field shapes must stay byte-for-byte identical to
/// `aionui_team::mcp::bridge::TeamMcpStdioServerSpec::into_sdk` — the
/// logic is inlined here rather than reused because `aionui-team` already
/// depends on this crate, so importing the spec would cycle. Both sides
/// derive `name` from `cfg.team_id` (phase1 interface-contracts §3).
fn team_mcp_server(cfg: &TeamMcpStdioConfig) -> McpServer {
    let env = vec![
        EnvVariable::new(TeamMcpStdioConfig::ENV_PORT.to_owned(), cfg.port.to_string()),
        EnvVariable::new(TeamMcpStdioConfig::ENV_TOKEN.to_owned(), cfg.token.clone()),
        EnvVariable::new(TeamMcpStdioConfig::ENV_SLOT_ID.to_owned(), cfg.slot_id.clone()),
    ];
    let stdio = McpServerStdio::new(format!("aionui-team-{}", cfg.team_id), &cfg.binary_path)
        .args(vec!["mcp-team-stdio".to_owned()])
        .env(env);
    McpServer::Stdio(stdio)
}

/// Build a stdio `McpServer` for the Guide MCP server in solo team-capable sessions.
fn guide_mcp_server(cfg: &GuideMcpConfig, extra: &AcpBuildExtra) -> McpServer {
    let env = vec![
        EnvVariable::new("AION_MCP_PORT".to_owned(), cfg.port.to_string()),
        EnvVariable::new("AION_MCP_TOKEN".to_owned(), cfg.token.clone()),
        EnvVariable::new("AION_MCP_BACKEND".to_owned(), extra.backend.clone().unwrap_or_default()),
    ];
    let stdio = McpServerStdio::new("aionui-team-guide", &cfg.binary_path)
        .args(vec!["mcp-guide-stdio".to_owned()])
        .env(env);
    McpServer::Stdio(stdio)
}

/// Internal state that changes at runtime.
struct AcpState {
    /// Current conversation status.
    status: Option<ConversationStatus>,
    /// Active session ID (set after session/new or session/load).
    session_id: Option<String>,
    /// Whether **this `AcpAgentManager` instance** has opened the session with
    /// the CLI — either through `session/new` (first turn) or through
    /// `session/load` / claude-meta-resume (recovering a persisted id). Once
    /// set, subsequent turns in the same process go through the short-path
    /// `prompt_existing_session` instead of re-loading.
    ///
    /// This is NOT the same as "the conversation has prior messages in the
    /// DB" — a task rebuild (idle cleanup, crash recovery, etc.) starts
    /// with `session_opened = false` even when a persisted `session_id`
    /// has already been restored, because the CLI child process is brand
    /// new and still needs the load/resume handshake on its very first
    /// prompt after rebuild.
    session_opened: bool,
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

/// Project an `AgentStreamEvent` onto the subset of `AgentHandshake`
/// fields the catalog cares about. Returns `None` for unrelated
/// events — the forwarder filters on that.
///
/// Event payloads may arrive here either already snake_case (from
/// `emit_snapshot_events`) or camelCase (from `SessionUpdate::*`
/// translation in `stream_event.rs`). We re-normalise unconditionally
/// so the persisted handshake blob is uniform; `camel_to_snake` is
/// idempotent on snake_case input.
fn catalog_partial_from_event(event: &AgentStreamEvent) -> Option<AgentHandshake> {
    fn snake(mut v: Value) -> Value {
        normalize_keys_to_snake_case(&mut v);
        v
    }
    match event {
        AgentStreamEvent::AcpModeInfo(v) => Some(AgentHandshake {
            available_modes: Some(snake(v.clone())),
            ..Default::default()
        }),
        AgentStreamEvent::AcpModelInfo(v) => Some(AgentHandshake {
            available_models: Some(snake(v.clone())),
            ..Default::default()
        }),
        AgentStreamEvent::AcpConfigOption(v) => Some(AgentHandshake {
            config_options: Some(snake(v.clone())),
            ..Default::default()
        }),
        AgentStreamEvent::AvailableCommands(data) => {
            // `AvailableCommand` is an ACP SDK struct — normalise on
            // the way into the catalog so the stored blob is snake_case.
            let cmds = sdk_to_snake_value(&data.commands)?;
            Some(AgentHandshake {
                available_commands: Some(cmds),
                ..Default::default()
            })
        }
        _ => None,
    }
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
    /// Snapshot of the `agent_metadata` row this session was spawned
    /// from. Captured once at construction — the catalog row does not
    /// change during a session's lifetime, so we read it locally
    /// instead of round-tripping through the registry on every call.
    metadata: AgentMetadata,
    /// Handle used to push partial handshake updates back into the
    /// catalog. The consumer task lives inside the registry.
    catalog_tx: CatalogSender,
    /// Build configuration (preset context, enabled/excluded skills, session mode, …).
    config: AcpBuildExtra,
    /// Preferred session mode to apply on the next session initialization.
    preferred_mode: RwLock<Option<String>>,
    /// Underlying CLI process (for lifecycle management: kill, is_running).
    process: Arc<CliAgentProcess>,
    /// ACP protocol handle (SDK connection).
    protocol: AcpProtocol,
    /// Typed event broadcast channel.
    event_tx: broadcast::Sender<AgentStreamEvent>,
    /// Raw stream chunk broadcast channel consumed by the team scheduler's
    /// wake-timeout watchdog. Emission points are wired up in W4-D25c-2;
    /// this channel exists from D25c-1 onward so `subscribe_stream` can
    /// hand out live receivers regardless of whether emitters are active.
    stream_tx: broadcast::Sender<AgentStreamChunk>,
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
    skill_manager: Arc<AcpSkillManager>,
}

impl AcpAgentManager {
    /// Current session mode id. Falls back to the configured session mode,
    /// then to `"default"`. Reading a cached snapshot is infallible.
    pub async fn modes(&self) -> Option<SessionModeState> {
        let snapshot = self.runtime_snapshot.read().await;
        snapshot.modes().cloned()
    }

    async fn preferred_mode(&self) -> Option<String> {
        self.preferred_mode.read().await.clone().filter(|mode| !mode.is_empty())
    }

    async fn update_cached_mode(&self, mode: &str) {
        let mut snapshot = self.runtime_snapshot.write().await;
        if let Some(modes) = snapshot.modes().cloned() {
            snapshot.set_modes(SessionModeState::new(mode.to_owned(), modes.available_modes));
        }
    }

    /// Restore user-chosen config option values on top of the CLI's
    /// freshly returned defaults.
    ///
    /// The CLI treats every `session/new` and `session/load` reply as a
    /// source of truth for the `config_options` schema (labels, enum
    /// values, ordering), but it does not remember the user's previous
    /// selection in AionUi. We persist those selections in
    /// `acp_session.session_config.runtime.config_selections`, preload
    /// them into the snapshot, and here — once a session id is live —
    /// replay any that diverge from the CLI's current value through
    /// `session/set_session_config_option`.
    ///
    /// Best-effort: a per-option failure is logged and skipped so the
    /// user still gets a usable session instead of a hard error.
    async fn apply_preferred_config_selections(&self, session_id: &str) {
        let (selections, cli_options) = {
            let snapshot = self.runtime_snapshot.read().await;
            (
                snapshot.config_selections().clone(),
                snapshot
                    .config_options()
                    .map(<[SessionConfigOption]>::to_vec)
                    .unwrap_or_default(),
            )
        };
        if selections.is_empty() {
            return;
        }

        for option in &cli_options {
            let cid = option.id.to_string();
            let Some(desired) = selections.get(&cid) else {
                continue;
            };
            let current = extract_config_current_value(&option.kind);
            if current.as_deref() == Some(desired.as_str()) {
                continue;
            }
            if let Err(err) = self
                .protocol
                .set_config_option(SetSessionConfigOptionRequest::new(
                    SessionId::new(session_id),
                    cid.clone(),
                    desired.clone(),
                ))
                .await
            {
                info!(
                    config_id = %cid,
                    desired = %desired,
                    error = %err,
                    "apply_preferred_config_selections: set_config_option failed; skipping"
                );
            }
        }
    }

    async fn apply_preferred_mode(&self, session_id: &str) -> Result<(), AppError> {
        let Some(mode) = self.preferred_mode().await else {
            return Ok(());
        };
        let normalized_mode = normalize_requested_mode(&self.metadata, &mode);
        if normalized_mode.is_empty() {
            return Ok(());
        }

        let current_mode = {
            let snapshot = self.runtime_snapshot.read().await;
            snapshot.current_mode_id()
        };

        if current_mode.as_deref() == Some(normalized_mode.as_str()) {
            return Ok(());
        }

        self.protocol
            .set_mode(SetSessionModeRequest::new(
                SessionId::new(session_id),
                normalized_mode.clone(),
            ))
            .await
            .map_err(AppError::from)?;

        self.update_cached_mode(&normalized_mode).await;
        let mut preferred_mode = self.preferred_mode.write().await;
        *preferred_mode = Some(normalized_mode);
        Ok(())
    }

    async fn _set_modes(&self, mode: &str) -> Result<(), AppError> {
        let sid = self.require_session_id().await?;
        self.protocol
            .set_mode(SetSessionModeRequest::new(SessionId::new(sid), mode.to_owned()))
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
            .set_model(SetSessionModelRequest::new(SessionId::new(sid), model_id.to_owned()))
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
    /// `command_spec` already points at the resolved binary — the factory
    /// reads it off `metadata` and passes it in here. `metadata` is a
    /// snapshot of the `agent_metadata` row captured by the factory; it
    /// never changes over this session's lifetime. `catalog_tx` is the
    /// MPSC sender the manager uses for both the one-shot initialize
    /// handshake write and the session-driven forwarder.
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        conversation_id: String,
        metadata: AgentMetadata,
        workspace: String,
        is_custom_workspace: bool,
        command_spec: CommandSpec,
        config: AcpBuildExtra,
        skill_manager: Arc<AcpSkillManager>,
        catalog_tx: CatalogSender,
    ) -> Result<Self, AppError> {
        let process = CliAgentProcess::spawn_for_sdk(command_spec).await?;

        // Take raw stdio for the SDK transport
        let (stdin, stdout) = process
            .take_stdio()
            .await
            .ok_or_else(|| AppError::Internal("Failed to take stdio from CLI process".into()))?;

        let (event_tx, _) = broadcast::channel(256);
        let (stream_tx, _) = broadcast::channel(256);
        let (permission_tx, permission_rx) = mpsc::channel(32);

        // Connect via ACP SDK — executes initialize handshake
        let protocol = AcpProtocol::connect(stdin, stdout, event_tx.clone(), stream_tx.clone(), permission_tx)
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

        // Push the static handshake payloads (agent_capabilities +
        // auth_methods) through the catalog sync channel. Session-driven
        // fields — modes, models, config_options, commands — flow
        // through the forwarder started in `start_catalog_sync`.
        let init_handshake = AgentHandshake {
            agent_capabilities: protocol.agent_capabilities().and_then(|c| sdk_to_snake_value(&c)),
            auth_methods: protocol.auth_methods().and_then(|m| sdk_to_snake_value(&m)),
            ..Default::default()
        };
        if init_handshake.agent_capabilities.is_some() || init_handshake.auth_methods.is_some() {
            catalog_tx.send_partial(metadata.id.clone(), init_handshake);
        }

        let manager = Self {
            conversation_id,
            workspace,
            is_custom_workspace,
            metadata,
            catalog_tx,
            preferred_mode: RwLock::new(config.session_mode.clone()),
            config,
            process: Arc::new(process),
            protocol,
            event_tx,
            stream_tx,
            state: RwLock::new(AcpState {
                status: None,
                session_id: None,
                session_opened: false,
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
                    && let Some(response_tx) = this.pending_permissions.lock().unwrap().remove(&call_id)
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

    /// Forward session-driven ACP events into the catalog sync channel
    /// so the `agent_metadata` row stays in sync with what the CLI is
    /// actually reporting. Runs as a subscriber on this manager's
    /// broadcast bus; the registry owns the single consumer that drains
    /// the resulting MPSC.
    pub fn start_catalog_sync(self: &Arc<Self>) {
        let id = self.metadata.id.clone();
        let sender = self.catalog_tx.clone();
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut rx = this.event_tx.subscribe();
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if let Some(partial) = catalog_partial_from_event(&event) {
                            sender.send_partial(id.clone(), partial);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        });
    }

    /// Seed the runtime snapshot with the user's last choices. Called
    /// by `ConversationService` on resume paths, before dispatching
    /// `send_message`. `None` fields are ignored — the CLI's
    /// `session/load` response fills in whatever the preload omits.
    pub async fn preload_snapshot(&self, state: PersistedSessionState) {
        let mut snapshot = self.runtime_snapshot.write().await;
        snapshot.preload_persisted(state);
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

        let state = self.state.read().await;
        let session_id = state.session_id.clone();
        let session_opened = state.session_opened;
        drop(state);

        match (session_id.as_deref(), session_opened) {
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

        let mut state = self.state.write().await;
        state.session_opened = true;
        state.status = Some(ConversationStatus::Running);

        Ok(())
    }

    /// Create a new ACP session and send the first prompt.
    async fn session_new_and_prompt(&self, data: &SendMessageData) -> Result<(), AppError> {
        // Emit Start event
        let _ = self
            .event_tx
            .send(AgentStreamEvent::Start(StartEventData { session_id: None }));

        let req = build_new_session_request(&self.workspace, &self.config);
        tracing::info!(
            has_team_mcp = self.config.team_mcp_stdio_config.is_some(),
            has_guide_mcp = self.config.guide_mcp_config.is_some(),
            guide_mcp_port = self.config.guide_mcp_config.as_ref().map(|c| c.port),
            mcp_servers_count = req.mcp_servers.len(),
            "session_new_and_prompt: sending session/new"
        );
        let session_response = self.protocol.new_session(req).await.map_err(AppError::from)?;

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

        // Notify subscribers (e.g. AcpAgentService) so the new id is
        // persisted into `acp_session.session_id` — resume can then
        // choose `session/load` instead of a fresh `session/new`.
        let _ = self
            .event_tx
            .send(AgentStreamEvent::SessionAssigned(SessionAssignedEventData {
                session_id: sid.clone(),
            }));

        if let Err(e) = self.apply_preferred_mode(&sid).await {
            tracing::error!(
                conversation_id = %self.conversation_id,
                error = %e,
                "failed to apply preferred mode, continuing with default"
            );
        }
        self.apply_preferred_config_selections(&sid).await;

        // Inject first-message prefix (preset context + skills index).
        // Backends with native skill discovery (e.g. Claude via .claude/skills/)
        // only need preset_context here; others get the full [Assistant Rules]
        // block with a skills index.
        //
        // Before handing off, (W5-D28b) prepend the solo Team Guide prompt to
        // `preset_context` when this agent is *not* part of a team and the
        // backend is on the solo-guide whitelist. Agents inside a team
        // (non-empty `team_mcp_stdio_config`) are deliberately excluded —
        // they coordinate through the team MCP bridge and must not receive
        // the "propose a team" prompt.
        let composed_preset_context = compose_preset_context_with_team_guide(
            self.config.preset_context.as_deref(),
            self.backend(),
            self.config.team_mcp_stdio_config.is_some(),
        );
        let injected_content = inject_first_message_prefix(
            &data.content,
            &self.skill_manager,
            InjectionConfig {
                preset_context: composed_preset_context.as_deref(),
                skills: &self.config.skills,
                native_skill_support: self.native_skill_support(),
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
        let _ = self
            .event_tx
            .send(AgentStreamEvent::Finish(FinishEventData { session_id: Some(sid) }));

        Ok(())
    }

    /// Resume an existing session and send a message.
    ///
    /// Assumes `preload_snapshot` has already been called by the
    /// caller (conversation service) on resume paths — the snapshot
    /// may therefore already carry `current_mode_id` / `current_model_id`
    /// from `acp_session.session_config.runtime`. When the CLI's
    /// `session/load` response arrives, we merge it in but keep the
    /// preloaded `current_*` values because they reflect the user's
    /// last explicit choice; the CLI's own `current_*` is only used
    /// when the snapshot has nothing yet.
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

                let req = build_new_session_request(&self.workspace, &self.config).meta(meta);

                info!(
                    session_id = %sid,
                    has_team_mcp = self.config.team_mcp_stdio_config.is_some(),
                    has_guide_mcp = self.config.guide_mcp_config.is_some(),
                    guide_mcp_port = self.config.guide_mcp_config.as_ref().map(|c| c.port),
                    mcp_servers_count = req.mcp_servers.len(),
                    "session_resume: using session/new with claudeCode.options.resume"
                );

                let session_response = self.protocol.new_session(req).await.map_err(AppError::from)?;

                let new_sid = session_response.session_id.to_string();
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

                let mut state = self.state.write().await;
                state.session_id = Some(new_sid.clone());
                drop(state);

                // Re-apply the user's preferred mode: the CLI resets
                // `currentModeId` to its own default on every resume
                // handshake (Claude meta-resume rebuilds the session), so
                // without this the mode the user had set (e.g.
                // `bypassPermissions`) silently downgrades to `default`
                // and the CLI starts prompting for permissions again.
                if let Err(e) = self.apply_preferred_mode(&new_sid).await {
                    tracing::error!(
                        conversation_id = %self.conversation_id,
                        error = %e,
                        "failed to re-apply preferred mode after meta-resume"
                    );
                }
                self.apply_preferred_config_selections(&new_sid).await;
                return self.prompt_existing_session(data, Some(&new_sid)).await;
            }
        } else if self.supports_session_load()
            && let Some(sid) = session_id
        {
            // Non-Claude backends (e.g. Codex): use session/load
            let (preloaded_mode, preloaded_model) = {
                let snapshot = self.runtime_snapshot.read().await;
                (
                    snapshot.modes().map(|m| m.current_mode_id.to_string()),
                    snapshot.model_info().map(|m| m.current_model_id.to_string()),
                )
            };

            let resp = self
                .protocol
                .load_session(LoadSessionRequest::new(SessionId::new(sid), &self.workspace))
                .await
                .map_err(AppError::from)?;

            let mut snapshot = self.runtime_snapshot.write().await;
            if let Some(mut models) = resp.models {
                if let Some(db_current) = preloaded_model {
                    models.current_model_id = db_current.into();
                }
                snapshot.set_model_info(models);
            }
            if let Some(mut modes) = resp.modes {
                if let Some(db_current) = preloaded_mode {
                    modes.current_mode_id = db_current.into();
                }
                snapshot.set_modes(modes);
            }
            if let Some(config_options) = resp.config_options {
                snapshot.set_config_options(config_options);
            }
        }

        self.emit_snapshot_events().await;

        if let Some(sid) = session_id {
            // Same reasoning as the Claude meta-resume branch above:
            // `session/load` returns the CLI's own `currentModeId`
            // (usually `default`), so re-apply the user's preferred
            // mode before prompting.
            if let Err(e) = self.apply_preferred_mode(sid).await {
                tracing::error!(
                    conversation_id = %self.conversation_id,
                    error = %e,
                    "failed to re-apply preferred mode after session/load"
                );
            }
            self.apply_preferred_config_selections(sid).await;
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
            // ModelInfoPayload is our own struct but go through the
            // normaliser for consistency with sibling events.
            if let Some(v) = sdk_to_snake_value(&payload) {
                let _ = self.event_tx.send(AgentStreamEvent::AcpModelInfo(v));
            }
        }
        if let Some(modes) = snapshot.modes()
            && let Some(v) = sdk_to_snake_value(&modes)
        {
            let _ = self.event_tx.send(AgentStreamEvent::AcpModeInfo(v));
        }
        if let Some(config_options) = snapshot.config_options()
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
        if let Some(cmds) = snapshot.available_commands() {
            let _ = self
                .event_tx
                .send(AgentStreamEvent::AvailableCommands(AvailableCommandsEventData {
                    commands: cmds.to_vec(),
                }));
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

    /// Restore a previously persisted session_id (e.g. from DB on task rebuild).
    /// Enables resume path on next send_message instead of creating a fresh session.
    ///
    /// Deliberately leaves `session_opened = false`: the CLI child process is
    /// brand new and still needs `session/load` (or claude-meta-resume) to
    /// re-attach to the persisted session before the next prompt. Subsequent
    /// turns — once the resume handshake has run — take the short path.
    pub async fn restore_session_id(&self, sid: String) {
        let mut state = self.state.write().await;
        state.session_id = Some(sid);
        state.session_opened = false;
    }

    /// Vendor label this session was spawned as (e.g. "claude"), if any.
    pub fn backend(&self) -> Option<&str> {
        self.metadata.backend.as_deref()
    }

    /// Agent metadata id this session was spawned from.
    pub fn agent_metadata_id(&self) -> &str {
        &self.metadata.id
    }

    /// Whether the configured agent supports side questions.
    pub fn supports_side_question(&self) -> bool {
        self.metadata.behavior_policy.supports_side_question
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
        agent_metadata_uses_claude_meta_resume(&self.metadata)
    }

    fn supports_session_load(&self) -> bool {
        self.metadata
            .handshake
            .agent_capabilities
            .as_ref()
            .and_then(|caps| caps.get("load_session"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    fn native_skill_support(&self) -> bool {
        self.metadata.native_skills_dirs.as_ref().is_some_and(|v| !v.is_empty())
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
impl IAgentManager for AcpAgentManager {
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

    fn subscribe_stream(&self) -> broadcast::Receiver<AgentStreamChunk> {
        self.stream_tx.subscribe()
    }

    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError> {
        self.last_activity.store(now_ms(), Ordering::Relaxed);

        // Drive the session, then emit a terminal chunk so subscribers
        // (wake timeout watchdog, crash detector) always see a Finish or
        // Error at the end of every turn — matching the contract documented
        // on `AgentStreamChunk`.
        let result = self.ensure_session_and_send(&data).await;
        match &result {
            Ok(()) => {
                let _ = self.stream_tx.send(AgentStreamChunk::Finish {
                    agent_crash: false,
                    stop_reason: None,
                });
            }
            Err(err) => {
                let _ = self.stream_tx.send(AgentStreamChunk::Error {
                    message: err.to_string(),
                });
            }
        }
        result
    }

    async fn stop(&self) -> Result<(), AppError> {
        let session_id = self.state.read().await.session_id.clone();
        if let Some(sid) = session_id {
            self.protocol.cancel(CancelNotification::new(SessionId::new(sid)));
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
        let option_id = confirm_option_id(&data)
            .ok_or_else(|| AppError::BadRequest("ACP confirmation requires an option_id string".into()))?;

        let responder = self
            .pending_permissions
            .lock()
            .unwrap()
            .remove(call_id)
            .ok_or_else(|| AppError::BadRequest(format!("Pending ACP permission not found: {call_id}")))?;

        responder
            .send(PermissionDecision::Selected { option_id })
            .map_err(|_| AppError::BadRequest(format!("Pending ACP permission expired: {call_id}")))?;

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
        self.closing.store(true, std::sync::atomic::Ordering::Release);

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
        let preferred_mode = self
            .preferred_mode()
            .await
            .map(|mode| normalize_requested_mode(&self.metadata, &mode))
            .filter(|mode| !mode.is_empty());
        Ok(aionui_api_types::AgentModeResponse {
            mode: self
                .modes()
                .await
                .map(|modes| modes.current_mode_id.to_string())
                .or(preferred_mode)
                .unwrap_or_else(|| normalize_requested_mode(&self.metadata, "default")),
            initialized: self.session_id().await.is_some(),
        })
    }

    async fn set_mode(&self, mode: &str) -> Result<(), AppError> {
        let normalized_mode = normalize_requested_mode(&self.metadata, mode);
        if normalized_mode.is_empty() {
            return Ok(());
        }
        let session_id = self.state.read().await.session_id.clone();

        if let Some(sid) = session_id {
            self.protocol
                .set_mode(SetSessionModeRequest::new(SessionId::new(sid), normalized_mode.clone()))
                .await
                .map_err(AppError::from)?;
            self.update_cached_mode(&normalized_mode).await;
        }

        let mut preferred_mode = self.preferred_mode.write().await;
        *preferred_mode = Some(normalized_mode);
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The stream channel powering [`AcpAgentManager::subscribe_stream`] is
    /// created identically to the one inside `new()` — capacity 256 and
    /// the `AgentStreamChunk` element type. Subscribing before any emit
    /// yields a live receiver that observes `TryRecvError::Empty`. Once
    /// D25c-2 wires up emitters, existing subscribers will begin seeing
    /// chunks; the empty-on-idle contract stays intact.
    #[test]
    fn stream_channel_yields_live_receiver_that_is_initially_empty() {
        let (tx, _) = broadcast::channel::<AgentStreamChunk>(256);
        let mut rx = tx.subscribe();
        assert!(matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Empty)));
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

    fn metadata_with_yolo_id(yolo_id: Option<&str>) -> AgentMetadata {
        use aionui_api_types::{AgentSource, AgentSourceInfo, BehaviorPolicy};
        AgentMetadata {
            id: "test".into(),
            icon: None,
            name: "Test".into(),
            name_i18n: None,
            description: None,
            description_i18n: None,
            backend: None,
            agent_type: AgentType::Acp,
            agent_source: AgentSource::Builtin,
            agent_source_info: AgentSourceInfo::default(),
            enabled: true,
            available: true,
            command: None,
            resolved_command: None,
            args: vec![],
            env: vec![],
            native_skills_dirs: None,
            behavior_policy: BehaviorPolicy::default(),
            yolo_id: yolo_id.map(ToOwned::to_owned),
            sort_order: 3130,
            handshake: AgentHandshake::default(),
        }
    }

    #[test]
    fn normalize_requested_mode_rewrites_yolo_when_behavior_policy_maps_it() {
        let meta = metadata_with_yolo_id(Some("full-access"));
        assert_eq!(normalize_requested_mode(&meta, "yolo"), "full-access");
        assert_eq!(normalize_requested_mode(&meta, "yoloNoSandbox"), "full-access");
    }

    #[test]
    fn normalize_requested_mode_passes_through_when_no_yolo_id() {
        let meta = metadata_with_yolo_id(None);
        // No mapping configured — aliases flow through unchanged.
        assert_eq!(normalize_requested_mode(&meta, "yolo"), "yolo");
        assert_eq!(normalize_requested_mode(&meta, "yoloNoSandbox"), "yoloNoSandbox");
    }

    #[test]
    fn normalize_requested_mode_passes_through_non_yolo_modes() {
        let meta = metadata_with_yolo_id(Some("full-access"));
        assert_eq!(normalize_requested_mode(&meta, "default"), "default");
        assert_eq!(normalize_requested_mode(&meta, "read-only"), "read-only");
        assert_eq!(
            normalize_requested_mode(&meta, "bypassPermissions"),
            "bypassPermissions"
        );
    }

    /// Vendor-specific yolo rewrites are entirely data-driven by
    /// `metadata.yolo_id`. Rebuild fixtures with the seed values
    /// `006_agent_metadata.sql` would hydrate, then assert both yolo
    /// aliases hit the native mode id for each vendor.
    #[test]
    fn normalize_requested_mode_rewrites_yolo_for_builtin_vendors() {
        // Claude / Codebuddy → bypassPermissions.
        let claude_like = metadata_with_yolo_id(Some("bypassPermissions"));
        assert_eq!(normalize_requested_mode(&claude_like, "yolo"), "bypassPermissions");
        assert_eq!(
            normalize_requested_mode(&claude_like, "yoloNoSandbox"),
            "bypassPermissions"
        );
        // Opencode → build.
        let opencode_like = metadata_with_yolo_id(Some("build"));
        assert_eq!(normalize_requested_mode(&opencode_like, "yolo"), "build");
        // Cursor → agent.
        let cursor_like = metadata_with_yolo_id(Some("agent"));
        assert_eq!(normalize_requested_mode(&cursor_like, "yolo"), "agent");
        // When a row has no yolo_id the alias flows through unchanged.
        let gemini_like = metadata_with_yolo_id(None);
        assert_eq!(normalize_requested_mode(&gemini_like, "yolo"), "yolo");
    }

    /// Codex's legacy `default` / `autoEdit` aliases should rewrite to
    /// its native `auto` mode when the row's backend label is "codex".
    /// Other backends must leave `default` / `autoEdit` untouched.
    #[test]
    fn normalize_requested_mode_rewrites_codex_default_and_auto_edit() {
        let mut codex_meta = metadata_with_yolo_id(Some("full-access"));
        codex_meta.backend = Some("codex".into());
        assert_eq!(normalize_requested_mode(&codex_meta, "default"), "auto");
        assert_eq!(normalize_requested_mode(&codex_meta, "autoEdit"), "auto");

        let other = metadata_with_yolo_id(None);
        assert_eq!(normalize_requested_mode(&other, "default"), "default");
        assert_eq!(normalize_requested_mode(&other, "autoEdit"), "autoEdit");
    }

    /// Claude backend must take the `session/new` + `_meta.claudeCode.options.resume`
    /// path so `mcpServers` are re-injected on resume. `backend == "claude"`
    /// alone is enough — we don't need the handshake to advertise `_meta`.
    #[test]
    fn uses_claude_meta_resume_true_for_claude_backend() {
        let mut meta = metadata_with_yolo_id(None);
        meta.backend = Some("claude".into());
        assert!(agent_metadata_uses_claude_meta_resume(&meta));
    }

    /// A non-Claude-labelled backend that still advertises
    /// `agent_capabilities._meta.claudeCode` (snake_case, as persisted by
    /// `sdk_to_snake_value`) must also follow the Claude resume path —
    /// this matches the frontend's `!!caps?._meta?.claudeCode` check.
    #[test]
    fn uses_claude_meta_resume_true_for_meta_claude_code() {
        let mut meta = metadata_with_yolo_id(None);
        meta.backend = Some("custom-claude-wrapper".into());
        meta.handshake.agent_capabilities = Some(json!({
            "_meta": {
                "claude_code": { "some": "flag" }
            }
        }));
        assert!(agent_metadata_uses_claude_meta_resume(&meta));

        // A handshake that bypassed snake_case normalisation (camelCase
        // `claudeCode`) must still be recognised.
        let mut camel_meta = metadata_with_yolo_id(None);
        camel_meta.backend = Some("custom-claude-wrapper".into());
        camel_meta.handshake.agent_capabilities = Some(json!({
            "_meta": {
                "claudeCode": { "some": "flag" }
            }
        }));
        assert!(agent_metadata_uses_claude_meta_resume(&camel_meta));
    }

    /// Codex (and any non-Claude backend without the `_meta.claudeCode`
    /// marker) must fall through to the `session/load` branch.
    #[test]
    fn uses_claude_meta_resume_false_for_codex() {
        let mut meta = metadata_with_yolo_id(Some("full-access"));
        meta.backend = Some("codex".into());
        assert!(!agent_metadata_uses_claude_meta_resume(&meta));

        // Codex with unrelated capability keys must still be false.
        meta.handshake.agent_capabilities = Some(json!({
            "load_session": true,
            "_meta": { "codex": { "whatever": true } }
        }));
        assert!(!agent_metadata_uses_claude_meta_resume(&meta));
    }

    /// Metadata with no `backend` label and no handshake capabilities
    /// must not opt into the Claude resume path.
    #[test]
    fn uses_claude_meta_resume_false_for_empty() {
        let meta = metadata_with_yolo_id(None);
        assert!(meta.backend.is_none());
        assert!(meta.handshake.agent_capabilities.is_none());
        assert!(!agent_metadata_uses_claude_meta_resume(&meta));
    }

    fn build_extra_without_team() -> AcpBuildExtra {
        serde_json::from_value(json!({ "backend": "claude" })).unwrap()
    }

    fn build_extra_with_team() -> AcpBuildExtra {
        serde_json::from_value(json!({
            "backend": "claude",
            "team_mcp_stdio_config": {
                "team_id": "team-42",
                "port": 54321,
                "token": "tok-abc",
                "slot_id": "slot-lead",
            },
        }))
        .unwrap()
    }

    #[test]
    fn build_new_session_request_skips_mcp_servers_without_team_config() {
        // Solo-chat path on a team-capable backend: no
        // `team_mcp_stdio_config` and Guide injection is still pending
        // D26a, so `mcp_servers` remains empty (matches the legacy
        // single-chat payload byte-for-byte).
        let req = build_new_session_request("/workspace", &build_extra_without_team());
        assert!(
            req.mcp_servers.is_empty(),
            "solo chat must not inject any MCP servers, got {:?}",
            req.mcp_servers
        );
    }

    #[test]
    fn build_new_session_request_solo_non_capable_backend_yields_empty_mcp_servers() {
        // Non-whitelisted backend: Guide must not be injected even once
        // D26a lands. This test pins the guard shape — it becomes
        // load-bearing when the D26a follow-up starts pushing
        // `McpServer::Http` onto the list.
        let extra: AcpBuildExtra = serde_json::from_value(json!({ "backend": "custom" })).unwrap();
        let req = build_new_session_request("/workspace", &extra);
        assert!(
            req.mcp_servers.is_empty(),
            "non-capable backend must not inject Guide MCP, got {:?}",
            req.mcp_servers
        );
    }

    #[test]
    fn build_new_session_request_solo_missing_backend_yields_empty_mcp_servers() {
        // Defensive: `extra.backend` is optional and historically may be
        // absent on legacy conversations. Guard must handle `None`.
        let extra: AcpBuildExtra = serde_json::from_value(json!({})).unwrap();
        let req = build_new_session_request("/workspace", &extra);
        assert!(
            req.mcp_servers.is_empty(),
            "missing backend must not inject Guide MCP, got {:?}",
            req.mcp_servers
        );
    }

    #[test]
    fn build_new_session_request_team_session_takes_priority_over_guide() {
        // Mutual exclusion: a team session injects the team MCP server
        // and must not also receive the Guide server once D26a lands.
        // Today this reduces to "team path still yields exactly one
        // server" — the assertion will tighten when the Guide branch
        // starts pushing servers.
        let req = build_new_session_request("/workspace", &build_extra_with_team());
        assert_eq!(
            req.mcp_servers.len(),
            1,
            "team session must inject exactly the team MCP server, not the Guide",
        );
    }

    /// Team MCP injection uses the HTTP transport on the `agent-client-protocol`
    /// schema (see `team_mcp_server`): `claude-agent-acp` actively connects to
    /// HTTP servers whereas its stdio path has spawn/init timing issues. This
    /// test pins the wire shape — name derived from `team_id`, URL pointing at
    /// the loopback port, and both `Authorization: Bearer <token>` and
    /// `X-Slot-Id` headers attached — so any accidental revert back to
    /// `McpServer::Stdio` fails loud.
    #[test]
    fn build_new_session_request_injects_team_http_server() {
        let req = build_new_session_request("/workspace", &build_extra_with_team());
        assert_eq!(req.mcp_servers.len(), 1, "exactly one team MCP server");

        let server = req.mcp_servers.into_iter().next().unwrap();
        let http = match server {
            McpServer::Http(h) => h,
            other => panic!("expected Http variant, got {other:?}"),
        };

        assert_eq!(http.name, "aionui-team-team-42");
        assert_eq!(http.url, "http://127.0.0.1:54321");

        let headers: std::collections::HashMap<_, _> = http
            .headers
            .iter()
            .map(|h| (h.name.as_str(), h.value.as_str()))
            .collect();
        assert_eq!(headers.get("Authorization"), Some(&"Bearer tok-abc"));
        assert_eq!(headers.get("X-Slot-Id"), Some(&"slot-lead"));
    }

    #[test]
    fn normalize_requested_mode_trims_and_returns_empty_for_blank() {
        let meta = metadata_with_yolo_id(Some("full-access"));
        assert_eq!(normalize_requested_mode(&meta, "   "), "");
    }

    const TEAM_GUIDE_MARKER: &str = "## Team Mode";

    #[test]
    fn compose_preset_context_injects_team_guide_for_solo_whitelisted_backend() {
        let out = compose_preset_context_with_team_guide(None, Some("claude"), false).expect("guide must be injected");
        assert!(
            out.contains(TEAM_GUIDE_MARKER),
            "solo whitelisted backend must receive Team Guide prompt, got: {out}"
        );
    }

    #[test]
    fn compose_preset_context_preserves_base_context_before_team_guide() {
        let base = "User rule: always respond in English.";
        let out =
            compose_preset_context_with_team_guide(Some(base), Some("gemini"), false).expect("guide must be injected");
        let guide_idx = out.find(TEAM_GUIDE_MARKER).expect("team guide present");
        let base_idx = out.find(base).expect("base rule present");
        assert!(
            base_idx < guide_idx,
            "base preset_context must come before the Team Guide so user rules stay at the top of [Assistant Rules]"
        );
    }

    #[test]
    fn compose_preset_context_skips_team_guide_when_already_in_team() {
        // Agent bound to a team: `team_mcp_stdio_config` present ⇒
        // `has_team_session = true`. Must NOT inject the Team Guide prompt.
        let out = compose_preset_context_with_team_guide(None, Some("claude"), true);
        assert!(out.is_none(), "in-team agent must not receive Team Guide, got: {out:?}");

        let with_base = compose_preset_context_with_team_guide(Some("Keep responses short."), Some("claude"), true)
            .expect("base context must pass through");
        assert!(
            !with_base.contains(TEAM_GUIDE_MARKER),
            "in-team agent must not receive Team Guide even when base context exists"
        );
        assert!(with_base.contains("Keep responses short."));
    }

    #[test]
    fn compose_preset_context_skips_team_guide_for_unknown_backend() {
        // Solo agent, but backend not on the whitelist: pass base context
        // through (possibly `None`) without adding the Team Guide.
        assert!(compose_preset_context_with_team_guide(None, Some("qwen"), false).is_none());
        assert!(compose_preset_context_with_team_guide(None, None, false).is_none());

        let with_base = compose_preset_context_with_team_guide(Some("Existing rule."), Some("qwen"), false)
            .expect("base context must pass through unchanged");
        assert_eq!(with_base, "Existing rule.");
    }

    #[test]
    fn compose_preset_context_treats_blank_base_as_absent() {
        // Whitespace-only base context should be treated as "none" so we
        // don't leak "\n\n" prefixes into the injected Team Guide.
        let out = compose_preset_context_with_team_guide(Some("   \n\t"), Some("claude"), false)
            .expect("guide must still be injected");
        assert!(out.starts_with("## Team Mode"));
    }

    /// Each session-driven event projects onto exactly one handshake
    /// field. Unrelated events produce `None` so the forwarder sends
    /// nothing for them.
    #[test]
    fn catalog_partial_covers_session_fields() {
        let modes = catalog_partial_from_event(&AgentStreamEvent::AcpModeInfo(json!({"x": 1})))
            .expect("mode event must project");
        assert_eq!(modes.available_modes, Some(json!({"x": 1})));
        assert!(modes.available_models.is_none());

        let models =
            catalog_partial_from_event(&AgentStreamEvent::AcpModelInfo(json!([1]))).expect("model event must project");
        assert_eq!(models.available_models, Some(json!([1])));

        let cfg = catalog_partial_from_event(&AgentStreamEvent::AcpConfigOption(json!([
            {"id":"mode"}
        ])))
        .expect("config event must project");
        assert_eq!(cfg.config_options, Some(json!([{"id":"mode"}])));

        // An unrelated event emits no update.
        assert!(catalog_partial_from_event(&AgentStreamEvent::Start(StartEventData { session_id: None })).is_none());
    }
}
