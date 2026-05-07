//! Process-wide snapshot of the `agent_metadata` catalog.
//!
//! The table is the single source of truth for every agent the user can
//! spawn — builtin vendor rows, extension-installed rows, and custom
//! rows all live there. The registry:
//!
//! - hydrates `select *` into memory at startup;
//! - probes each row's spawn command via `which()` so the `available`
//!   field reflects PATH state right now (not a persisted column);
//! - exposes lookups the factory and routes use (`get`,
//!   `find_by_backend`, `list_by_agent_type`, etc.);
//! - writes ACP handshake payloads back to the row through
//!   [`AgentRegistry::catalog_sender`] (serialised through a single
//!   consumer task, see [`CatalogSender`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use aionui_api_types::{AgentEnvEntry, AgentHandshake, AgentMetadata, AgentSource, AgentSourceInfo, BehaviorPolicy};
use aionui_common::{AgentType, AppError};
use aionui_db::{AgentMetadataRow, IAgentMetadataRepository, UpdateAgentHandshakeParams};
use serde_json::Value;
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, warn};

/// Capacity of the catalog-sync MPSC channel. A single writer thread
/// drains it serially, so the bound just sizes the burst we can absorb
/// before producers start to back off.
const CATALOG_SYNC_CHANNEL_CAPACITY: usize = 256;

/// One unit of work submitted to the catalog sync consumer task.
#[derive(Debug)]
struct CatalogSyncMessage {
    agent_metadata_id: String,
    handshake: AgentHandshake,
}

pub struct AgentRegistry {
    repo: Arc<dyn IAgentMetadataRepository>,
    by_id: RwLock<HashMap<String, AgentMetadata>>,
    /// MPSC sender shared with every forwarder in every `AcpAgentManager`.
    /// Draining happens in a single background task owned by this
    /// registry, so DB writes for the same (id, field) serialize.
    catalog_tx: mpsc::Sender<CatalogSyncMessage>,
}

impl AgentRegistry {
    pub fn new(repo: Arc<dyn IAgentMetadataRepository>) -> Arc<Self> {
        let (tx, rx) = mpsc::channel::<CatalogSyncMessage>(CATALOG_SYNC_CHANNEL_CAPACITY);
        let this = Arc::new(Self {
            repo,
            by_id: RwLock::new(HashMap::new()),
            catalog_tx: tx,
        });

        this.clone().spawn_catalog_consumer(rx);
        this
    }

    /// Drive the single consumer task. Runs until every sender (including
    /// the one held by the registry itself) has been dropped — which only
    /// happens at process shutdown because the registry lives as long as
    /// `AppServices`.
    fn spawn_catalog_consumer(self: Arc<Self>, mut rx: mpsc::Receiver<CatalogSyncMessage>) {
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(err) = self.apply_handshake_inner(&msg.agent_metadata_id, &msg.handshake).await {
                    warn!(
                        agent_metadata_id = %msg.agent_metadata_id,
                        error = %err,
                        "Catalog sync: apply_handshake failed"
                    );
                }
            }
            debug!("Catalog sync consumer task exiting — all senders dropped");
        });
    }

    /// Persist handshake snapshot fields onto the row and refresh the
    /// cached copy. Internal — production code writes through
    /// [`AgentRegistry::catalog_sender`] so every write is serialized
    /// through the single consumer task. Direct calls exist only for
    /// tests and the consumer itself.
    ///
    /// `None` fields are left untouched (partial update).
    async fn apply_handshake_inner(&self, id: &str, snapshot: &AgentHandshake) -> Result<(), AppError> {
        let agent_capabilities = encode_optional(&snapshot.agent_capabilities, "agent_capabilities")?;
        let auth_methods = encode_optional(&snapshot.auth_methods, "auth_methods")?;
        let config_options = encode_optional(&snapshot.config_options, "config_options")?;
        let available_modes = encode_optional(&snapshot.available_modes, "available_modes")?;
        let available_models = encode_optional(&snapshot.available_models, "available_models")?;
        let available_commands = encode_optional(&snapshot.available_commands, "available_commands")?;

        let params = UpdateAgentHandshakeParams {
            agent_capabilities: agent_capabilities.as_deref().map(Some),
            auth_methods: auth_methods.as_deref().map(Some),
            config_options: config_options.as_deref().map(Some),
            available_modes: available_modes.as_deref().map(Some),
            available_models: available_models.as_deref().map(Some),
            available_commands: available_commands.as_deref().map(Some),
        };

        let Some(row) = self
            .repo
            .apply_handshake(id, &params)
            .await
            .map_err(|e| AppError::Internal(format!("apply_handshake: {e}")))?
        else {
            return Ok(());
        };

        if let Some(meta) = decode_row(row) {
            self.by_id.write().await.insert(meta.id.clone(), meta);
        }
        Ok(())
    }
}

