//! ACP protocol layer: SDK integration for JSON-RPC communication.
//!
//! This module owns the `agent-client-protocol` SDK connection. It provides
//! typed async methods for all ACP operations and routes incoming agent
//! notifications/requests to the appropriate channels.
//!
//! All requests are dispatched through a command channel to the SDK event loop
//! running inside `connect_with`. This is required because `block_task()` only
//! works within the `connect_with` closure's execution context.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use agent_client_protocol::schema::{
    AGENT_METHOD_NAMES, AuthenticateResponse, ClientNotification, ClientRequest, CloseSessionResponse, ExtResponse,
    ForkSessionResponse, InitializeRequest, LoadSessionResponse, PromptResponse, ProtocolVersion,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse, ResumeSessionResponse,
    SelectedPermissionOutcome, SessionNotification, SetSessionConfigOptionResponse, SetSessionModeResponse,
    SetSessionModelResponse,
};
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo, on_receive_notification, on_receive_request};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::{debug, warn};

use crate::acp_error::AcpError;
use crate::stream_event::{self, AgentStreamEvent};
use crate::types::AgentStreamChunk;

use agent_client_protocol::schema::{
    AgentCapabilities, AuthMethod, AuthenticateRequest, CancelNotification, CloseSessionRequest, ExtNotification,
    ExtRequest, ForkSessionRequest, InitializeResponse, ListSessionsRequest, ListSessionsResponse, LoadSessionRequest,
    NewSessionRequest, NewSessionResponse, PromptRequest, ResumeSessionRequest, SetSessionConfigOptionRequest,
    SetSessionModeRequest, SetSessionModelRequest,
};

type PermissionResponder = agent_client_protocol::Responder<RequestPermissionResponse>;

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

/// Commands constructed by SDK callbacks and dispatched to handle agent→client messages.
///
/// Mirrors [`AcpClientCommand`] for the reverse direction. Each callback
/// builds the appropriate variant and calls [`dispatch_agent_command`].
enum AcpAgentCommand {
    /// Agent sent a session update notification (streaming chunks, tool calls, etc.).
    SessionUpdate {
        notification: SessionNotification,
        event_tx: broadcast::Sender<AgentStreamEvent>,
        stream_tx: broadcast::Sender<AgentStreamChunk>,
    },
    /// Agent requests permission from the user before performing an action.
    RequestPermission {
        request: RequestPermissionRequest,
        responder: PermissionResponder,
        event_tx: mpsc::Sender<PermissionRequest>,
    },
}

// ── Internal command protocol ────────────────────────────────────────────

