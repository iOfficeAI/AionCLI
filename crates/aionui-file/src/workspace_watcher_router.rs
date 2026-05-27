//! WebSocket message router for workspace file watch subscriptions.

use std::sync::Arc;

use serde::Deserialize;
use tracing::{debug, warn};

use aionui_realtime::{ConnectionId, MessageRouter};

use crate::workspace_watcher_registry::SubscriptionRegistry;

// ---------------------------------------------------------------------------
// Message payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SubscribePayload {
    workspace: String,
    #[serde(default)]
    dirs: Vec<String>,
    #[serde(default)]
    extensions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct UnsubscribePayload {
    workspace: String,
    #[serde(default)]
    dirs: Vec<String>,
    #[serde(default)]
    extensions: Vec<String>,
}

// ---------------------------------------------------------------------------
// Router callback for watcher lifecycle
// ---------------------------------------------------------------------------

/// Callback invoked when a workspace watcher needs to be created or destroyed.
pub trait WatcherLifecycle: Send + Sync {
    fn start_workspace_watch(&self, workspace: &str);
    fn stop_workspace_watch(&self, workspace: &str);
}

// ---------------------------------------------------------------------------
// WorkspaceWatchRouter
// ---------------------------------------------------------------------------

/// Routes WebSocket messages for workspace file watching.
///
/// Handles:
/// - `workspace.subscribe`: add directory subscriptions
/// - `workspace.unsubscribe`: remove directory subscriptions
/// - `on_disconnect`: clean up all subscriptions for the connection
pub struct WorkspaceWatchRouter {
    registry: Arc<SubscriptionRegistry>,
    lifecycle: Arc<dyn WatcherLifecycle>,
}

impl WorkspaceWatchRouter {
    pub fn new(registry: Arc<SubscriptionRegistry>, lifecycle: Arc<dyn WatcherLifecycle>) -> Self {
        Self { registry, lifecycle }
    }
}