impl AgentRegistry {
    /// Sender end of the catalog-sync MPSC, cloned by each
    /// `AcpAgentManager` forwarder.
    pub fn catalog_sender(&self) -> CatalogSender {
        CatalogSender {
            tx: self.catalog_tx.clone(),
        }
    }
    /// Reload every enabled row from the database and re-probe their
    /// spawn commands on `$PATH`.
    pub async fn hydrate(&self) -> Result<(), AppError> {
        let rows = self
            .repo
            .list_all()
            .await
            .map_err(|e| AppError::Internal(format!("load agent_metadata: {e}")))?;

        let mut map = HashMap::with_capacity(rows.len());
        for row in rows {
            let Some(meta) = decode_row(row) else {
                continue;
            };
            map.insert(meta.id.clone(), meta);
        }
        *self.by_id.write().await = map;
        debug!(rows = self.by_id.read().await.len(), "AgentRegistry hydrated");
        Ok(())
    }

    /// Re-probe every row's command without refetching from the DB.
    /// Useful after PATH has changed (e.g. `launchctl setenv`).
    pub async fn refresh_availability(&self) {
        let mut guard = self.by_id.write().await;
        for meta in guard.values_mut() {
            meta.resolved_command = probe_resolved_command(meta);
            meta.available = meta.resolved_command.is_some()
                || (meta.enabled && meta.command.is_none() && meta.agent_source == AgentSource::Internal);
        }
    }

    pub async fn get(&self, id: &str) -> Option<AgentMetadata> {
        self.by_id.read().await.get(id).cloned()
    }

    /// First row whose vendor label matches, among `agent_source = 'builtin'`.
    pub async fn find_builtin_by_backend(&self, vendor: &str) -> Option<AgentMetadata> {
        self.by_id
            .read()
            .await
            .values()
            .find(|m| m.backend.as_deref() == Some(vendor) && m.agent_source == AgentSource::Builtin)
            .cloned()
    }

    /// Every enabled, installed row whose `agent_type` matches,
    /// sorted by `sort_order`. See [`Self::list_all`] for the filter
    /// semantics.
    pub async fn list_by_agent_type(&self, agent_type: AgentType) -> Vec<AgentMetadata> {
        let guard = self.by_id.read().await;
        let mut rows: Vec<AgentMetadata> = guard
            .values()
            .filter(|m| m.agent_type == agent_type && is_visible(m))
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
        rows
    }

    /// Snapshot of every row the caller is expected to see — rows
    /// that are user-disabled (`enabled = 0`) or whose spawn command
    /// could not be located on `$PATH` (`available = false`) are
    /// filtered out. `/api/agents` feeds the frontend pill bar, which
    /// would otherwise render unusable vendor chips that fail the
    /// moment the user tries to spawn them.
    pub async fn list_all(&self) -> Vec<AgentMetadata> {
        let mut rows: Vec<AgentMetadata> = self
            .by_id
            .read()
            .await
            .values()
            .filter(|m| is_visible(m))
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
        rows
    }

    /// Unfiltered snapshot — used by internal paths that legitimately
    /// need to see user-disabled or missing rows (e.g. the UI's
    /// "manage agents" surface). Keep external API handlers on
    /// [`Self::list_all`].
    pub async fn list_all_including_hidden(&self) -> Vec<AgentMetadata> {
        let mut rows: Vec<AgentMetadata> = self.by_id.read().await.values().cloned().collect();
        rows.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
        rows
    }
}

/// A catalog row is visible to callers when the user has it enabled
/// and the spawn command was resolved at hydrate/refresh time. The
/// second check is what keeps uninstalled CLIs (e.g. `cursor` when
/// only `claude` is on PATH) off the pill bar.
fn is_visible(meta: &AgentMetadata) -> bool {
    meta.enabled && meta.available
}

