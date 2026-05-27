//! WebSocket event types for the workspace file watcher.
//!
//! Emitted by `aionui-file::workspace_watcher` and consumed by the frontend
//! via `WebSocketMessage<T>`.

use serde::{Deserialize, Serialize};

/// Kind of file-system change detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WatchChangeKind {
    Create,
    Modify,
    Delete,
}

/// A single file-system change within a workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchChange {
    pub path: String,
    pub kind: WatchChangeKind,
}

/// Batch event pushed to subscribed connections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchBatchEvent {
    pub workspace: String,
    pub changes: Vec<WatchChange>,
}

/// Overflow event when too many changes occur in a single batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchOverflowEvent {
    pub workspace: String,
}
