//! Workspace-level file watcher: shared OS watcher via notify-debouncer-full,
//! gitignore filtering, event fan-out to workspace subscribers + office watch.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify_debouncer_full::{DebounceEventResult, DebouncedEvent, Debouncer, RecommendedCache, new_debouncer};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use aionui_api_types::{WatchBatchEvent, WatchChange, WatchChangeKind, WatchOverflowEvent, WebSocketMessage};
use aionui_common::AppError;
use aionui_realtime::{ConnectionId, WebSocketManager};

use crate::workspace_watcher_registry::SubscriptionRegistry;

// ---------------------------------------------------------------------------
// Debounced event from notify-debouncer-full
// ---------------------------------------------------------------------------

/// Processed batch of debounced events for a workspace.
#[derive(Debug)]
pub struct DebouncedBatch {
    pub workspace: String,
    pub events: Vec<DebouncedEvent>,
}

// ---------------------------------------------------------------------------
// GitignoreFilter
// ---------------------------------------------------------------------------

/// Caches per-workspace gitignore matchers.
pub struct GitignoreFilter {
    cache: DashMap<String, Gitignore>,
}

impl Default for GitignoreFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl GitignoreFilter {
    pub fn new() -> Self {
        Self { cache: DashMap::new() }
    }

    /// Returns true if the path should be ignored (filtered out).
    pub fn is_ignored(&self, workspace: &str, relative_path: &str, is_dir: bool) -> bool {
        if self.cache.get(workspace).is_none() {
            self.rebuild(workspace);
        }
        if let Some(matcher) = self.cache.get(workspace) {
            // Check the path itself
            if matcher.matched(relative_path, is_dir).is_ignore() {
                return true;
            }
            // Check ancestors (e.g. ".git/config" → check ".git" as dir)
            let p = Path::new(relative_path);
            for ancestor in p.ancestors().skip(1) {
                if ancestor == Path::new("") {
                    break;
                }
                if matcher.matched(ancestor.to_string_lossy().as_ref(), true).is_ignore() {
                    return true;
                }
            }
            false
        } else {
            false
        }
    }

    /// Rebuild the gitignore matcher for a workspace.
    pub fn rebuild(&self, workspace: &str) {
        let gitignore_path = Path::new(workspace).join(".gitignore");
        let mut builder = GitignoreBuilder::new(workspace);
        if gitignore_path.exists() {
            let _ = builder.add(&gitignore_path);
        }
        // Always ignore .git directory and its contents
        let _ = builder.add_line(None, ".git");
        match builder.build() {
            Ok(matcher) => {
                self.cache.insert(workspace.to_owned(), matcher);
            }
            Err(e) => {
                warn!(workspace, error = %e, "failed to build gitignore matcher");
            }
        }
    }

    /// Invalidate cache for a workspace (e.g. when .gitignore changes).
    pub fn invalidate(&self, workspace: &str) {
        self.cache.remove(workspace);
    }
}

// ---------------------------------------------------------------------------
// SharedWorkspaceWatcher (using notify-debouncer-full)
// ---------------------------------------------------------------------------

/// A shared OS-level watcher for a single workspace directory.
///
/// Uses `notify-debouncer-full` to handle:
/// - Atomic save detection (write-to-tmp + rename → single Modify)
/// - Event deduplication and coalescing
/// - Cross-platform rename pairing via file-id tracking
pub struct SharedWorkspaceWatcher {
    _debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
    pub workspace: String,
}

