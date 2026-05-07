//! ACP protocol layer: SDK integration for JSON-RPC communication.
//!
//! This module owns the `agent-client-protocol` SDK connection. It provides
//! typed async methods for all ACP operations (wrappers around the SDK's
//! `send_request` / `send_notification`) plus dedicated handlers for the
//! notifications and permission requests the CLI sends back.
//!
//! # Concurrency model
//!
//! We follow the SDK's documented best practice (see
//! `jsonrpc::Builder` "Event Loop and Concurrency" and
//! `jsonrpc::SentRequest::block_task` doc comments): `connect_with` runs the
//! SDK background actors on a dedicated tokio task; its `main_fn` completes
//! the `initialize` handshake, hands the resulting [`ConnectionTo<Agent>`] out
//! to this struct, and then parks on a shutdown oneshot until
//! [`AcpProtocol`] is dropped. The connection handle is `Clone + Send` and
//! is used directly by every method — outgoing requests / notifications go
//! through the SDK's own outgoing actor, so they are naturally concurrent.
//! No hand-rolled command channel is involved.
//!
//! This is what makes `session/cancel` preempt an in-flight `session/prompt`:
//! both requests are just `send_request` / `send_notification` calls on the
//! shared connection, each awaited in its own caller task.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use agent_client_protocol::schema::{
    AGENT_METHOD_NAMES, AuthenticateResponse, ClientNotification, ClientRequest, CloseSessionResponse, ExtResponse,
    ForkSessionResponse, InitializeRequest, LoadSessionResponse, PromptResponse, ProtocolVersion,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse, ResumeSessionResponse,
    SelectedPermissionOutcome, SessionNotification, SetSessionConfigOptionResponse, SetSessionModeResponse,
    SetSessionModelResponse,
};
use agent_client_protocol::{
    Agent, ByteStreams, Client, ConnectionTo, Responder, on_receive_notification, on_receive_request,
};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::{debug, warn};

use crate::protocol::error::AcpError;
use crate::protocol::events::{self as stream_event, AgentStreamEvent};

use agent_client_protocol::schema::{
    AgentCapabilities, AuthMethod, AuthenticateRequest, CancelNotification, CloseSessionRequest, ExtNotification,
    ExtRequest, ForkSessionRequest, InitializeResponse, ListSessionsRequest, ListSessionsResponse, LoadSessionRequest,
    NewSessionRequest, NewSessionResponse, PromptRequest, ResumeSessionRequest, SetSessionConfigOptionRequest,
    SetSessionModeRequest, SetSessionModelRequest,
};

/// Timeout for the ACP initialize handshake (seconds).
const INIT_TIMEOUT_SECS: u64 = 30;

/// A pending permission request from the agent, awaiting user decision.
pub struct PermissionRequest {
    /// Raw ACP permission request as defined by the SDK schema.
    pub request: RequestPermissionRequest,
    /// Channel to send the user's decision back to the SDK responder.
    pub response_tx: oneshot::Sender<PermissionDecision>,
}

/// User's decision on a permission request.
pub enum PermissionDecision {
    /// User selected a permission option.
    Selected { option_id: String },
    /// User cancelled (rejected) the request.
    Cancelled,
}

/// ACP protocol handle: wraps the SDK connection and provides typed operations.
///
/// All request methods are thin wrappers over `connection.send_request(...)
/// .block_task().await` — safe because each caller runs in its own tokio
/// task, separate from the SDK background actors spawned by `connect_with`.
pub struct AcpProtocol {
    /// SDK connection handle. Cheap to clone (channel senders only) and
    /// shared by every method. Kept alive by the background task parked
    /// on `shutdown_rx` in `connect_with`'s `main_fn`.
    connection: ConnectionTo<Agent>,
    /// Signal dropped on `Drop` to make `main_fn` return, which in turn
    /// lets the SDK background actors shut down cleanly.
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Flipped to `false` when the background task exits. Used by
    /// [`Self::is_connected`] as a fast synchronous check.
    alive: Arc<AtomicBool>,
    /// Cached initialize response from the ACP handshake.
    initialize_response: Arc<RwLock<Option<InitializeResponse>>>,
}

