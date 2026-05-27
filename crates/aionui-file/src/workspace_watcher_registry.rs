//! Per-connection subscription registry for workspace directory watches.

use std::collections::{HashMap, HashSet};

use dashmap::DashMap;

use aionui_realtime::ConnectionId;

// ---------------------------------------------------------------------------
// Per-connection state
// ---------------------------------------------------------------------------

/// Subscriptions held by a single WebSocket connection.
#[derive(Debug, Default)]
struct PerConnectionState {
    /// workspace path -> set of subscribed relative directories
    subscriptions: HashMap<String, HashSet<String>>,
    /// workspace path -> set of subscribed file extensions (e.g. "docx", "pptx")
    extension_subscriptions: HashMap<String, HashSet<String>>,
}

impl PerConnectionState {
    /// Returns true if this connection has any subscription (dirs or extensions) for the workspace.
    fn has_workspace(&self, workspace: &str) -> bool {
        let has_dirs = self.subscriptions.get(workspace).is_some_and(|s| !s.is_empty());
        let has_exts = self
            .extension_subscriptions
            .get(workspace)
            .is_some_and(|s| !s.is_empty());
        has_dirs || has_exts
    }
}

// ---------------------------------------------------------------------------
// SubscriptionRegistry
// ---------------------------------------------------------------------------

/// Tracks which connections are subscribed to which workspace directories.
///
/// Thread-safe via `DashMap`; designed for concurrent access from
/// multiple WS handler tasks.
pub struct SubscriptionRegistry {
    connections: DashMap<ConnectionId, PerConnectionState>,
    /// workspace -> number of connections subscribed (for watcher lifecycle).
    workspace_refcount: DashMap<String, usize>,
}

impl Default for SubscriptionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SubscriptionRegistry {
    pub fn new() -> Self {
        Self {
            connections: DashMap::new(),
            workspace_refcount: DashMap::new(),
        }
    }

    /// Subscribe a connection to one or more directories in a workspace.
    ///
    /// Returns `true` if this is the first subscription for this workspace
    /// (caller should create the OS watcher).
    pub fn subscribe(&self, conn_id: ConnectionId, workspace: &str, dirs: &[String]) -> bool {
        let mut conn = self.connections.entry(conn_id).or_default();

        let had_any = conn.has_workspace(workspace);
        let dir_set = conn.subscriptions.entry(workspace.to_owned()).or_default();
        for dir in dirs {
            dir_set.insert(dir.clone());
        }
        let has_any = conn.has_workspace(workspace);

        self.update_refcount(workspace, had_any, has_any)
    }

    /// Subscribe a connection to file extensions in a workspace (full-recursive matching).
    ///
    /// Returns `true` if this is the first subscription for this workspace.
    pub fn subscribe_extensions(&self, conn_id: ConnectionId, workspace: &str, extensions: &[String]) -> bool {
        let mut conn = self.connections.entry(conn_id).or_default();

        let had_any = conn.has_workspace(workspace);
        let ext_set = conn.extension_subscriptions.entry(workspace.to_owned()).or_default();
        for ext in extensions {
            ext_set.insert(ext.to_ascii_lowercase());
        }
        let has_any = conn.has_workspace(workspace);

        self.update_refcount(workspace, had_any, has_any)
    }

    /// Unsubscribe a connection from directories in a workspace.
    /// If a parent dir is unsubscribed, all child dirs are also removed.
    ///
    /// Returns `true` if this was the last subscription for this workspace
    /// (caller should destroy the OS watcher).
    pub fn unsubscribe(&self, conn_id: ConnectionId, workspace: &str, dirs: &[String]) -> bool {
        let mut conn = match self.connections.get_mut(&conn_id) {
            Some(c) => c,
            None => return false,
        };

        let had_any = conn.has_workspace(workspace);

        let dir_set = match conn.subscriptions.get_mut(workspace) {
            Some(s) => s,
            None => return false,
        };

        for dir in dirs {
            dir_set.remove(dir.as_str());
            let prefix = if dir.is_empty() {
                dir_set.clear();
                break;
            } else {
                format!("{dir}/")
            };
            dir_set.retain(|d| !d.starts_with(&prefix));
        }

        if dir_set.is_empty() {
            conn.subscriptions.remove(workspace);
        }

        let has_any = conn.has_workspace(workspace);
        drop(conn);
        self.update_refcount(workspace, had_any, has_any)
    }