/// Turn a DB row into the public `AgentMetadata`, probing the command
/// on disk so `available` reflects the current PATH state.
fn decode_row(row: AgentMetadataRow) -> Option<AgentMetadata> {
    let agent_type = parse_agent_type(&row.agent_type)?;
    let agent_source = parse_agent_source(&row.agent_source)?;
    let agent_source_info = decode_json_field(row.agent_source_info.as_deref(), "agent_source_info")
        .unwrap_or_else(AgentSourceInfo::default);
    let args = decode_json_field::<Vec<String>>(row.args.as_deref(), "args").unwrap_or_default();
    let env = decode_json_field::<Vec<AgentEnvEntry>>(row.env.as_deref(), "env").unwrap_or_default();
    let native_skills_dirs = decode_json_field::<Vec<String>>(row.native_skills_dirs.as_deref(), "native_skills_dirs");
    let behavior_policy =
        decode_json_field(row.behavior_policy.as_deref(), "behavior_policy").unwrap_or_else(BehaviorPolicy::default);

    let handshake = AgentHandshake {
        agent_capabilities: parse_json(row.agent_capabilities.as_deref(), "agent_capabilities"),
        auth_methods: parse_json(row.auth_methods.as_deref(), "auth_methods"),
        config_options: parse_json(row.config_options.as_deref(), "config_options"),
        available_modes: parse_json(row.available_modes.as_deref(), "available_modes"),
        available_models: parse_json(row.available_models.as_deref(), "available_models"),
        available_commands: parse_json(row.available_commands.as_deref(), "available_commands"),
    };

    let mut meta = AgentMetadata {
        id: row.id,
        icon: row.icon,
        name: row.name,
        name_i18n: parse_json(row.name_i18n.as_deref(), "name_i18n"),
        description: row.description,
        description_i18n: parse_json(row.description_i18n.as_deref(), "description_i18n"),
        backend: row.backend,
        agent_type,
        agent_source,
        agent_source_info,
        enabled: row.enabled,
        available: false,
        command: row.command,
        resolved_command: None,
        args,
        env,
        native_skills_dirs,
        behavior_policy,
        yolo_id: row.yolo_id,
        sort_order: row.sort_order,
        handshake,
    };

    meta.resolved_command = probe_resolved_command(&meta);
    meta.available = meta.resolved_command.is_some()
        || (meta.enabled && meta.command.is_none() && meta.agent_source == AgentSource::Internal);
    Some(meta)
}

fn parse_agent_type(raw: &str) -> Option<AgentType> {
    serde_json::from_value(Value::String(raw.to_owned())).ok()
}

fn parse_agent_source(raw: &str) -> Option<AgentSource> {
    serde_json::from_value(Value::String(raw.to_owned())).ok()
}

fn decode_json_field<T: serde::de::DeserializeOwned>(raw: Option<&str>, field: &str) -> Option<T> {
    raw.and_then(|s| match serde_json::from_str(s) {
        Ok(v) => Some(v),
        Err(err) => {
            warn!(field, error = %err, "agent_metadata: failed to decode JSON column");
            None
        }
    })
}

fn parse_json(raw: Option<&str>, field: &str) -> Option<Value> {
    raw.and_then(|s| match serde_json::from_str::<Value>(s) {
        Ok(v) => Some(v),
        Err(err) => {
            warn!(field, error = %err, "agent_metadata: failed to parse JSON");
            None
        }
    })
}

fn encode_optional(value: &Option<Value>, field: &str) -> Result<Option<String>, AppError> {
    match value {
        Some(v) => serde_json::to_string(v)
            .map(Some)
            .map_err(|e| AppError::Internal(format!("encode {field}: {e}"))),
        None => Ok(None),
    }
}

/// Cloneable handle each `AcpAgentManager` holds to forward ACP events
/// into the registry's background consumer task. Dropping it is cheap
/// and does not affect the consumer — the registry itself keeps one
/// sender alive for the life of the process.
#[derive(Clone)]
pub struct CatalogSender {
    tx: mpsc::Sender<CatalogSyncMessage>,
}

impl CatalogSender {
    /// Submit a partial handshake update. Returns without error when the
    /// channel is closed (only happens at shutdown) or full — callers do
    /// not need to care because the consumer is best-effort.
    pub fn send_partial(&self, agent_metadata_id: String, handshake: AgentHandshake) {
        let msg = CatalogSyncMessage {
            agent_metadata_id,
            handshake,
        };
        if let Err(err) = self.tx.try_send(msg) {
            use mpsc::error::TrySendError;
            match err {
                TrySendError::Full(_) => {
                    warn!("Catalog sync channel full; dropping handshake update");
                }
                TrySendError::Closed(_) => {
                    debug!("Catalog sync channel closed; consumer already shut down");
                }
            }
        }
    }
}