/// Commands sent from `AcpProtocol` methods to the SDK event loop.
enum AcpClientCommand {
    NewSession {
        req: NewSessionRequest,
        event_tx: oneshot::Sender<Result<NewSessionResponse, AcpError>>,
    },
    LoadSession {
        req: LoadSessionRequest,
        event_tx: oneshot::Sender<Result<LoadSessionResponse, AcpError>>,
    },
    ForkSession {
        req: ForkSessionRequest,
        event_tx: oneshot::Sender<Result<ForkSessionResponse, AcpError>>,
    },
    ResumeSession {
        req: ResumeSessionRequest,
        event_tx: oneshot::Sender<Result<ResumeSessionResponse, AcpError>>,
    },
    CloseSession {
        req: CloseSessionRequest,
        event_tx: oneshot::Sender<Result<CloseSessionResponse, AcpError>>,
    },
    Prompt {
        req: PromptRequest,
        event_tx: oneshot::Sender<Result<PromptResponse, AcpError>>,
    },
    Cancel {
        notification: CancelNotification,
    },
    SetMode {
        req: SetSessionModeRequest,
        event_tx: oneshot::Sender<Result<SetSessionModeResponse, AcpError>>,
    },
    SetModel {
        req: SetSessionModelRequest,
        event_tx: oneshot::Sender<Result<SetSessionModelResponse, AcpError>>,
    },
    SetConfigOption {
        req: SetSessionConfigOptionRequest,
        event_tx: oneshot::Sender<Result<SetSessionConfigOptionResponse, AcpError>>,
    },
    ListSessions {
        req: ListSessionsRequest,
        event_tx: oneshot::Sender<Result<ListSessionsResponse, AcpError>>,
    },
    Authenticate {
        req: AuthenticateRequest,
        event_tx: oneshot::Sender<Result<AuthenticateResponse, AcpError>>,
    },
    ExtMethod {
        req: ExtRequest,
        event_tx: oneshot::Sender<Result<ExtResponse, AcpError>>,
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
    cmd_tx: mpsc::Sender<AcpClientCommand>,
    /// Whether the SDK connection is still alive.
    alive: Arc<AtomicBool>,
    /// Cached initialize response from the ACP handshake.
    initialize_response: Arc<RwLock<Option<InitializeResponse>>>,
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
        stream_tx: broadcast::Sender<AgentStreamChunk>,
        permission_tx: mpsc::Sender<PermissionRequest>,
    ) -> Result<Self, AcpError> {
        let alive = Arc::new(AtomicBool::new(true));
        let alive_clone = Arc::clone(&alive);
        let initialize_response = Arc::new(RwLock::new(None));

        // Command channel: external methods → SDK event loop
        let (clicmd_tx, clicmd_rx) = mpsc::channel::<AcpClientCommand>(32);

        // Signal that init completed successfully
        let (init_tx, init_rx) = oneshot::channel::<Result<InitializeResponse, AcpError>>();

        let _bg_task = tokio::spawn(run_sdk_event_loop(
            stdin,
            stdout,
            event_tx,
            stream_tx,
            permission_tx,
            init_tx,
            clicmd_rx,
            alive_clone,
        ));

        // Wait for init to complete with timeout
        let init_result = tokio::time::timeout(std::time::Duration::from_secs(INIT_TIMEOUT_SECS), init_rx)
            .await
            .map_err(|_| AcpError::InitTimeout {
                timeout_secs: INIT_TIMEOUT_SECS,
            })?
            .map_err(|_| AcpError::Disconnected {
                exit_code: None,
                signal: None,
                stderr: "Init channel dropped".into(),
            })?;

        let init_response = init_result?;
        *initialize_response.write().unwrap() = Some(init_response);

        Ok(Self {
            cmd_tx: clicmd_tx,
            _bg_task,
            alive,
            initialize_response,
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
        self.send_cmd(|event_tx| AcpClientCommand::NewSession { req, event_tx })
            .await
    }

    /// Load (resume) an existing ACP session.
    pub async fn load_session(&self, req: LoadSessionRequest) -> Result<LoadSessionResponse, AcpError> {
        self.send_cmd(|event_tx| AcpClientCommand::LoadSession { req, event_tx })
            .await
    }

    /// Fork an existing ACP session into a new session.
    pub async fn fork_session(&self, req: ForkSessionRequest) -> Result<ForkSessionResponse, AcpError> {
        self.send_cmd(|event_tx| AcpClientCommand::ForkSession { req, event_tx })
            .await
    }

    /// Resume an existing ACP session.
    pub async fn resume_session(&self, req: ResumeSessionRequest) -> Result<ResumeSessionResponse, AcpError> {
        self.send_cmd(|event_tx| AcpClientCommand::ResumeSession { req, event_tx })
            .await
    }

    /// Close an ACP session.
    pub async fn close_session(&self, req: CloseSessionRequest) -> Result<CloseSessionResponse, AcpError> {
        self.send_cmd(|event_tx| AcpClientCommand::CloseSession { req, event_tx })
            .await
    }

    /// Send a prompt to the agent in an active session.
    ///
    /// Blocks until the agent returns a `PromptResponse` (turn completed).
    /// Streaming events arrive via the `event_tx` broadcast channel.
    pub async fn prompt(&self, req: PromptRequest) -> Result<PromptResponse, AcpError> {
        self.send_cmd(|event_tx| AcpClientCommand::Prompt { req, event_tx })
            .await
    }

    /// Cancel the current prompt in a session (fire-and-forget notification).
    pub fn cancel(&self, notification: CancelNotification) {
        if !self.is_connected() {
            return;
        }
        let _ = self.cmd_tx.try_send(AcpClientCommand::Cancel { notification });
    }

    /// Set the session mode.
    pub async fn set_mode(&self, req: SetSessionModeRequest) -> Result<SetSessionModeResponse, AcpError> {
        self.send_cmd(|event_tx| AcpClientCommand::SetMode { req, event_tx })
            .await
    }

    /// Set the session model.
    pub async fn set_model(&self, req: SetSessionModelRequest) -> Result<SetSessionModelResponse, AcpError> {
        self.send_cmd(|event_tx| AcpClientCommand::SetModel { req, event_tx })
            .await
    }

    /// Set a session config option.
    pub async fn set_config_option(
        &self,
        req: SetSessionConfigOptionRequest,
    ) -> Result<SetSessionConfigOptionResponse, AcpError> {
        self.send_cmd(|event_tx| AcpClientCommand::SetConfigOption { req, event_tx })
            .await
    }

    /// List sessions, optionally filtered by working directory.
    pub async fn list_sessions(&self, req: ListSessionsRequest) -> Result<ListSessionsResponse, AcpError> {
        self.send_cmd(|event_tx| AcpClientCommand::ListSessions { req, event_tx })
            .await
    }

    /// Authenticate with the agent using a previously advertised auth method.
    pub async fn authenticate(&self, req: AuthenticateRequest) -> Result<AuthenticateResponse, AcpError> {
        self.send_cmd(|event_tx| AcpClientCommand::Authenticate { req, event_tx })
            .await
    }

    /// Send an extension request (method name must start with `_`).
    ///
    /// Returns the raw JSON response value from the agent.
    pub async fn ext_request(&self, req: ExtRequest) -> Result<ExtResponse, AcpError> {
        self.send_cmd(|event_tx| AcpClientCommand::ExtMethod { req, event_tx })
            .await
    }

    /// Send an extension notification (fire-and-forget, method name must start with `_`).
    pub fn ext_notify(&self, notification: ExtNotification) {
        if !self.is_connected() {
            return;
        }
        let _ = self.cmd_tx.try_send(AcpClientCommand::ExtNotify { notification });
    }

    /// Check whether the SDK connection is still alive.
    pub fn is_connected(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    // ── Private helpers ──────────────────────────────────────────────────

    /// Send a command to the SDK event loop and wait for the reply.
    async fn send_cmd<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, AcpError>>) -> AcpClientCommand,
    ) -> Result<T, AcpError> {
        self.ensure_connected()?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(build(tx)).await.map_err(|_| AcpError::NotConnected)?;
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
    init_tx: oneshot::Sender<Result<InitializeResponse, AcpError>>,
) -> Result<(), ()> {
    let req = InitializeRequest::new(ProtocolVersion::LATEST);
    log_request("initialize", &json_str(&req));

    let raw = connection.send_request(req).block_task().await;
    log_response("initialize", &json_or_err(&raw));

    if let Err(e) = raw {
        let _ = init_tx.send(Err(AcpError::from_sdk(e, "initialize")));
        return Err(());
    }
    let _ = init_tx.send(raw.map_err(|e| AcpError::from_sdk(e, "initialize")));
    Ok(())
}

/// Run the SDK event loop: build the client, connect transport, handle
/// notifications/requests, execute the initialize handshake, then process
/// commands until the channel closes.
///
/// This is the top-level future spawned as the background task in
/// [`AcpProtocol::connect`].
#[allow(clippy::too_many_arguments)]
async fn run_sdk_event_loop(
    stdin: ChildStdin,
    stdout: ChildStdout,
    event_tx: broadcast::Sender<AgentStreamEvent>,
    stream_tx: broadcast::Sender<AgentStreamChunk>,
    permission_tx: mpsc::Sender<PermissionRequest>,
    init_tx: oneshot::Sender<Result<InitializeResponse, AcpError>>,
    clicmd_rx: mpsc::Receiver<AcpClientCommand>,
    alive: Arc<AtomicBool>,
) {
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());

    let result = Client
        .builder()
        .on_receive_notification(
            {
                async move |notification: SessionNotification, _cx: ConnectionTo<Agent>| {
                    let cmd = AcpAgentCommand::SessionUpdate {
                        notification,
                        event_tx: event_tx.clone(),
                        stream_tx: stream_tx.clone(),
                    };
                    dispatch_agent_command(cmd).await;
                    Ok(())
                }
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            {
                async move |request: RequestPermissionRequest,
                            responder: PermissionResponder,
                            _cx: ConnectionTo<Agent>| {
                    let cmd = AcpAgentCommand::RequestPermission {
                        request,
                        responder,
                        event_tx: permission_tx.clone(),
                    };
                    dispatch_agent_command(cmd).await;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .connect_with(transport, {
            let mut cmd_rx = clicmd_rx;
            move |connection: ConnectionTo<Agent>| async move {
                if let Err(()) = execute_initialize(&connection, init_tx).await {
                    return Ok(());
                }

                // Command loop: process requests from AcpProtocol methods
                while let Some(cmd) = cmd_rx.recv().await {
                    dispatch_client_command(&connection, cmd).await;
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

/// Dispatch a single [`AcpClientCommand`] over the connection.
///
/// Mirrored by [`dispatch_agent_command`] for the reverse direction.
async fn dispatch_client_command(connection: &ConnectionTo<Agent>, cmd: AcpClientCommand) {
    match cmd {
        AcpClientCommand::NewSession { req, event_tx } => {
            let _ = event_tx.send(send_and_log(connection, req, AGENT_METHOD_NAMES.session_new).await);
        }
        AcpClientCommand::ForkSession { req, event_tx } => {
            let _ = event_tx.send(send_and_log(connection, req, AGENT_METHOD_NAMES.session_fork).await);
        }
        AcpClientCommand::LoadSession { req, event_tx } => {
            let _ = event_tx.send(send_and_log(connection, req, AGENT_METHOD_NAMES.session_load).await);
        }
        AcpClientCommand::ResumeSession { req, event_tx } => {
            let _ = event_tx.send(send_and_log(connection, req, AGENT_METHOD_NAMES.session_resume).await);
        }
        AcpClientCommand::CloseSession { req, event_tx } => {
            let _ = event_tx.send(send_and_log(connection, req, AGENT_METHOD_NAMES.session_close).await);
        }
        AcpClientCommand::Prompt { req, event_tx } => {
            // Run the prompt on a dedicated SDK task so the dispatch loop can
            // keep processing commands (most importantly `Cancel`) while the
            // CLI is still streaming the turn. Awaiting `send_and_log` here
            // directly would block every other command until the turn ends,
            // which is exactly what made `stop` not interrupt live turns —
            // the `session/cancel` notification was only dispatched after
            // the prompt response arrived.
            let connection_for_task = connection.clone();
            if let Err(err) = connection.spawn(async move {
                let result = send_and_log(&connection_for_task, req, AGENT_METHOD_NAMES.session_prompt).await;
                let _ = event_tx.send(result);
                Ok(())
            }) {
                // Spawn can only fail when the SDK is shutting down. In that
                // case `event_tx` was consumed by the closure, so the caller
                // will observe `AcpError::NotConnected` via the dropped
                // oneshot — which is the correct signal.
                warn!(error = %err, "Failed to spawn ACP prompt task");
            }
        }
        AcpClientCommand::SetMode { req, event_tx } => {
            let _ = event_tx.send(send_and_log(connection, req, AGENT_METHOD_NAMES.session_set_mode).await);
        }
        AcpClientCommand::SetModel { req, event_tx } => {
            let _ = event_tx.send(send_and_log(connection, req, AGENT_METHOD_NAMES.session_set_model).await);
        }
        AcpClientCommand::SetConfigOption { req, event_tx } => {
            let _ = event_tx.send(send_and_log(connection, req, AGENT_METHOD_NAMES.session_set_config_option).await);
        }
        AcpClientCommand::Cancel { notification } => {
            log_notify(AGENT_METHOD_NAMES.session_cancel, &json_str(&notification));
            let _ = connection.send_notification(notification);
        }
        AcpClientCommand::ListSessions { req, event_tx } => {
            let _ = event_tx.send(send_and_log(connection, req, AGENT_METHOD_NAMES.session_list).await);
        }
        AcpClientCommand::Authenticate { req, event_tx } => {
            let _ = event_tx.send(send_and_log(connection, req, AGENT_METHOD_NAMES.authenticate).await);
        }
        AcpClientCommand::ExtMethod { req, event_tx } => {
            let method = format!("_{}", req.method);
            let wrapped = ClientRequest::ExtMethodRequest(req);
            let result = send_and_log(connection, wrapped, &method).await;
            let _ = event_tx.send(result.and_then(|v| {
                let raw = serde_json::value::to_raw_value(&v).map_err(|e| AcpError::AgentInternal {
                    message: format!("Failed to convert ext response: {e}"),
                    code: -32603,
                })?;
                Ok(ExtResponse::new(raw.into()))
            }));
        }
        AcpClientCommand::ExtNotify { notification } => {
            let method = format!("_{}", notification.method);
            log_notify(&method, &json_str(&notification));
            let wrapped = ClientNotification::ExtNotification(notification);
            let _ = connection.send_notification(wrapped);
        }
    }
}

/// Dispatch a single [`AcpAgentCommand`] (agent → client direction).
///
/// Mirrors [`dispatch_client_command`] for the client → agent direction.
async fn dispatch_agent_command(cmd: AcpAgentCommand) {
    match cmd {
        AcpAgentCommand::SessionUpdate {
            notification,
            event_tx,
            stream_tx,
        } => {
            log_incoming("session/update", &json_str(&notification));

            let events = stream_event::session_notification_to_events(&notification);
            for event in events {
                let _ = event_tx.send(event);
            }

            // Parallel projection onto the AgentStreamChunk broadcast channel.
            // Subscribers (wake timeout watchdog, crash detector) only care
            // about message/thought/tool-call chunks; handshake-like updates
            // are filtered out inside `session_notification_to_chunks`.
            let chunks = stream_event::session_notification_to_chunks(&notification);
            for chunk in chunks {
                let _ = stream_tx.send(chunk);
            }
        }
        AcpAgentCommand::RequestPermission {
            request,
            responder,
            event_tx,
        } => {
            log_incoming("session/request_permission", &json_str(&request));

            let (resp_tx, resp_rx) = oneshot::channel();

            let perm_req = PermissionRequest {
                request,
                response_tx: resp_tx,
            };

            if event_tx.send(perm_req).await.is_err() {
                warn!("Permission channel closed, cancelling request");
                let _ = responder.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled));
                return;
            }

            let response = match resp_rx.await {
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
    }
}

/// Send an SDK request over the connection, logging the request and response JSON.
///
/// The `method` string is used for log labels and as error context for
/// [`AcpError::from_sdk`].
async fn send_and_log<Req>(connection: &ConnectionTo<Agent>, req: Req, method: &str) -> Result<Req::Response, AcpError>
where
    Req: agent_client_protocol::JsonRpcRequest + serde::Serialize + std::fmt::Debug,
    Req::Response: serde::Serialize + std::fmt::Debug,
{
    log_request(method, &json_str(&req));
    let rsp = connection.send_request(req).block_task().await;
    log_response(method, &json_or_err(&rsp));
    rsp.map_err(|e| AcpError::from_sdk(e, method))
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

/// Log an outgoing agent notification/request (`→`).
fn log_outgoing(method: &str, body: &str) {
    debug!("[ACP] {method}\n → {body}");
}

impl std::fmt::Debug for AcpProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpProtocol")
            .field("alive", &self.is_connected())
            .finish_non_exhaustive()
    }
}
