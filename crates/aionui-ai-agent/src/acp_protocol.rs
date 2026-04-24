//! ACP protocol layer: SDK integration for JSON-RPC communication.
//!
//! This module owns the `agent-client-protocol` SDK connection. It provides
//! typed async methods for all ACP operations and routes incoming agent
//! notifications/requests to the appropriate channels.
//!
//! All requests are dispatched through a command channel to the SDK event loop
//! running inside `connect_with`. This is required because `block_task()` only
//! works within the `connect_with` closure's execution context.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol::schema::{
    ClientNotification, ClientRequest, InitializeRequest, ProtocolVersion,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification,
};
use agent_client_protocol::{
    Agent, ByteStreams, Client, ConnectionTo, on_receive_notification, on_receive_request,
};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::{debug, warn};

use crate::acp_error::AcpError;
use crate::stream_event::{self, AgentStreamEvent};

// Re-export SDK types used in public method signatures.
pub use agent_client_protocol::schema::{
    AuthenticateRequest, CancelNotification, CloseSessionRequest, ContentBlock, ExtNotification,
    ExtRequest, ForkSessionRequest, ListSessionsRequest, ListSessionsResponse, LoadSessionRequest,
    McpServer, Meta, NewSessionRequest, PromptRequest, ResumeSessionRequest, SessionId,
    SetSessionConfigOptionRequest, SetSessionModeRequest, SetSessionModelRequest,
};

/// Type alias to shorten `agent_client_protocol::Responder<RequestPermissionResponse>`.
type PermissionResponder = agent_client_protocol::Responder<RequestPermissionResponse>;

/// Timeout for the ACP initialize handshake (seconds).
const INIT_TIMEOUT_SECS: u64 = 30;