impl SharedWorkspaceWatcher {
    /// Create a new recursive debounced watcher for the given workspace.
    /// Debounced events are forwarded to the provided sender.
    pub fn new(workspace: &str, event_tx: mpsc::UnboundedSender<DebouncedBatch>) -> Result<Self, AppError> {
        let ws = workspace.to_owned();
        let canonical = std::fs::canonicalize(workspace)
            .map_err(|e| AppError::NotFound(format!("cannot resolve workspace {workspace}: {e}")))?;

        let ws_clone = ws.clone();
        let mut debouncer = new_debouncer(
            std::time::Duration::from_millis(500),
            None,
            move |result: DebounceEventResult| {
                let events = match result {
                    Ok(events) => events,
                    Err(errors) => {
                        for e in errors {
                            warn!(error = %e, "debouncer error");
                        }
                        return;
                    }
                };
                if events.is_empty() {
                    return;
                }
                let _ = event_tx.send(DebouncedBatch {
                    workspace: ws_clone.clone(),
                    events,
                });
            },
        )
        .map_err(|e| AppError::Internal(format!("failed to create workspace debouncer: {e}")))?;

        debouncer
            .watch(&canonical, notify::RecursiveMode::Recursive)
            .map_err(|e| AppError::Internal(format!("failed to watch workspace {workspace}: {e}")))?;

        Ok(Self {
            _debouncer: debouncer,
            workspace: ws,
        })
    }
}

// ---------------------------------------------------------------------------
// Office file extension check (for fan-out)
// ---------------------------------------------------------------------------

const OFFICE_EXTENSIONS: &[&str] = &["pptx", "docx", "xlsx"];

fn is_office_file(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| {
        let lower = ext.to_ascii_lowercase();
        OFFICE_EXTENSIONS.contains(&lower.as_str())
    })
}

// ---------------------------------------------------------------------------
// EventDispatcher (replaces EventAggregator)
// ---------------------------------------------------------------------------

/// Overflow threshold: max changes per directory per batch.
const OVERFLOW_THRESHOLD: usize = 500;

/// Receives debounced events and dispatches to workspace subscribers + office fan-out.
pub struct EventDispatcher {
    registry: Arc<SubscriptionRegistry>,
    ws_manager: Arc<WebSocketManager>,
    gitignore: Arc<GitignoreFilter>,
    office_broadcaster: Option<Arc<dyn aionui_realtime::EventBroadcaster>>,
}

impl EventDispatcher {
    pub fn new(
        registry: Arc<SubscriptionRegistry>,
        ws_manager: Arc<WebSocketManager>,
        gitignore: Arc<GitignoreFilter>,
    ) -> Self {
        Self {
            registry,
            ws_manager,
            gitignore,
            office_broadcaster: None,
        }
    }

    pub fn with_office_broadcaster(mut self, broadcaster: Arc<dyn aionui_realtime::EventBroadcaster>) -> Self {
        self.office_broadcaster = Some(broadcaster);
        self
    }

    /// Run the dispatch loop, consuming debounced batches from the channel.
    pub async fn run(self, mut event_rx: mpsc::UnboundedReceiver<DebouncedBatch>) {
        while let Some(batch) = event_rx.recv().await {
            self.dispatch_batch(batch);
        }
    }

    fn dispatch_batch(&self, batch: DebouncedBatch) {
        let workspace = batch.workspace.as_str();
        let workspace_path = PathBuf::from(workspace);
        let mut changes: Vec<WatchChange> = Vec::new();

        for event in &batch.events {
            let kind = match map_debounced_kind(&event.kind) {
                Some(k) => k,
                None => continue,
            };

            for path in &event.paths {
                let relative = match path.strip_prefix(&workspace_path) {
                    Ok(r) => r.to_string_lossy().into_owned(),
                    Err(_) => continue,
                };

                if relative.is_empty() {
                    continue;
                }

                if is_temp_file(&relative) {
                    continue;
                }

                if relative == ".gitignore" {
                    self.gitignore.invalidate(workspace);
                    self.gitignore.rebuild(workspace);
                }

                let is_dir = path.is_dir();
                if self.gitignore.is_ignored(workspace, &relative, is_dir) {
                    continue;
                }

                // Office fan-out (legacy, kept until frontend migrates to
                // extensions). MUST happen with the original path (before
                // Phase 1 path rewriting) — office_broadcaster relies on the
                // original path/extension to identify office files.
                if kind == WatchChangeKind::Create && is_office_file(path) {
                    self.emit_office_event(path, workspace);
                }

                changes.push(WatchChange { path: relative, kind });
            }
        }

        if changes.is_empty() {
            return;
        }

        self.dispatch_changes(workspace, changes);
    }