/// Resolve a command name to an absolute path.
///
/// For `bun` / `bunx` we go through `aionui_runtime` so the bundled
/// runtime is used when present; everything else falls back to the
/// user's `$PATH` via `which::which`.
fn resolve_command_path(cmd: &str) -> Option<PathBuf> {
    match cmd {
        "bun" => aionui_runtime::resolve_bun().ok(),
        "bunx" => {
            let bunx_name = if cfg!(windows) { "bunx.exe" } else { "bunx" };
            if let Some(dir) = aionui_runtime::bun_bin_dir() {
                let p = dir.join(bunx_name);
                if p.exists() {
                    return Some(p);
                }
            }
            which::which("bunx").ok()
        }
        other => which::which(other).ok(),
    }
}

/// Resolve the spawn command to an absolute path via `$PATH`. Returns
/// `None` when the row is disabled, the command is missing, or any
/// required binary (spawn command, bridge binary, primary CLI) is not
/// on `$PATH`. The value is the single source of truth for
/// `available` — callers never re-run `which()` themselves.
///
/// Bridge-based rows (e.g. `bun x @pkg`) require both `bun` (the spawn
/// command) and the wrapped CLI (`claude`, recorded in
/// `agent_source_info.binary_name`) to be present. Direct-CLI rows
/// have `spawn command == primary binary`, so the primary-binary check
/// is a no-op for them.
fn probe_resolved_command(meta: &AgentMetadata) -> Option<PathBuf> {
    if !meta.enabled {
        return None;
    }
    let cmd = meta.command.as_deref().filter(|s| !s.is_empty())?;

    if let Some(bridge) = meta.agent_source_info.bridge_binary.as_deref()
        && bridge != cmd
        && resolve_command_path(bridge).is_none()
    {
        return None;
    }
    if let Some(primary) = meta.agent_source_info.binary_name.as_deref()
        && primary != cmd
        && meta.agent_source_info.bridge_binary.as_deref() != Some(primary)
        && resolve_command_path(primary).is_none()
    {
        return None;
    }

    resolve_command_path(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_db::{SqliteAgentMetadataRepository, init_database_memory};

    async fn registry() -> Arc<AgentRegistry> {
        let db = init_database_memory().await.unwrap();
        let repo = Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone()));
        let reg = AgentRegistry::new(repo);
        reg.hydrate().await.unwrap();
        reg
    }

    #[tokio::test]
    async fn hydrate_loads_seed_rows() {
        // `list_all_including_hidden` bypasses the available/enabled
        // filter so this assertion keeps counting the seed rows even
        // when none of the CLIs are installed on the test host.
        let reg = registry().await;
        let all = reg.list_all_including_hidden().await;
        assert_eq!(all.len(), 20);
    }

    #[tokio::test]
    async fn find_builtin_claude_has_bridge_command() {
        let reg = registry().await;
        let m = reg.find_builtin_by_backend("claude").await.unwrap();
        assert_eq!(m.command.as_deref(), Some("bun"));
        assert!(m.behavior_policy.supports_side_question);
        assert_eq!(
            m.native_skills_dirs.as_deref(),
            Some(&[".claude/skills".to_string()][..])
        );
    }

    #[tokio::test]
    async fn codex_yolo_id_maps_to_full_access() {
        let reg = registry().await;
        let codex = reg.find_builtin_by_backend("codex").await.unwrap();
        // Legacy AionUi yolo aliases resolve to Codex's native
        // `full-access` mode via the catalog row.
        assert_eq!(codex.yolo_id.as_deref(), Some("full-access"));
    }

    #[tokio::test]
    async fn claude_yolo_id_maps_to_bypass_permissions() {
        let reg = registry().await;
        let claude = reg.find_builtin_by_backend("claude").await.unwrap();
        assert_eq!(claude.yolo_id.as_deref(), Some("bypassPermissions"));
    }

    /// On a host that has *none* of the seeded CLIs installed, the
    /// public listing collapses to the rows that don't need one
    /// (Aion CLI is `agent_source = internal` with no `command`).
    /// This guards the pill-bar contract: never show an unusable
    /// vendor.
    #[tokio::test]
    async fn list_all_filters_out_unavailable_rows() {
        let reg = registry().await;
        let visible = reg.list_all().await;
        assert!(
            visible.iter().all(|m| m.enabled && m.available),
            "list_all must only return enabled + available rows, got: {:?}",
            visible
                .iter()
                .map(|m| (&m.id, m.enabled, m.available))
                .collect::<Vec<_>>()
        );
        // Aion CLI (internal, no spawn command) is always available.
        assert!(
            visible.iter().any(|m| m.agent_type == AgentType::Aionrs),
            "internal aionrs row should survive the filter"
        );
    }

    #[tokio::test]
    async fn list_by_agent_type_counts_seed_rows() {
        // Seed counts — exercised against the unfiltered view because
        // on CI hosts the CLIs aren't installed, so `list_by_agent_type`
        // (which applies the visibility filter) would report zero.
        let reg = registry().await;
        let all = reg.list_all_including_hidden().await;
        let count = |t: AgentType| all.iter().filter(|m| m.agent_type == t).count();
        assert_eq!(count(AgentType::Acp), 17);
        assert_eq!(count(AgentType::Nanobot), 1);
        assert_eq!(count(AgentType::OpenclawGateway), 1);
        assert_eq!(count(AgentType::Aionrs), 1);
    }

    #[tokio::test]
    async fn aionrs_internal_row_is_available_without_command() {
        let reg = registry().await;
        let aionrs = reg
            .list_by_agent_type(AgentType::Aionrs)
            .await
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(aionrs.agent_source, AgentSource::Internal);
        assert!(aionrs.command.is_none());
        assert!(aionrs.available);
    }

    #[tokio::test]
    async fn apply_handshake_persists_json_payload() {
        let reg = registry().await;
        let claude = reg.find_builtin_by_backend("claude").await.unwrap();

        let snapshot = AgentHandshake {
            auth_methods: Some(serde_json::json!([
                {"type":"agent","id":"oauth","name":"OAuth"}
            ])),
            ..Default::default()
        };
        reg.apply_handshake_inner(&claude.id, &snapshot).await.unwrap();

        let refreshed = reg.get(&claude.id).await.unwrap();
        let methods = refreshed.handshake.auth_methods.unwrap();
        assert_eq!(methods.as_array().unwrap().len(), 1);
    }

    /// Partial updates must leave unrelated columns untouched.
    ///
    /// Three consecutive writes target three different columns — each
    /// later write only carries one `Some(..)` field, the rest are
    /// `None`. After all three land, every earlier value must still be
    /// readable. This locks the contract that `None` means "don't
    /// touch" (as opposed to "clear to null"), which is what the
    /// `initialize` / `session/new` / `AvailableCommandsUpdate` write
    /// sites rely on.
    #[tokio::test]
    async fn apply_handshake_is_partial_does_not_clobber_siblings() {
        let reg = registry().await;
        let claude = reg.find_builtin_by_backend("claude").await.unwrap();

        // Write #1: agent_capabilities only.
        reg.apply_handshake_inner(
            &claude.id,
            &AgentHandshake {
                agent_capabilities: Some(serde_json::json!({"load_session": true})),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Write #2: auth_methods only. Capabilities must survive.
        reg.apply_handshake_inner(
            &claude.id,
            &AgentHandshake {
                auth_methods: Some(serde_json::json!([{"type": "agent", "id": "oauth"}])),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Write #3: available_modes only. Capabilities + auth_methods must survive.
        reg.apply_handshake_inner(
            &claude.id,
            &AgentHandshake {
                available_modes: Some(serde_json::json!([{"id": "code", "name": "Code"}])),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let refreshed = reg.get(&claude.id).await.unwrap();
        assert_eq!(
            refreshed.handshake.agent_capabilities,
            Some(serde_json::json!({"load_session": true})),
            "agent_capabilities must survive later partial writes"
        );
        assert!(
            refreshed.handshake.auth_methods.is_some(),
            "auth_methods must survive the later available_modes write"
        );
        assert!(refreshed.handshake.available_modes.is_some());
        // The untouched fields stay untouched (still None from seed).
        assert!(refreshed.handshake.available_models.is_none());
        assert!(refreshed.handshake.config_options.is_none());
        assert!(refreshed.handshake.available_commands.is_none());
    }

    /// An empty snapshot is a no-op — no column gets overwritten.
    #[tokio::test]
    async fn apply_handshake_with_empty_snapshot_is_noop() {
        let reg = registry().await;
        let claude = reg.find_builtin_by_backend("claude").await.unwrap();

        reg.apply_handshake_inner(
            &claude.id,
            &AgentHandshake {
                agent_capabilities: Some(serde_json::json!({"x": 1})),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        reg.apply_handshake_inner(&claude.id, &AgentHandshake::default())
            .await
            .unwrap();

        let refreshed = reg.get(&claude.id).await.unwrap();
        assert_eq!(
            refreshed.handshake.agent_capabilities,
            Some(serde_json::json!({"x": 1}))
        );
    }
}