#[allow(dead_code)] // Full ACP method set; some methods await wiring (fork, close, list, auth, ext).
impl AcpProtocol {
    /// Connect to a running CLI process and execute the ACP initialize handshake.
    ///
    /// Takes ownership of the child's stdin/stdout (from [`CliAgentProcess::take_stdio`]).
    /// Spawns the SDK background task for JSON-RPC message routing.
    /// Returns after the initialize handshake completes successfully.
    pub async fn connect(
        stdin: ChildStdin,
        stdout: ChildStdout,
        event_tx: broadcast::Sender<AgentStreamEvent>,
        permission_tx: mpsc::Sender<PermissionRequest>,
        notification_tx: mpsc::Sender<SessionNotification>,
    ) -> Result<Self, AcpError> {
        let alive = Arc::new(AtomicBool::new(true));

        // Signals from the background task:
        // - `init_tx`: initialize handshake result (with possible SDK error)
        // - `ready_tx`: connection handle once init succeeded; if init fails
        //   this oneshot is dropped and the caller observes `NotConnected`
        let (init_tx, init_rx) = oneshot::channel::<Result<InitializeResponse, AcpError>>();
        let (ready_tx, ready_rx) = oneshot::channel::<ConnectionTo<Agent>>();

        // Signal from us → background task telling `main_fn` to return,
        // which triggers a clean SDK shutdown.
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        tokio::spawn(run_sdk_background(
            stdin,
            stdout,
            event_tx,
            permission_tx,
            notification_tx,
            init_tx,
            ready_tx,
            shutdown_rx,
            Arc::clone(&alive),
        ));

        // Wait for init to complete with timeout.
        let init_response = tokio::time::timeout(std::time::Duration::from_secs(INIT_TIMEOUT_SECS), init_rx)
            .await
            .map_err(|_| AcpError::InitTimeout {
                timeout_secs: INIT_TIMEOUT_SECS,
            })?
            .map_err(|_| AcpError::Disconnected {
                exit_code: None,
                signal: None,
                stderr: "Init channel dropped".into(),
            })??;

        // `ready_rx` should resolve almost immediately after init_tx fires.
        let connection = ready_rx.await.map_err(|_| AcpError::NotConnected)?;

        Ok(Self {
            connection,
            shutdown_tx: Some(shutdown_tx),
            alive,
            initialize_response: Arc::new(RwLock::new(Some(init_response))),
        })
    }

    pub fn initialize_response(&self) -> Option<InitializeResponse> {
        self.initialize_response.read().unwrap().clone()
    }

    pub fn agent_capabilities(&self) -> Option<AgentCapabilities> {
        self.initialize_response().map(|response| response.agent_capabilities)
    }

    pub fn auth_methods(&self) -> Option<Vec<AuthMethod>> {
        self.initialize_response().map(|response| response.auth_methods)
    }

    /// Create a new ACP session.
    pub async fn new_session(&self, req: NewSessionRequest) -> Result<NewSessionResponse, AcpError> {
        self.send_request(req, AGENT_METHOD_NAMES.session_new).await
    }

    /// Load (resume) an existing ACP session.
    pub async fn load_session(&self, req: LoadSessionRequest) -> Result<LoadSessionResponse, AcpError> {
        self.send_request(req, AGENT_METHOD_NAMES.session_load).await
    }

    /// Fork an existing ACP session into a new session.
    pub async fn fork_session(&self, req: ForkSessionRequest) -> Result<ForkSessionResponse, AcpError> {
        self.send_request(req, AGENT_METHOD_NAMES.session_fork).await
    }

    /// Resume an existing ACP session.
    pub async fn resume_session(&self, req: ResumeSessionRequest) -> Result<ResumeSessionResponse, AcpError> {
        self.send_request(req, AGENT_METHOD_NAMES.session_resume).await
    }

    /// Close an ACP session.
    pub async fn close_session(&self, req: CloseSessionRequest) -> Result<CloseSessionResponse, AcpError> {
        self.send_request(req, AGENT_METHOD_NAMES.session_close).await
    }

    /// Send a prompt to the agent in an active session.
    ///
    /// Blocks until the agent returns a `PromptResponse` (turn completed).
    /// Streaming events arrive via the `event_tx` broadcast channel.
    pub async fn prompt(&self, req: PromptRequest) -> Result<PromptResponse, AcpError> {
        self.send_request(req, AGENT_METHOD_NAMES.session_prompt).await
    }

    /// Cancel the current prompt in a session (fire-and-forget notification).
    pub fn cancel(&self, notification: CancelNotification) {
        if !self.is_connected() {
            return;
        }
        log_notify(AGENT_METHOD_NAMES.session_cancel, &json_str(&notification));
        let _ = self.connection.send_notification(notification);
    }

    /// Set the session mode.
    pub async fn set_mode(&self, req: SetSessionModeRequest) -> Result<SetSessionModeResponse, AcpError> {
        self.send_request(req, AGENT_METHOD_NAMES.session_set_mode).await
    }

    /// Set the session model.
    pub async fn set_model(&self, req: SetSessionModelRequest) -> Result<SetSessionModelResponse, AcpError> {
        self.send_request(req, AGENT_METHOD_NAMES.session_set_model).await
    }