    /// Dispatch already-extracted relative changes for a workspace.
    ///
    /// Split out from `dispatch_batch` so it can be exercised directly from
    /// tests without constructing real `notify` events.
    fn dispatch_changes(&self, workspace: &str, changes: Vec<WatchChange>) {
        if changes.is_empty() {
            return;
        }

        // Track which (conn, path) pairs have been sent via dirs to avoid duplicates
        let mut sent: HashSet<(ConnectionId, usize)> = HashSet::new();

        // --- Phase 1: dispatch by directory subscription (first-level rule) ---
        //
        // For each change, walk the ancestor chain to find — PER CONNECTION —
        // the nearest subscribed directory of that connection. Each connection
        // sees its OWN nearest ancestor (so a root subscriber and a `src`
        // subscriber both receive an appropriate event for the same deep
        // change, each rewritten to their own direct child).
        //
        // The dispatched path is rewritten to the subscriber's direct child
        // along the ancestor chain. When the subscriber is exactly the
        // change's parent dir (direct child case), the original path/kind
        // are kept; otherwise the kind is forced to `Modify`.
        //
        // Bucketed per-connection, dedup'd by (rewritten_path, kind) so
        // multiple deep changes resolving to the same rewritten path collapse
        // into a single outbound WatchChange for that connection.
        struct ConnBucket {
            changes: Vec<WatchChange>,
            seen: HashSet<(String, WatchChangeKind)>,
            indices: Vec<usize>,
        }
        let mut per_conn: HashMap<ConnectionId, ConnBucket> = HashMap::new();

        for (idx, change) in changes.iter().enumerate() {
            let chain = ancestor_chain(&change.path);
            // For each connection subscribed to ANY directory in the chain,
            // pick the SHALLOWEST chain index that connection has subscribed
            // to (= the deepest dir, i.e. the "nearest" ancestor in path
            // terms). We iterate from i=0 (deepest) upward, and only keep
            // the first hit per connection.
            let mut conn_resolved: HashMap<ConnectionId, (String, String)> = HashMap::new();
            for (i, dir) in chain.iter().enumerate() {
                let subs = self.registry.get_subscribers_for_dir(workspace, dir);
                if subs.is_empty() {
                    continue;
                }
                let rewritten = if i == 0 {
                    change.path.clone()
                } else {
                    chain[i - 1].clone()
                };
                for conn_id in subs {
                    conn_resolved
                        .entry(conn_id)
                        .or_insert_with(|| (dir.clone(), rewritten.clone()));
                }
            }

            for (conn_id, (subscribed_dir, rewritten_path)) in conn_resolved {
                let was_rewritten = rewritten_path != change.path;
                let dispatch_kind = if was_rewritten {
                    WatchChangeKind::Modify
                } else {
                    change.kind
                };

                let bucket = per_conn.entry(conn_id).or_insert_with(|| ConnBucket {
                    changes: Vec::new(),
                    seen: HashSet::new(),
                    indices: Vec::new(),
                });
                bucket.indices.push(idx);
                let key = (rewritten_path.clone(), dispatch_kind);
                if bucket.seen.insert(key) {
                    bucket.changes.push(WatchChange {
                        path: rewritten_path,
                        kind: dispatch_kind,
                    });
                }
                // Mark as sent for Phase 2 dedup. Note: this uses the ORIGINAL
                // change idx (not the rewritten path), because Phase 2 keys on
                // the original change.
                sent.insert((conn_id, idx));
                let _ = subscribed_dir; // currently unused beyond rewriting; reserved for diagnostics
            }
        }

        for (conn_id, bucket) in per_conn {
            if bucket.changes.len() > OVERFLOW_THRESHOLD {
                let event = WatchOverflowEvent {
                    workspace: workspace.to_owned(),
                };
                let msg = WebSocketMessage::new("workspace.overflow", serde_json::to_value(&event).unwrap_or_default());
                self.ws_manager.send_to(conn_id, msg);
            } else {
                let event = WatchBatchEvent {
                    workspace: workspace.to_owned(),
                    changes: bucket.changes,
                };
                let msg = WebSocketMessage::new("workspace.changed", serde_json::to_value(&event).unwrap_or_default());
                self.ws_manager.send_to(conn_id, msg);
            }
        }

        // --- Phase 2: dispatch by extension subscription (full-recursive) ---
        let mut ext_per_conn: HashMap<ConnectionId, Vec<&WatchChange>> = HashMap::new();

        for (idx, change) in changes.iter().enumerate() {
            if let Some(ext) = file_extension(&change.path) {
                let subscribers = self.registry.get_subscribers_for_extension(workspace, &ext);
                for conn_id in subscribers {
                    if !sent.contains(&(conn_id, idx)) {
                        ext_per_conn.entry(conn_id).or_default().push(change);
                    }
                }
            }
        }

        for (conn_id, ext_changes) in ext_per_conn {
            if ext_changes.len() > OVERFLOW_THRESHOLD {
                let event = WatchOverflowEvent {
                    workspace: workspace.to_owned(),
                };
                let msg = WebSocketMessage::new("workspace.overflow", serde_json::to_value(&event).unwrap_or_default());
                self.ws_manager.send_to(conn_id, msg);
            } else {
                let event = WatchBatchEvent {
                    workspace: workspace.to_owned(),
                    changes: ext_changes.into_iter().cloned().collect(),
                };
                let msg = WebSocketMessage::new("workspace.changed", serde_json::to_value(&event).unwrap_or_default());
                self.ws_manager.send_to(conn_id, msg);
            }
        }
    }

