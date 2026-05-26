use crate::agent_runtime::AgentRuntime;
use crate::protocol::acp::{PermissionDecision, PermissionRequest};
use crate::protocol::events::{AgentStreamEvent, permission_request_to_event_data};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::debug;

/// MCP tool prefixes that are auto-approved without user permission.
///
/// `mcp__aionui-team__` matches the team stdio bridge tools (server name is now
/// the fixed `aionui-team` — see `TEAM_MCP_SERVER_NAME`).
/// `mcp__aionui-team-guide__` matches the solo-session Guide tools.
const AUTO_APPROVE_PREFIXES: &[&str] = &["mcp__aionui-team__", "mcp__aionui-team-guide__"];

/// Routes ACP permission requests from the protocol layer to the user
/// (via `event_tx`) and back (via `confirm`). Owns the receiver channel
/// for incoming permission requests, the pending responder map, and the
/// `closing` flag that prevents new requests from being routed after a
/// graceful shutdown has started.
pub struct PermissionRouter {
    /// Receiver for permission requests from the protocol layer.
    permission_rx: Mutex<mpsc::Receiver<PermissionRequest>>,
    /// Pending ACP permission responders keyed by tool call ID.
    pending_permissions: StdMutex<HashMap<String, oneshot::Sender<PermissionDecision>>>,
    /// Whether a graceful shutdown is in progress.
    closing: AtomicBool,
}

impl PermissionRouter {
    /// Create a new permission router.
    pub fn new(permission_rx: mpsc::Receiver<PermissionRequest>) -> Self {
        Self {
            permission_rx: Mutex::new(permission_rx),
            pending_permissions: StdMutex::new(HashMap::new()),
            closing: AtomicBool::new(false),
        }
    }

    /// Start the permission handler loop.
    ///
    /// This background task receives permission requests from the protocol
    /// layer, converts them to `Permission` events, and waits for user
    /// responses routed through the `confirm()` method.
    ///
    /// `runtime` is shared with the parent manager so permission
    /// arrivals count as activity (preventing idle timeouts) via
    /// `runtime.bump_activity()`.
    pub fn start(self: &Arc<Self>, runtime: AgentRuntime) {
        let this = Arc::clone(self);

        tokio::spawn(async move {
            let mut rx = this.permission_rx.lock().await;

            while let Some(perm_req) = rx.recv().await {
                runtime.bump_activity();

                let call_id = perm_req.request.tool_call.tool_call_id.to_string();

                // Auto-approve team MCP tools without user interaction.
                if is_auto_approve_tool(&perm_req.request) {
                    let _ = perm_req.response_tx.send(PermissionDecision::Selected {
                        option_id: "allow_always".into(),
                    });
                    continue;
                }

                let mut pending = this.pending_permissions.lock().unwrap();
                if let Some(previous) = pending.insert(call_id.clone(), perm_req.response_tx) {
                    let _ = previous.send(PermissionDecision::Cancelled);
                }
                drop(pending);

                let permission_event = permission_request_to_event_data(&perm_req.request);

                if runtime
                    .event_sender()
                    .send(AgentStreamEvent::AcpPermission(permission_event))
                    .is_err()
                    && let Some(response_tx) = this.pending_permissions.lock().unwrap().remove(&call_id)
                {
                    let _ = response_tx.send(PermissionDecision::Cancelled);
                }
            }
        });
    }

    /// Resolve a pending permission request with the user's selected option.
    pub fn confirm(
        &self,
        call_id: &str,
        option_id: String,
        conversation_id: &str,
    ) -> Result<(), aionui_common::AppError> {
        let responder = self
            .pending_permissions
            .lock()
            .unwrap()
            .remove(call_id)
            .ok_or_else(|| {
                aionui_common::AppError::BadRequest(format!("Pending ACP permission not found: {call_id}"))
            })?;

        responder
            .send(PermissionDecision::Selected { option_id })
            .map_err(|_| aionui_common::AppError::BadRequest(format!("Pending ACP permission expired: {call_id}")))?;

        debug!(conversation_id = %conversation_id, call_id, "ACP permission response forwarded");
        Ok(())
    }

    /// Cancel all pending permission requests. Called during `stop()` and `kill()`.
    pub fn cancel_all(&self) {
        for (_, responder) in self.pending_permissions.lock().unwrap().drain() {
            let _ = responder.send(PermissionDecision::Cancelled);
        }
    }

    /// Whether a graceful shutdown is in progress.
    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    /// Mark the router as closing (graceful shutdown in progress).
    pub fn set_closing(&self) {
        self.closing.store(true, Ordering::Release);
    }
}

fn is_auto_approve_tool(request: &agent_client_protocol::schema::RequestPermissionRequest) -> bool {
    let title = request.tool_call.fields.title.as_deref().unwrap_or("");
    AUTO_APPROVE_PREFIXES.iter().any(|prefix| title.starts_with(prefix))
}