    /// Set a session config option.
    pub async fn set_config_option(
        &self,
        req: SetSessionConfigOptionRequest,
    ) -> Result<SetSessionConfigOptionResponse, AcpError> {
        self.send_request(req, AGENT_METHOD_NAMES.session_set_config_option)
            .await
    }

    /// List sessions, optionally filtered by working directory.
    pub async fn list_sessions(&self, req: ListSessionsRequest) -> Result<ListSessionsResponse, AcpError> {
        self.send_request(req, AGENT_METHOD_NAMES.session_list).await
    }

    /// Authenticate with the agent using a previously advertised auth method.
    pub async fn authenticate(&self, req: AuthenticateRequest) -> Result<AuthenticateResponse, AcpError> {
        self.send_request(req, AGENT_METHOD_NAMES.authenticate).await
    }

    /// Send an extension request (method name must start with `_`).
    ///
    /// Returns the raw JSON response value from the agent.
    pub async fn ext_request(&self, req: ExtRequest) -> Result<ExtResponse, AcpError> {
        self.ensure_connected()?;
        let method = format!("_{}", req.method);
        let wrapped = ClientRequest::ExtMethodRequest(req);
        let value = self.send_request(wrapped, &method).await?;
        let raw = serde_json::value::to_raw_value(&value).map_err(|e| AcpError::AgentInternal {
            message: format!("Failed to convert ext response: {e}"),
            code: -32603,
        })?;
        Ok(ExtResponse::new(raw.into()))
    }

    /// Send an extension notification (fire-and-forget, method name must start with `_`).
    pub fn ext_notify(&self, notification: ExtNotification) {
        if !self.is_connected() {
            return;
        }
        let method = format!("_{}", notification.method);
        log_notify(&method, &json_str(&notification));
        let wrapped = ClientNotification::ExtNotification(notification);
        let _ = self.connection.send_notification(wrapped);
    }