    fn emit_office_event(&self, path: &Path, workspace: &str) {
        if let Some(ref broadcaster) = self.office_broadcaster {
            let payload = crate::types::OfficeFileAddedEvent {
                file_path: path.to_string_lossy().into_owned(),
                workspace: workspace.to_owned(),
            };
            let json = serde_json::to_value(&payload).unwrap_or_default();
            broadcaster.broadcast(WebSocketMessage::new("workspaceOfficeWatch.fileAdded", json));
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map debounced EventKind to our WatchChangeKind.
fn map_debounced_kind(kind: &notify::EventKind) -> Option<WatchChangeKind> {
    match kind {
        notify::EventKind::Create(_) => Some(WatchChangeKind::Create),
        notify::EventKind::Modify(_) => Some(WatchChangeKind::Modify),
        notify::EventKind::Remove(_) => Some(WatchChangeKind::Delete),
        notify::EventKind::Access(_) => None,
        notify::EventKind::Any | notify::EventKind::Other => Some(WatchChangeKind::Modify),
    }
}

/// Returns true if the path looks like an editor temporary file.
fn is_temp_file(relative_path: &str) -> bool {
    let name = Path::new(relative_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(relative_path);

    // Pattern: "file.tmp.XXXXX" (e.g. README.md.tmp.74488.abea7e8eacb7)
    if name.contains(".tmp.") {
        return true;
    }
    // Vim swap files
    if name.ends_with(".swp") || name.ends_with(".swo") {
        return true;
    }
    // Emacs backup/lock files
    if (name.starts_with('#') && name.ends_with('#')) || name.ends_with('~') {
        return true;
    }
    false
}

/// Get the parent directory of a relative path (as a string).
/// Returns "" for top-level files.
///
/// Kept for tests / potential future callers; current dispatch logic uses
/// `resolve_subscribed_path` which walks the full ancestor chain instead of
/// looking up a single parent.
#[allow(dead_code)]
pub(crate) fn parent_dir(relative_path: &str) -> String {
    match Path::new(relative_path).parent() {
        Some(p) if p == Path::new("") => String::new(),
        Some(p) => p.to_string_lossy().into_owned(),
        None => String::new(),
    }
}

/// Build the ancestor chain of dirs for `relative_path`:
/// `chain[0]` = parent(path), `chain[1]` = grandparent(path), …,
/// `chain[last]` = `""` (workspace root).
///
/// For a top-level path like `"main.rs"`, returns `[""]`.
pub(crate) fn ancestor_chain(relative_path: &str) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    let mut cursor = Path::new(relative_path);
    while let Some(parent) = cursor.parent() {
        let s = if parent == Path::new("") {
            String::new()
        } else {
            parent.to_string_lossy().into_owned()
        };
        chain.push(s);
        if parent == Path::new("") {
            break;
        }
        cursor = parent;
    }
    chain
}

/// Walk the ancestor chain of `relative_path` (starting from its parent
/// directory upward to the workspace root represented as `""`) and find the
/// NEAREST directory that satisfies `is_subscribed`.
///
/// Returns `(subscribed_dir, rewritten_path)`:
/// - If the subscribed directory is exactly the path's parent, `rewritten_path`
///   is `relative_path` itself (direct child case — original event applies).
/// - Otherwise `rewritten_path` is the subscriber's DIRECT CHILD along the
///   ancestor chain (one level shallower than the subscriber would be too
///   shallow; one level deeper is the next ancestor toward the change).
///
/// Returns `None` if no ancestor in the chain is subscribed.
///
/// This pure function is exposed primarily for unit testing the rewrite
/// rule in isolation; the actual dispatch path uses `ancestor_chain` +
/// per-connection subscriber lookup instead so multiple subscribers at
/// different ancestor depths each get their own nearest-ancestor rewrite.
#[allow(dead_code)]
pub(crate) fn resolve_subscribed_path<F>(relative_path: &str, is_subscribed: F) -> Option<(String, String)>
where
    F: Fn(&str) -> bool,
{
    let chain = ancestor_chain(relative_path);
    for (i, dir) in chain.iter().enumerate() {
        if is_subscribed(dir) {
            let rewritten = if i == 0 {
                // Subscriber is the direct parent → keep original path.
                relative_path.to_owned()
            } else {
                // Subscriber is an ancestor → rewrite to its direct child along
                // the chain (one entry shallower toward the change).
                chain[i - 1].clone()
            };
            return Some((dir.clone(), rewritten));
        }
    }
    None
}

/// Extract the file extension from a relative path (lowercase).
fn file_extension(relative_path: &str) -> Option<String> {
    Path::new(relative_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// WorkspaceWatchManager
// ---------------------------------------------------------------------------

/// Top-level manager that owns shared watchers and coordinates lifecycle.
///
/// Implements `WatcherLifecycle` so the router can trigger start/stop.
pub struct WorkspaceWatchManager {
    shared_watchers: DashMap<String, Arc<SharedWorkspaceWatcher>>,
    event_tx: mpsc::UnboundedSender<DebouncedBatch>,
}

impl WorkspaceWatchManager {
    pub fn new(event_tx: mpsc::UnboundedSender<DebouncedBatch>) -> Self {
        Self {
            shared_watchers: DashMap::new(),
            event_tx,
        }
    }
}

impl crate::workspace_watcher_router::WatcherLifecycle for WorkspaceWatchManager {
    fn start_workspace_watch(&self, workspace: &str) {
        if self.shared_watchers.contains_key(workspace) {
            return;
        }
        match SharedWorkspaceWatcher::new(workspace, self.event_tx.clone()) {
            Ok(watcher) => {
                debug!(workspace, "workspace watcher started (debouncer-full)");
                self.shared_watchers.insert(workspace.to_owned(), Arc::new(watcher));
            }
            Err(e) => {
                warn!(workspace, error = %e, "failed to start workspace watcher");
            }
        }
    }

    fn stop_workspace_watch(&self, workspace: &str) {
        if self.shared_watchers.remove(workspace).is_some() {
            debug!(workspace, "workspace watcher stopped");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_debounced_kind_create() {
        assert_eq!(
            map_debounced_kind(&notify::EventKind::Create(notify::event::CreateKind::File)),
            Some(WatchChangeKind::Create)
        );
    }

    #[test]
    fn map_debounced_kind_modify() {
        assert_eq!(
            map_debounced_kind(&notify::EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content
            ))),
            Some(WatchChangeKind::Modify)
        );
    }

    #[test]
    fn map_debounced_kind_rename_is_modify() {
        assert_eq!(
            map_debounced_kind(&notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::Both
            ))),
            Some(WatchChangeKind::Modify)
        );
    }

    #[test]
    fn map_debounced_kind_remove() {
        assert_eq!(
            map_debounced_kind(&notify::EventKind::Remove(notify::event::RemoveKind::File)),
            Some(WatchChangeKind::Delete)
        );
    }

    #[test]
    fn map_debounced_kind_access_is_none() {
        assert_eq!(
            map_debounced_kind(&notify::EventKind::Access(notify::event::AccessKind::Read)),
            None
        );
    }

    #[test]
    fn parent_dir_top_level() {
        assert_eq!(parent_dir("main.rs"), "");
    }

    #[test]
    fn parent_dir_nested() {
        assert_eq!(parent_dir("src/main.rs"), "src");
    }

    #[test]
    fn parent_dir_deeply_nested() {
        assert_eq!(parent_dir("src/components/Button.tsx"), "src/components");
    }

    #[test]
    fn watch_change_serialization() {
        let change = WatchChange {
            path: "src/new_file.rs".into(),
            kind: WatchChangeKind::Create,
        };
        let json = serde_json::to_value(&change).unwrap();
        assert_eq!(json["path"], "src/new_file.rs");
        assert_eq!(json["kind"], "create");
    }

    #[test]
    fn watch_batch_event_serialization() {
        let event = WatchBatchEvent {
            workspace: "/project".into(),
            changes: vec![
                WatchChange {
                    path: "src/a.rs".into(),
                    kind: WatchChangeKind::Create,
                },
                WatchChange {
                    path: "src/b.rs".into(),
                    kind: WatchChangeKind::Delete,
                },
            ],
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["workspace"], "/project");
        assert_eq!(json["changes"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn watch_overflow_event_serialization() {
        let event = WatchOverflowEvent {
            workspace: "/project".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["workspace"], "/project");
    }

    #[test]
    fn gitignore_filter_always_ignores_git_dir() {
        let tmp = std::env::temp_dir().join("test_gitignore_ws");
        let _ = std::fs::create_dir_all(&tmp);
        let ws = tmp.to_string_lossy().into_owned();

        let filter = GitignoreFilter::new();
        filter.rebuild(&ws);
        assert!(filter.is_ignored(&ws, ".git/config", false));
        assert!(filter.is_ignored(&ws, ".git", true));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn gitignore_filter_non_ignored_passes() {
        let tmp = std::env::temp_dir().join("test_gitignore_ws2");
        let _ = std::fs::create_dir_all(&tmp);
        let ws = tmp.to_string_lossy().into_owned();

        let filter = GitignoreFilter::new();
        filter.rebuild(&ws);
        assert!(!filter.is_ignored(&ws, "src/main.rs", false));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn no_rename_kind_exists() {
        let json = serde_json::to_string(&WatchChangeKind::Create).unwrap();
        assert_eq!(json, "\"create\"");
        let json = serde_json::to_string(&WatchChangeKind::Modify).unwrap();
        assert_eq!(json, "\"modify\"");
        let json = serde_json::to_string(&WatchChangeKind::Delete).unwrap();
        assert_eq!(json, "\"delete\"");
    }

    // -------------------------------------------------------------------
    // resolve_subscribed_path: pure-function unit tests
    // -------------------------------------------------------------------

    #[test]
    fn resolve_subscribed_path_subscriber_is_direct_parent() {
        // Subscribed: "src". Event: "src/main.tsx" → direct child case.
        let resolved = resolve_subscribed_path("src/main.tsx", |d| d == "src");
        assert_eq!(resolved, Some(("src".to_string(), "src/main.tsx".to_string())));
    }

    #[test]
    fn resolve_subscribed_path_subscriber_is_ancestor_rewrites_to_child() {
        // Subscribed: "src". Event: "src/docs/main.md" → rewrite to "src/docs".
        let resolved = resolve_subscribed_path("src/docs/main.md", |d| d == "src");
        assert_eq!(resolved, Some(("src".to_string(), "src/docs".to_string())));
    }

    #[test]
    fn resolve_subscribed_path_root_subscriber_for_deep_change() {
        // Subscribed: "" (root). Event: "src/docs/main.md" → rewrite to "src".
        let resolved = resolve_subscribed_path("src/docs/main.md", |d| d.is_empty());
        assert_eq!(resolved, Some((String::new(), "src".to_string())));
    }

    #[test]
    fn resolve_subscribed_path_picks_nearest_ancestor() {
        // Subscribed: "" AND "src". Nearest is "src" (not root).
        let resolved = resolve_subscribed_path("src/docs/main.md", |d| d.is_empty() || d == "src");
        assert_eq!(resolved, Some(("src".to_string(), "src/docs".to_string())));
    }

    #[test]
    fn resolve_subscribed_path_no_subscriber_returns_none() {
        let resolved = resolve_subscribed_path("src/docs/main.md", |_| false);
        assert_eq!(resolved, None);
    }

    #[test]
    fn resolve_subscribed_path_top_level_file_with_root_subscriber() {
        // Subscribed: "". Event: "main.rs" → direct child of root.
        let resolved = resolve_subscribed_path("main.rs", |d| d.is_empty());
        assert_eq!(resolved, Some((String::new(), "main.rs".to_string())));
    }

    // -------------------------------------------------------------------
    // dispatch_changes: integration tests with a real registry + ws manager
    // -------------------------------------------------------------------

    use crate::workspace_watcher_registry::SubscriptionRegistry;
    use aionui_realtime::{ConnectionId, PER_CONNECTION_BUFFER, WsOutbound};

    fn build_dispatcher() -> (Arc<EventDispatcher>, Arc<SubscriptionRegistry>, Arc<WebSocketManager>) {
        let registry = Arc::new(SubscriptionRegistry::new());
        let ws = Arc::new(WebSocketManager::new());
        let gitignore = Arc::new(GitignoreFilter::new());
        let dispatcher = Arc::new(EventDispatcher::new(registry.clone(), ws.clone(), gitignore));
        (dispatcher, registry, ws)
    }

    fn add_test_client(ws: &WebSocketManager) -> (ConnectionId, mpsc::Receiver<WsOutbound>) {
        let (tx, rx) = mpsc::channel(PER_CONNECTION_BUFFER);
        let id = ws.add_client("token".into(), tx);
        (id, rx)
    }

    fn drain_changed_events(rx: &mut mpsc::Receiver<WsOutbound>) -> Vec<WatchBatchEvent> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            let WsOutbound::Text(text) = msg else { continue };
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            if v["name"] == "workspace.changed" {
                let evt: WatchBatchEvent = serde_json::from_value(v["data"].clone()).unwrap();
                out.push(evt);
            }
        }
        out
    }

    #[test]
    fn dispatch_deep_change_rewrites_to_subscriber_child() {
        let (dispatcher, registry, ws) = build_dispatcher();
        let (conn, mut rx) = add_test_client(&ws);
        registry.subscribe(conn, "/ws", &["src".into()]);

        dispatcher.dispatch_changes(
            "/ws",
            vec![WatchChange {
                path: "src/docs/main.md".into(),
                kind: WatchChangeKind::Create,
            }],
        );

        let events = drain_changed_events(&mut rx);
        assert_eq!(events.len(), 1, "expected one batch event");
        assert_eq!(events[0].changes.len(), 1);
        assert_eq!(events[0].changes[0].path, "src/docs");
        assert_eq!(events[0].changes[0].kind, WatchChangeKind::Modify);
    }

    #[test]
    fn dispatch_direct_child_keeps_original_kind() {
        let (dispatcher, registry, ws) = build_dispatcher();
        let (conn, mut rx) = add_test_client(&ws);
        registry.subscribe(conn, "/ws", &["src".into()]);

        dispatcher.dispatch_changes(
            "/ws",
            vec![WatchChange {
                path: "src/main.tsx".into(),
                kind: WatchChangeKind::Create,
            }],
        );

        let events = drain_changed_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].changes.len(), 1);
        assert_eq!(events[0].changes[0].path, "src/main.tsx");
        assert_eq!(events[0].changes[0].kind, WatchChangeKind::Create);
    }

    #[test]
    fn dispatch_nearest_ancestor_wins() {
        let (dispatcher, registry, ws) = build_dispatcher();
        let (conn_root, mut rx_root) = add_test_client(&ws);
        let (conn_src, mut rx_src) = add_test_client(&ws);
        registry.subscribe(conn_root, "/ws", &["".into()]);
        registry.subscribe(conn_src, "/ws", &["src".into()]);

        dispatcher.dispatch_changes(
            "/ws",
            vec![WatchChange {
                path: "src/docs/main.md".into(),
                kind: WatchChangeKind::Create,
            }],
        );

        let root_events = drain_changed_events(&mut rx_root);
        assert_eq!(root_events.len(), 1, "root subscriber should receive 1 event");
        assert_eq!(root_events[0].changes.len(), 1);
        assert_eq!(root_events[0].changes[0].path, "src");
        assert_eq!(root_events[0].changes[0].kind, WatchChangeKind::Modify);

        let src_events = drain_changed_events(&mut rx_src);
        assert_eq!(src_events.len(), 1, "src subscriber should receive 1 event");
        assert_eq!(src_events[0].changes.len(), 1);
        assert_eq!(src_events[0].changes[0].path, "src/docs");
        assert_eq!(src_events[0].changes[0].kind, WatchChangeKind::Modify);
    }

    #[test]
    fn dispatch_multiple_deep_changes_dedup() {
        let (dispatcher, registry, ws) = build_dispatcher();
        let (conn, mut rx) = add_test_client(&ws);
        registry.subscribe(conn, "/ws", &["src".into()]);

        dispatcher.dispatch_changes(
            "/ws",
            vec![
                WatchChange {
                    path: "src/docs/a.md".into(),
                    kind: WatchChangeKind::Create,
                },
                WatchChange {
                    path: "src/docs/b.md".into(),
                    kind: WatchChangeKind::Modify,
                },
            ],
        );

        let events = drain_changed_events(&mut rx);
        assert_eq!(events.len(), 1);
        // Both deep changes collapse into a single (path="src/docs", kind=Modify) entry.
        assert_eq!(
            events[0].changes.len(),
            1,
            "expected dedup'd to 1 change, got {:?}",
            events[0].changes
        );
        assert_eq!(events[0].changes[0].path, "src/docs");
        assert_eq!(events[0].changes[0].kind, WatchChangeKind::Modify);
    }

    #[test]
    fn dispatch_no_subscriber_drops_event() {
        let (dispatcher, _registry, ws) = build_dispatcher();
        let (_conn, mut rx) = add_test_client(&ws);
        // No subscription at all.

        dispatcher.dispatch_changes(
            "/ws",
            vec![WatchChange {
                path: "src/docs/main.md".into(),
                kind: WatchChangeKind::Create,
            }],
        );

        let events = drain_changed_events(&mut rx);
        assert!(events.is_empty(), "no subscriber → no event");
    }
}