    /// Unsubscribe a connection from file extensions in a workspace.
    ///
    /// Returns `true` if this was the last subscription for this workspace.
    pub fn unsubscribe_extensions(&self, conn_id: ConnectionId, workspace: &str, extensions: &[String]) -> bool {
        let mut conn = match self.connections.get_mut(&conn_id) {
            Some(c) => c,
            None => return false,
        };

        let had_any = conn.has_workspace(workspace);

        let ext_set = match conn.extension_subscriptions.get_mut(workspace) {
            Some(s) => s,
            None => return false,
        };

        for ext in extensions {
            ext_set.remove(&ext.to_ascii_lowercase());
        }

        if ext_set.is_empty() {
            conn.extension_subscriptions.remove(workspace);
        }

        let has_any = conn.has_workspace(workspace);
        drop(conn);
        self.update_refcount(workspace, had_any, has_any)
    }

    /// Remove all subscriptions for a connection (called on WS disconnect).
    ///
    /// Returns the list of workspaces that lost their last subscriber.
    pub fn remove_connection(&self, conn_id: ConnectionId) -> Vec<String> {
        let mut orphaned_workspaces = Vec::new();

        if let Some((_, state)) = self.connections.remove(&conn_id) {
            let mut workspaces: HashSet<String> = HashSet::new();
            for (ws, dirs) in &state.subscriptions {
                if !dirs.is_empty() {
                    workspaces.insert(ws.clone());
                }
            }
            for (ws, exts) in &state.extension_subscriptions {
                if !exts.is_empty() {
                    workspaces.insert(ws.clone());
                }
            }

            for workspace in workspaces {
                let mut rc = self.workspace_refcount.entry(workspace.clone()).or_insert(0);
                *rc = rc.saturating_sub(1);
                if *rc == 0 {
                    orphaned_workspaces.push(workspace.clone());
                    drop(rc);
                    self.workspace_refcount.remove(&workspace);
                }
            }
        }

        orphaned_workspaces
    }

    /// Get all connection IDs subscribed to a specific directory in a workspace.
    pub fn get_subscribers_for_dir(&self, workspace: &str, dir: &str) -> Vec<ConnectionId> {
        let mut result = Vec::new();
        for entry in self.connections.iter() {
            let conn_id = *entry.key();
            if let Some(dirs) = entry.value().subscriptions.get(workspace)
                && dirs.contains(dir)
            {
                result.push(conn_id);
            }
        }
        result
    }

    /// Get all connection IDs subscribed to a specific file extension in a workspace.
    pub fn get_subscribers_for_extension(&self, workspace: &str, extension: &str) -> Vec<ConnectionId> {
        let lower = extension.to_ascii_lowercase();
        let mut result = Vec::new();
        for entry in self.connections.iter() {
            let conn_id = *entry.key();
            if let Some(exts) = entry.value().extension_subscriptions.get(workspace)
                && exts.contains(&lower)
            {
                result.push(conn_id);
            }
        }
        result
    }

    /// Update refcount when a connection's subscription state changes.
    /// Returns `true` if the transition is 0→1 (is_first) or 1→0 (is_last).
    fn update_refcount(&self, workspace: &str, had_any: bool, has_any: bool) -> bool {
        match (had_any, has_any) {
            (false, true) => {
                // New subscriber for this workspace
                let mut rc = self.workspace_refcount.entry(workspace.to_owned()).or_insert(0);
                let is_first = *rc == 0;
                *rc += 1;
                is_first
            }
            (true, false) => {
                // Lost subscriber for this workspace
                let mut rc = self.workspace_refcount.entry(workspace.to_owned()).or_insert(0);
                *rc = rc.saturating_sub(1);
                let is_last = *rc == 0;
                if is_last {
                    drop(rc);
                    self.workspace_refcount.remove(workspace);
                }
                is_last
            }
            _ => false,
        }
    }

    /// Get workspace reference count (for testing / diagnostics).
    pub fn workspace_refcount(&self, workspace: &str) -> usize {
        self.workspace_refcount.get(workspace).map(|v| *v).unwrap_or(0)
    }