/// A pending permission request from the agent, awaiting user decision.
pub struct PermissionRequest {
    /// ACP session ID.
    pub session_id: String,
    /// Tool call details from the agent.
    pub tool_call: serde_json::Value,
    /// Available permission options (allow once, always, reject, etc.).
    pub options: serde_json::Value,
    /// Optional metadata from the agent.
    pub meta: Option<serde_json::Value>,
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

// ── Internal command protocol ────────────────────────────────────────────

/// Commands sent from `AcpProtocol` methods to the SDK event loop.
enum AcpCommand {
    NewSession {
        req: NewSessionRequest,
        reply: oneshot::Sender<Result<SessionId, AcpError>>,
    },
    LoadSession {
        req: LoadSessionRequest,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    ForkSession {
        req: ForkSessionRequest,
        reply: oneshot::Sender<Result<SessionId, AcpError>>,
    },
    ResumeSession {
        req: ResumeSessionRequest,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    CloseSession {
        req: CloseSessionRequest,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    Prompt {
        req: PromptRequest,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    Cancel {
        notification: CancelNotification,
    },
    SetMode {
        req: SetSessionModeRequest,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    SetModel {
        req: SetSessionModelRequest,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    SetConfigOption {
        req: SetSessionConfigOptionRequest,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    ListSessions {
        req: ListSessionsRequest,
        reply: oneshot::Sender<Result<ListSessionsResponse, AcpError>>,
    },
    Authenticate {
        req: AuthenticateRequest,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    ExtMethod {
        req: ExtRequest,
        reply: oneshot::Sender<Result<serde_json::Value, AcpError>>,
    },
    ExtNotify {
        notification: ExtNotification,
    },
}

/// ACP protocol handle: wraps the SDK connection and provides typed operations.
///
/// All methods send commands to the SDK event loop via a channel. The event
/// loop runs inside `connect_with` where `block_task()` is safe to use.
pub struct AcpProtocol {
    /// Background task handle (SDK transport + routing).
    _bg_task: JoinHandle<()>,
    /// Command sender to the SDK event loop.
    cmd_tx: mpsc::Sender<AcpCommand>,
    /// Whether the SDK connection is still alive.
    alive: Arc<AtomicBool>,
}

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
    ) -> Result<Self, AcpError> {
        let alive = Arc::new(AtomicBool::new(true));
        let alive_clone = Arc::clone(&alive);

        // Command channel: external methods → SDK event loop
        let (cmd_tx, cmd_rx) = mpsc::channel::<AcpCommand>(32);

        // Signal that init completed successfully
        let (init_tx, init_rx) = oneshot::channel::<Result<(), AcpError>>();

        let _bg_task = tokio::spawn(run_sdk_event_loop(
            stdin,
            stdout,
            event_tx,
            permission_tx,
            cmd_rx,
            init_tx,
            alive_clone,
        ));

        // Wait for init to complete with timeout
        let init_result =
            tokio::time::timeout(std::time::Duration::from_secs(INIT_TIMEOUT_SECS), init_rx)
                .await
                .map_err(|_| AcpError::InitTimeout {
                    timeout_secs: INIT_TIMEOUT_SECS,
                })?
                .map_err(|_| AcpError::Disconnected {
                    exit_code: None,
                    signal: None,
                    stderr: "Init channel dropped".into(),
                })?;

        init_result?;

        Ok(Self {
            cmd_tx,
            _bg_task,
            alive,
        })
    }

    /// Create a new ACP session.
    pub async fn new_session(&self, req: NewSessionRequest) -> Result<SessionId, AcpError> {
        self.send_cmd(|reply| AcpCommand::NewSession { req, reply })
            .await
    }

    /// Load (resume) an existing ACP session.
    pub async fn load_session(&self, req: LoadSessionRequest) -> Result<(), AcpError> {
        self.send_cmd(|reply| AcpCommand::LoadSession { req, reply })
            .await
    }

    /// Fork an existing ACP session into a new session.
    pub async fn fork_session(&self, req: ForkSessionRequest) -> Result<SessionId, AcpError> {
        self.send_cmd(|reply| AcpCommand::ForkSession { req, reply })
            .await
    }

    /// Resume an existing ACP session.
    pub async fn resume_session(&self, req: ResumeSessionRequest) -> Result<(), AcpError> {
        self.send_cmd(|reply| AcpCommand::ResumeSession { req, reply })
            .await
    }

    /// Close an ACP session.
    pub async fn close_session(&self, req: CloseSessionRequest) -> Result<(), AcpError> {
        self.send_cmd(|reply| AcpCommand::CloseSession { req, reply })
            .await
    }

    /// Send a prompt to the agent in an active session.
    ///
    /// Blocks until the agent returns a `PromptResponse` (turn completed).
    /// Streaming events arrive via the `event_tx` broadcast channel.
    pub async fn prompt(&self, req: PromptRequest) -> Result<(), AcpError> {
        self.send_cmd(|reply| AcpCommand::Prompt { req, reply })
            .await
    }

    /// Cancel the current prompt in a session (fire-and-forget notification).
    pub fn cancel(&self, notification: CancelNotification) {
        if !self.is_connected() {
            return;
        }
        let _ = self.cmd_tx.try_send(AcpCommand::Cancel { notification });
    }

    /// Set the session mode.
    pub async fn set_mode(&self, req: SetSessionModeRequest) -> Result<(), AcpError> {
        self.send_cmd(|reply| AcpCommand::SetMode { req, reply })
            .await
    }

    /// Set the session model.
    pub async fn set_model(&self, req: SetSessionModelRequest) -> Result<(), AcpError> {
        self.send_cmd(|reply| AcpCommand::SetModel { req, reply })
            .await
    }

    /// Set a session config option.
    pub async fn set_config_option(
        &self,
        req: SetSessionConfigOptionRequest,
    ) -> Result<(), AcpError> {
        self.send_cmd(|reply| AcpCommand::SetConfigOption { req, reply })
            .await
    }

    /// List sessions, optionally filtered by working directory.
    pub async fn list_sessions(
        &self,
        req: ListSessionsRequest,
    ) -> Result<ListSessionsResponse, AcpError> {
        self.send_cmd(|reply| AcpCommand::ListSessions { req, reply })
            .await
    }

    /// Authenticate with the agent using a previously advertised auth method.
    pub async fn authenticate(&self, req: AuthenticateRequest) -> Result<(), AcpError> {
        self.send_cmd(|reply| AcpCommand::Authenticate { req, reply })
            .await
    }

    /// Send an extension request (method name must start with `_`).
    ///
    /// Returns the raw JSON response value from the agent.
    pub async fn ext_request(&self, req: ExtRequest) -> Result<serde_json::Value, AcpError> {
        self.send_cmd(|reply| AcpCommand::ExtMethod { req, reply })
            .await
    }

    /// Send an extension notification (fire-and-forget, method name must start with `_`).
    pub fn ext_notify(&self, notification: ExtNotification) {
        if !self.is_connected() {
            return;
        }
        let _ = self.cmd_tx.try_send(AcpCommand::ExtNotify { notification });
    }

    /// Check whether the SDK connection is still alive.
    pub fn is_connected(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    // ── Private helpers ──────────────────────────────────────────────────

    /// Send a command to the SDK event loop and wait for the reply.
    async fn send_cmd<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, AcpError>>) -> AcpCommand,
    ) -> Result<T, AcpError> {
        self.ensure_connected()?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(build(tx))
            .await
            .map_err(|_| AcpError::NotConnected)?;
        rx.await.map_err(|_| AcpError::NotConnected)?
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

/// Execute the ACP initialize handshake over the given connection.
///
/// Sends `InitializeRequest` and signals success/failure via `init_tx`.
/// Returns `Err(())` when the handshake failed (the caller should bail out).
async fn execute_initialize(
    connection: &ConnectionTo<Agent>,
    init_tx: oneshot::Sender<Result<(), AcpError>>,
) -> Result<(), ()> {
    let req = InitializeRequest::new(ProtocolVersion::LATEST);
    log_request("initialize", &json_str(&req));

    let raw = connection.send_request(req).block_task().await;
    log_response("initialize", &json_or_err(&raw));

    if let Err(e) = raw {
        let _ = init_tx.send(Err(AcpError::from_sdk(e, "initialize")));
        return Err(());
    }
    let _ = init_tx.send(Ok(()));
    Ok(())
}

/// Run the SDK event loop: build the client, connect transport, handle
/// notifications/requests, execute the initialize handshake, then process
/// commands until the channel closes.
///
/// This is the top-level future spawned as the background task in
/// [`AcpProtocol::connect`].
async fn run_sdk_event_loop(
    stdin: ChildStdin,
    stdout: ChildStdout,
    event_tx: broadcast::Sender<AgentStreamEvent>,
    permission_tx: mpsc::Sender<PermissionRequest>,
    cmd_rx: mpsc::Receiver<AcpCommand>,
    init_tx: oneshot::Sender<Result<(), AcpError>>,
    alive: Arc<AtomicBool>,
) {
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());

    let result = Client
        .builder()
        .on_receive_notification(
            {
                let event_tx = event_tx.clone();
                async move |notification: SessionNotification, _cx: ConnectionTo<Agent>| {
                    log_incoming("session/update", &json_str(&notification));

                    let events = stream_event::session_notification_to_events(&notification);
                    for event in events {
                        let _ = event_tx.send(event);
                    }
                    Ok(())
                }
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            {
                let permission_tx = permission_tx.clone();
                async move |request: RequestPermissionRequest,
                            responder: PermissionResponder,
                            _cx: ConnectionTo<Agent>| {
                    let session_id = request.session_id.to_string();
                    log_incoming("session/request_permission", &json_str(&request));
                    let tool_call = serde_json::to_value(&request.tool_call).unwrap_or_default();
                    let options = serde_json::to_value(&request.options).unwrap_or_default();
                    let meta = request
                        .meta
                        .as_ref()
                        .and_then(|m| serde_json::to_value(m).ok());

                    let (resp_tx, resp_rx) = oneshot::channel();

                    let perm_req = PermissionRequest {
                        session_id,
                        tool_call,
                        options,
                        meta,
                        response_tx: resp_tx,
                    };

                    if permission_tx.send(perm_req).await.is_err() {
                        warn!("Permission channel closed, cancelling request");
                        return responder.respond(RequestPermissionResponse::new(
                            RequestPermissionOutcome::Cancelled,
                        ));
                    }

                    match resp_rx.await {
                        Ok(PermissionDecision::Selected { option_id }) => responder.respond(
                            RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                                SelectedPermissionOutcome::new(option_id),
                            )),
                        ),
                        Ok(PermissionDecision::Cancelled) | Err(_) => responder.respond(
                            RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
                        ),
                    }
                }
            },
            on_receive_request!(),
        )
        .connect_with(transport, {
            let mut cmd_rx = cmd_rx;
            move |connection: ConnectionTo<Agent>| async move {
                if let Err(()) = execute_initialize(&connection, init_tx).await {
                    return Ok(());
                }

                // Command loop: process requests from AcpProtocol methods
                while let Some(cmd) = cmd_rx.recv().await {
                    dispatch_command(&connection, cmd).await;
                }

                debug!("ACP command channel closed, connection ending");
                Ok(())
            }
        })
        .await;

    // Mark connection as dead
    alive.store(false, Ordering::Release);

    match result {
        Ok(_) => debug!("ACP SDK connection closed normally"),
        Err(e) => warn!(error = %e, "ACP SDK connection closed with error"),
    }
}

/// Dispatch a single [`AcpCommand`] over the connection.
async fn dispatch_command(connection: &ConnectionTo<Agent>, cmd: AcpCommand) {
    match cmd {
        AcpCommand::NewSession { req, reply } => {
            let _ = reply.send(
                send_and_log(connection, req, "session/new")
                    .await
                    .map(|r| r.session_id),
            );
        }
        AcpCommand::ForkSession { req, reply } => {
            let _ = reply.send(
                send_and_log(connection, req, "session/fork")
                    .await
                    .map(|r| r.session_id),
            );
        }
        AcpCommand::LoadSession { req, reply } => {
            let _ = reply.send(
                send_and_log(connection, req, "session/load")
                    .await
                    .map(|_| ()),
            );
        }
        AcpCommand::ResumeSession { req, reply } => {
            let _ = reply.send(
                send_and_log(connection, req, "session/resume")
                    .await
                    .map(|_| ()),
            );
        }
        AcpCommand::CloseSession { req, reply } => {
            let _ = reply.send(
                send_and_log(connection, req, "session/close")
                    .await
                    .map(|_| ()),
            );
        }
        AcpCommand::Prompt { req, reply } => {
            let _ = reply.send(
                send_and_log(connection, req, "session/prompt")
                    .await
                    .map(|_| ()),
            );
        }
        AcpCommand::SetMode { req, reply } => {
            let _ = reply.send(
                send_and_log(connection, req, "session/set_mode")
                    .await
                    .map(|_| ()),
            );
        }
        AcpCommand::SetModel { req, reply } => {
            let _ = reply.send(
                send_and_log(connection, req, "session/set_model")
                    .await
                    .map(|_| ()),
            );
        }
        AcpCommand::SetConfigOption { req, reply } => {
            let _ = reply.send(
                send_and_log(connection, req, "session/set_config_option")
                    .await
                    .map(|_| ()),
            );
        }
        AcpCommand::Cancel { notification } => {
            log_notify("session/cancel", &json_str(&notification));
            let _ = connection.send_notification(notification);
        }
        AcpCommand::ListSessions { req, reply } => {
            let _ = reply.send(send_and_log(connection, req, "session/list").await);
        }
        AcpCommand::Authenticate { req, reply } => {
            let _ = reply.send(
                send_and_log(connection, req, "authenticate")
                    .await
                    .map(|_| ()),
            );
        }
        AcpCommand::ExtMethod { req, reply } => {
            let method = format!("_{}", req.method);
            let wrapped = ClientRequest::ExtMethodRequest(req);
            let _ = reply.send(send_and_log(connection, wrapped, &method).await);
        }
        AcpCommand::ExtNotify { notification } => {
            let method = format!("_{}", notification.method);
            log_notify(&method, &json_str(&notification));
            let wrapped = ClientNotification::ExtNotification(notification);
            let _ = connection.send_notification(wrapped);
        }
    }
}

/// Send an SDK request over the connection, logging the request and response JSON.
///
/// The `method` string is used for log labels and as error context for
/// [`AcpError::from_sdk`].
async fn send_and_log<Req>(
    connection: &ConnectionTo<Agent>,
    req: Req,
    method: &str,
) -> Result<Req::Response, AcpError>
where
    Req: agent_client_protocol::JsonRpcRequest + serde::Serialize + std::fmt::Debug,
    Req::Response: serde::Serialize + std::fmt::Debug,
{
    log_request(method, &json_str(&req));
    let raw = connection.send_request(req).block_task().await;
    log_response(method, &json_or_err(&raw));
    raw.map_err(|e| AcpError::from_sdk(e, method))
}

/// Serialize a value to a compact JSON string, falling back to Debug on failure.
fn json_str(value: &(impl serde::Serialize + std::fmt::Debug)) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}"))
}

/// Serialize the Ok side of a Result to JSON, or format the Err with Debug.
fn json_or_err<T: serde::Serialize + std::fmt::Debug, E: std::fmt::Debug>(
    result: &Result<T, E>,
) -> String {
    match result {
        Ok(v) => json_str(v),
        Err(e) => format!("{e:?}"),
    }
}

/// Log an outgoing ACP request (`→`).
fn log_request(method: &str, body: &str) {
    debug!("[ACP] {method}\n → {body}");
}

/// Log an incoming ACP response (`←`).
fn log_response(method: &str, body: &str) {
    debug!("[ACP] {method}\n ← {body}");
}

/// Log a fire-and-forget notification (`⚡`).
fn log_notify(method: &str, body: &str) {
    debug!("[ACP] {method}\n ⚡ {body}");
}

/// Log an incoming agent notification/request (`←`).
fn log_incoming(method: &str, body: &str) {
    debug!("[ACP] {method}\n ← {body}");
}

impl std::fmt::Debug for AcpProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpProtocol")
            .field("alive", &self.is_connected())
            .finish_non_exhaustive()
    }
}