impl MessageRouter for WorkspaceWatchRouter {
    fn route(&self, conn_id: ConnectionId, name: &str, data: serde_json::Value) {
        match name {
            "workspace.subscribe" => {
                let payload: SubscribePayload = match serde_json::from_value(data) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!(%conn_id, error = %e, "invalid workspace.subscribe payload");
                        return;
                    }
                };
                debug!(%conn_id, workspace = %payload.workspace, dirs = ?payload.dirs, extensions = ?payload.extensions, "workspace.subscribe");

                let mut triggered_first = false;
                if !payload.dirs.is_empty() && self.registry.subscribe(conn_id, &payload.workspace, &payload.dirs) {
                    triggered_first = true;
                }
                if !payload.extensions.is_empty()
                    && self
                        .registry
                        .subscribe_extensions(conn_id, &payload.workspace, &payload.extensions)
                {
                    triggered_first = true;
                }
                if triggered_first {
                    self.lifecycle.start_workspace_watch(&payload.workspace);
                }
            }
            "workspace.unsubscribe" => {
                let payload: UnsubscribePayload = match serde_json::from_value(data) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!(%conn_id, error = %e, "invalid workspace.unsubscribe payload");
                        return;
                    }
                };
                debug!(%conn_id, workspace = %payload.workspace, dirs = ?payload.dirs, extensions = ?payload.extensions, "workspace.unsubscribe");

                let mut triggered_last = false;
                if !payload.dirs.is_empty() && self.registry.unsubscribe(conn_id, &payload.workspace, &payload.dirs) {
                    triggered_last = true;
                }
                if !payload.extensions.is_empty()
                    && self
                        .registry
                        .unsubscribe_extensions(conn_id, &payload.workspace, &payload.extensions)
                {
                    triggered_last = true;
                }
                if triggered_last {
                    self.lifecycle.stop_workspace_watch(&payload.workspace);
                }
            }
            _ => {
                // Not our message, ignore silently
            }
        }
    }

    fn on_disconnect(&self, conn_id: ConnectionId) {
        debug!(%conn_id, "workspace watch: connection disconnected, cleaning up");
        let orphaned = self.registry.remove_connection(conn_id);
        for workspace in orphaned {
            self.lifecycle.stop_workspace_watch(&workspace);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    struct MockLifecycle {
        started: Mutex<Vec<String>>,
        stopped: Mutex<Vec<String>>,
    }

    impl MockLifecycle {
        fn new() -> Self {
            Self {
                started: Mutex::new(Vec::new()),
                stopped: Mutex::new(Vec::new()),
            }
        }
    }

    impl WatcherLifecycle for MockLifecycle {
        fn start_workspace_watch(&self, workspace: &str) {
            self.started.lock().unwrap().push(workspace.to_owned());
        }
        fn stop_workspace_watch(&self, workspace: &str) {
            self.stopped.lock().unwrap().push(workspace.to_owned());
        }
    }

    fn setup() -> (WorkspaceWatchRouter, Arc<MockLifecycle>) {
        let registry = Arc::new(SubscriptionRegistry::new());
        let lifecycle = Arc::new(MockLifecycle::new());
        let router = WorkspaceWatchRouter::new(registry, lifecycle.clone());
        (router, lifecycle)
    }

    #[test]
    fn subscribe_triggers_start_on_first() {
        let (router, lifecycle) = setup();
        let data = json!({"workspace": "/ws", "dirs": ["src"]});
        router.route(ConnectionId(1), "workspace.subscribe", data);
        assert_eq!(lifecycle.started.lock().unwrap().len(), 1);
        assert_eq!(lifecycle.started.lock().unwrap()[0], "/ws");
    }

    #[test]
    fn subscribe_does_not_trigger_start_on_second() {
        let (router, lifecycle) = setup();
        router.route(
            ConnectionId(1),
            "workspace.subscribe",
            json!({"workspace": "/ws", "dirs": ["src"]}),
        );
        router.route(
            ConnectionId(2),
            "workspace.subscribe",
            json!({"workspace": "/ws", "dirs": ["docs"]}),
        );
        assert_eq!(lifecycle.started.lock().unwrap().len(), 1);
    }

    #[test]
    fn unsubscribe_triggers_stop_on_last() {
        let (router, lifecycle) = setup();
        router.route(
            ConnectionId(1),
            "workspace.subscribe",
            json!({"workspace": "/ws", "dirs": ["src"]}),
        );
        router.route(
            ConnectionId(1),
            "workspace.unsubscribe",
            json!({"workspace": "/ws", "dirs": ["src"]}),
        );
        assert_eq!(lifecycle.stopped.lock().unwrap().len(), 1);
    }

    #[test]
    fn on_disconnect_cleans_up_and_stops() {
        let (router, lifecycle) = setup();
        router.route(
            ConnectionId(1),
            "workspace.subscribe",
            json!({"workspace": "/ws", "dirs": ["src"]}),
        );
        router.on_disconnect(ConnectionId(1));
        assert_eq!(lifecycle.stopped.lock().unwrap().len(), 1);
    }

    #[test]
    fn unknown_message_is_ignored() {
        let (router, lifecycle) = setup();
        router.route(ConnectionId(1), "conversation.send-message", json!({}));
        assert!(lifecycle.started.lock().unwrap().is_empty());
        assert!(lifecycle.stopped.lock().unwrap().is_empty());
    }

    #[test]
    fn invalid_payload_does_not_panic() {
        let (router, _) = setup();
        router.route(ConnectionId(1), "workspace.subscribe", json!("invalid"));
    }

    #[test]
    fn subscribe_extensions_triggers_start() {
        let (router, lifecycle) = setup();
        router.route(
            ConnectionId(1),
            "workspace.subscribe",
            json!({"workspace": "/ws", "extensions": ["docx", "pptx"]}),
        );
        assert_eq!(lifecycle.started.lock().unwrap().len(), 1);
    }

    #[test]
    fn subscribe_dirs_and_extensions_together() {
        let (router, lifecycle) = setup();
        router.route(
            ConnectionId(1),
            "workspace.subscribe",
            json!({"workspace": "/ws", "dirs": ["src"], "extensions": ["docx"]}),
        );
        // Only one start call even though both dirs and extensions
        assert_eq!(lifecycle.started.lock().unwrap().len(), 1);
    }

    #[test]
    fn unsubscribe_extensions_triggers_stop_when_last() {
        let (router, lifecycle) = setup();
        router.route(
            ConnectionId(1),
            "workspace.subscribe",
            json!({"workspace": "/ws", "extensions": ["docx"]}),
        );
        router.route(
            ConnectionId(1),
            "workspace.unsubscribe",
            json!({"workspace": "/ws", "extensions": ["docx"]}),
        );
        assert_eq!(lifecycle.stopped.lock().unwrap().len(), 1);
    }

    #[test]
    fn unsubscribe_extensions_does_not_stop_if_dirs_remain() {
        let (router, lifecycle) = setup();
        router.route(
            ConnectionId(1),
            "workspace.subscribe",
            json!({"workspace": "/ws", "dirs": ["src"], "extensions": ["docx"]}),
        );
        router.route(
            ConnectionId(1),
            "workspace.unsubscribe",
            json!({"workspace": "/ws", "extensions": ["docx"]}),
        );
        // dirs still subscribed, no stop
        assert!(lifecycle.stopped.lock().unwrap().is_empty());
    }

    #[test]
    fn subscribe_with_empty_dirs_and_extensions_is_noop() {
        let (router, lifecycle) = setup();
        router.route(ConnectionId(1), "workspace.subscribe", json!({"workspace": "/ws"}));
        assert!(lifecycle.started.lock().unwrap().is_empty());
    }
}