    /// Get total number of tracked connections.
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_first_returns_true() {
        let reg = SubscriptionRegistry::new();
        let first = reg.subscribe(ConnectionId(1), "/ws", &["src".into()]);
        assert!(first);
    }

    #[test]
    fn subscribe_second_connection_returns_false() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe(ConnectionId(1), "/ws", &["src".into()]);
        let second = reg.subscribe(ConnectionId(2), "/ws", &["docs".into()]);
        assert!(!second);
    }

    #[test]
    fn unsubscribe_last_returns_true() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe(ConnectionId(1), "/ws", &["src".into()]);
        let last = reg.unsubscribe(ConnectionId(1), "/ws", &["src".into()]);
        assert!(last);
        assert_eq!(reg.workspace_refcount("/ws"), 0);
    }

    #[test]
    fn unsubscribe_not_last_returns_false() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe(ConnectionId(1), "/ws", &["src".into()]);
        reg.subscribe(ConnectionId(2), "/ws", &["docs".into()]);
        let last = reg.unsubscribe(ConnectionId(1), "/ws", &["src".into()]);
        assert!(!last);
        assert_eq!(reg.workspace_refcount("/ws"), 1);
    }

    #[test]
    fn unsubscribe_cascades_children() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe(
            ConnectionId(1),
            "/ws",
            &["src".into(), "src/components".into(), "src/utils".into()],
        );
        reg.unsubscribe(ConnectionId(1), "/ws", &["src".into()]);
        // All src/* should be removed too
        let subs = reg.get_subscribers_for_dir("/ws", "src/components");
        assert!(subs.is_empty());
    }

    #[test]
    fn unsubscribe_root_clears_all() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe(ConnectionId(1), "/ws", &["src".into(), "docs".into(), "".into()]);
        reg.unsubscribe(ConnectionId(1), "/ws", &["".into()]);
        let subs = reg.get_subscribers_for_dir("/ws", "src");
        assert!(subs.is_empty());
    }

    #[test]
    fn remove_connection_cleans_all_subscriptions() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe(ConnectionId(1), "/ws1", &["src".into()]);
        reg.subscribe(ConnectionId(1), "/ws2", &["docs".into()]);
        let orphaned = reg.remove_connection(ConnectionId(1));
        assert_eq!(orphaned.len(), 2);
        assert_eq!(reg.connection_count(), 0);
    }

    #[test]
    fn remove_connection_partial_refcount() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe(ConnectionId(1), "/ws", &["src".into()]);
        reg.subscribe(ConnectionId(2), "/ws", &["docs".into()]);
        let orphaned = reg.remove_connection(ConnectionId(1));
        assert!(orphaned.is_empty());
        assert_eq!(reg.workspace_refcount("/ws"), 1);
    }

    #[test]
    fn get_subscribers_for_dir_filters_correctly() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe(ConnectionId(1), "/ws", &["src".into()]);
        reg.subscribe(ConnectionId(2), "/ws", &["docs".into()]);
        reg.subscribe(ConnectionId(3), "/ws", &["src".into()]);

        let subs = reg.get_subscribers_for_dir("/ws", "src");
        assert_eq!(subs.len(), 2);
        assert!(subs.contains(&ConnectionId(1)));
        assert!(subs.contains(&ConnectionId(3)));
    }

    #[test]
    fn get_subscribers_empty_for_unsubscribed_dir() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe(ConnectionId(1), "/ws", &["src".into()]);
        let subs = reg.get_subscribers_for_dir("/ws", "docs");
        assert!(subs.is_empty());
    }

    #[test]
    fn multiple_dirs_same_connection() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe(ConnectionId(1), "/ws", &["src".into(), "docs".into()]);
        let src_subs = reg.get_subscribers_for_dir("/ws", "src");
        let docs_subs = reg.get_subscribers_for_dir("/ws", "docs");
        assert_eq!(src_subs.len(), 1);
        assert_eq!(docs_subs.len(), 1);
    }

    // --- Extension subscription tests ---

    #[test]
    fn subscribe_extensions_first_returns_true() {
        let reg = SubscriptionRegistry::new();
        let first = reg.subscribe_extensions(ConnectionId(1), "/ws", &["docx".into()]);
        assert!(first);
        assert_eq!(reg.workspace_refcount("/ws"), 1);
    }

    #[test]
    fn subscribe_extensions_second_connection_returns_false() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe_extensions(ConnectionId(1), "/ws", &["docx".into()]);
        let second = reg.subscribe_extensions(ConnectionId(2), "/ws", &["pptx".into()]);
        assert!(!second);
        assert_eq!(reg.workspace_refcount("/ws"), 2);
    }

    #[test]
    fn subscribe_extensions_case_insensitive() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe_extensions(ConnectionId(1), "/ws", &["DOCX".into(), "Pptx".into()]);
        let subs = reg.get_subscribers_for_extension("/ws", "docx");
        assert_eq!(subs.len(), 1);
        let subs = reg.get_subscribers_for_extension("/ws", "pptx");
        assert_eq!(subs.len(), 1);
    }

    #[test]
    fn unsubscribe_extensions_last_returns_true() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe_extensions(ConnectionId(1), "/ws", &["docx".into()]);
        let last = reg.unsubscribe_extensions(ConnectionId(1), "/ws", &["docx".into()]);
        assert!(last);
        assert_eq!(reg.workspace_refcount("/ws"), 0);
    }

    #[test]
    fn unsubscribe_extensions_partial_not_last() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe_extensions(ConnectionId(1), "/ws", &["docx".into(), "pptx".into()]);
        let last = reg.unsubscribe_extensions(ConnectionId(1), "/ws", &["docx".into()]);
        assert!(!last);
        assert_eq!(reg.workspace_refcount("/ws"), 1);
    }

    #[test]
    fn dirs_and_extensions_share_refcount() {
        let reg = SubscriptionRegistry::new();
        // First subscription via dirs
        let first = reg.subscribe(ConnectionId(1), "/ws", &["src".into()]);
        assert!(first);
        // Extensions on same connection/workspace — not first (already has dirs)
        let second = reg.subscribe_extensions(ConnectionId(1), "/ws", &["docx".into()]);
        assert!(!second);
        assert_eq!(reg.workspace_refcount("/ws"), 1);
        // Unsubscribe dirs — still has extensions, not last
        let last = reg.unsubscribe(ConnectionId(1), "/ws", &["src".into()]);
        assert!(!last);
        assert_eq!(reg.workspace_refcount("/ws"), 1);
        // Unsubscribe extensions — now truly last
        let last = reg.unsubscribe_extensions(ConnectionId(1), "/ws", &["docx".into()]);
        assert!(last);
        assert_eq!(reg.workspace_refcount("/ws"), 0);
    }

    #[test]
    fn get_subscribers_for_extension_filters_correctly() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe_extensions(ConnectionId(1), "/ws", &["docx".into(), "pptx".into()]);
        reg.subscribe_extensions(ConnectionId(2), "/ws", &["xlsx".into()]);
        reg.subscribe_extensions(ConnectionId(3), "/ws", &["docx".into()]);

        let subs = reg.get_subscribers_for_extension("/ws", "docx");
        assert_eq!(subs.len(), 2);
        assert!(subs.contains(&ConnectionId(1)));
        assert!(subs.contains(&ConnectionId(3)));

        let subs = reg.get_subscribers_for_extension("/ws", "xlsx");
        assert_eq!(subs.len(), 1);
        assert!(subs.contains(&ConnectionId(2)));
    }

    #[test]
    fn remove_connection_cleans_extensions() {
        let reg = SubscriptionRegistry::new();
        reg.subscribe_extensions(ConnectionId(1), "/ws", &["docx".into()]);
        reg.subscribe(ConnectionId(1), "/ws", &["src".into()]);
        let orphaned = reg.remove_connection(ConnectionId(1));
        assert_eq!(orphaned.len(), 1);
        assert_eq!(reg.workspace_refcount("/ws"), 0);
    }

    #[test]
    fn extensions_only_subscription_counts_for_refcount() {
        let reg = SubscriptionRegistry::new();
        // Only extensions, no dirs
        let first = reg.subscribe_extensions(ConnectionId(1), "/ws", &["pptx".into()]);
        assert!(first);
        assert_eq!(reg.workspace_refcount("/ws"), 1);
    }
}