    /// Check whether the SDK connection is still alive.
    pub fn is_connected(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    // ── Private helpers ──────────────────────────────────────────────────

    /// Shared request path: connectivity check, structured logging, SDK call.
    async fn send_request<Req>(&self, req: Req, method: &str) -> Result<Req::Response, AcpError>
    where
        Req: agent_client_protocol::JsonRpcRequest + serde::Serialize + std::fmt::Debug,
        Req::Response: serde::Serialize + std::fmt::Debug + Send,
    {
        self.ensure_connected()?;
        log_request(method, &json_str(&req));
        let rsp = self.connection.send_request(req).block_task().await;
        log_response(method, &json_or_err(&rsp));
        rsp.map_err(|e| AcpError::from_sdk(e, method))
    }

    /// Return `Err(NotConnected)` if the connection is dead.
    fn ensure_connected(&self) -> Result<(), AcpError> {
        if self.is_connected() {
            Ok(())
        } else {
            Err(AcpError::NotConnected)
        }
    }
}

impl Drop for AcpProtocol {
    fn drop(&mut self) {
        // Releasing the oneshot wakes `main_fn` in the background task, which
        // returns, which drives SDK shutdown. The bg_task joins naturally
        // (we don't await it here — Drop can't be async; the task is
        // `tokio::spawn`ed, so it gets cleaned up by the runtime).
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Run the SDK `connect_with` future: register notification/request
/// handlers, execute the initialize handshake, publish the connection
/// handle, then park on the shutdown signal until [`AcpProtocol`] is dropped.
#[allow(clippy::too_many_arguments)]
async fn run_sdk_background(
    stdin: ChildStdin,
    stdout: ChildStdout,
    event_tx: broadcast::Sender<AgentStreamEvent>,
    permission_tx: mpsc::Sender<PermissionRequest>,
    notification_tx: mpsc::Sender<SessionNotification>,
    init_tx: oneshot::Sender<Result<InitializeResponse, AcpError>>,
    ready_tx: oneshot::Sender<ConnectionTo<Agent>>,
    shutdown_rx: oneshot::Receiver<()>,
    alive: Arc<AtomicBool>,
) {
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());

    // `init_tx` / `ready_tx` are consumed inside the main_fn closure; wrap
    // them in Option so we can .take() without moving out of captured state.
    let mut init_tx = Some(init_tx);
    let mut ready_tx = Some(ready_tx);
    let mut shutdown_rx = Some(shutdown_rx);

    let result = Client
        .builder()
        .on_receive_notification(
            {
                let event_tx = event_tx.clone();
                let notification_tx = notification_tx.clone();
                async move |notification: SessionNotification, _cx: ConnectionTo<Agent>| {
                    // Fan out the raw SDK notification to the manager's apply-loop
                    // FIRST, so session state is consistent by the time the UI
                    // event hits the broadcast channel. Swallow send errors — if
                    // the manager has dropped the receiver, session consistency
                    // is moot anyway (we're on our way down).
                    let _ = notification_tx.send(notification.clone()).await;
                    handle_session_notification(notification, &event_tx).await;
                    Ok(())
                }
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            {
                async move |request: RequestPermissionRequest, responder, _cx| {
                    handle_permission_request(request, responder, &permission_tx).await;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .connect_with(transport, async move |connection: ConnectionTo<Agent>| {
            // Step 1 — initialize handshake. main_fn is the canonical place
            // to call `block_task` (see SDK `connect_with` doc example).
            let init_result = {
                let req = InitializeRequest::new(ProtocolVersion::LATEST);
                log_request("initialize", &json_str(&req));
                let raw = connection.send_request(req).block_task().await;
                log_response("initialize", &json_or_err(&raw));
                raw.map_err(|e| AcpError::from_sdk(e, "initialize"))
            };

            let Some(tx) = init_tx.take() else {
                return Ok(());
            };
            match init_result {
                Ok(resp) => {
                    let _ = tx.send(Ok(resp));
                }
                Err(err) => {
                    let _ = tx.send(Err(err));
                    // init failure: let main_fn return so SDK cleans up.
                    return Ok(());
                }
            }

            // Step 2 — publish the connection handle so the outer
            // AcpProtocol can start issuing requests.
            if let Some(tx) = ready_tx.take()
                && tx.send(connection).is_err()
            {
                // Owner dropped before we became ready — nothing more to do.
                return Ok(());
            }

            // Step 3 — keep the connection alive until AcpProtocol::drop
            // releases the shutdown oneshot.
            if let Some(rx) = shutdown_rx.take() {
                let _ = rx.await;
            }
            Ok(())
        })
        .await;

    alive.store(false, Ordering::Release);

    match result {
        Ok(_) => debug!("ACP SDK connection closed normally"),
        Err(e) => warn!(error = %e, "ACP SDK connection closed with error"),
    }
}

/// Fan out a CLI session notification to the event broadcast channel.
async fn handle_session_notification(
    notification: SessionNotification,
    event_tx: &broadcast::Sender<AgentStreamEvent>,
) {
    log_incoming("session/update", &json_str(&notification));

    let events = stream_event::session_notification_to_events(&notification);
    for event in events {
        let _ = event_tx.send(event);
    }
}

/// Relay a CLI permission request to the pending-permission channel and
/// forward the user's decision back to the SDK responder.
async fn handle_permission_request(
    request: RequestPermissionRequest,
    responder: Responder<RequestPermissionResponse>,
    event_tx: &mpsc::Sender<PermissionRequest>,
) {
    log_incoming("session/request_permission", &json_str(&request));

    let (response_tx, response_rx) = oneshot::channel();

    if event_tx.send(PermissionRequest { request, response_tx }).await.is_err() {
        warn!("Permission channel closed, cancelling request");
        let _ = responder.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled));
        return;
    }

    let response = match response_rx.await {
        Ok(PermissionDecision::Selected { option_id }) => RequestPermissionResponse::new(
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
        ),
        Ok(PermissionDecision::Cancelled) | Err(_) => {
            RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled)
        }
    };

    log_outgoing("session/request_permission", &json_str(&response));
    let _ = responder.respond(response);
}

/// Serialize a value to a compact JSON string, falling back to Debug on failure.
fn json_str(value: &(impl serde::Serialize + std::fmt::Debug)) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}"))
}

/// Serialize the Ok side of a Result to JSON, or format the Err with Debug.
fn json_or_err<T: serde::Serialize + std::fmt::Debug, E: std::fmt::Debug>(result: &Result<T, E>) -> String {
    match result {
        Ok(v) => json_str(v),
        Err(e) => format!("{e:?}"),
    }
}

/// Log an outgoing ACP request (`→`).
fn log_request(method: &str, body: &str) {
    debug!("[ACP] {method} ->\n -> {body}");
}

/// Log an incoming ACP response (`←`).
fn log_response(method: &str, body: &str) {
    debug!("[ACP] {method} <-\n <- {body}");
}

/// Log a fire-and-forget notification (`⚡`).
fn log_notify(method: &str, body: &str) {
    debug!("[ACP] {method} ⚡⚡\n ⚡⚡ {body}");
}

/// Log an incoming agent notification/request (`←`).
fn log_incoming(method: &str, body: &str) {
    debug!("[ACP] {method} <-\n <- {body}");
}

/// Log an outgoing agent notification/request (`→`).
fn log_outgoing(method: &str, body: &str) {
    debug!("[ACP] {method} ->\n -> {body}");
}

impl std::fmt::Debug for AcpProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpProtocol")
            .field("alive", &self.is_connected())
            .finish_non_exhaustive()
    }
}
