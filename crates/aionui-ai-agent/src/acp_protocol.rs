//! ACP protocol layer: SDK integration for JSON-RPC communication.
//!
//! This module owns the `agent-client-protocol` SDK connection. It provides
//! typed async methods for all ACP operations and routes incoming agent
//! notifications/requests to the appropriate channels.
//!
//! All requests are dispatched through a command channel to the SDK event loop
//! running inside `connect_with`. This is required because `block_task()` only
//! works within the `connect_with` closure's execution context.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol::schema::{
    CancelNotification, ContentBlock, InitializeRequest, LoadSessionRequest, NewSessionRequest,
    PromptRequest, ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionId, SessionNotification,
    SetSessionConfigOptionRequest, SetSessionModeRequest, SetSessionModelRequest,
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
        workspace: PathBuf,
        reply: oneshot::Sender<Result<SessionId, AcpError>>,
    },
    LoadSession {
        session_id: String,
        workspace: PathBuf,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    Prompt {
        session_id: String,
        content: String,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    Cancel {
        session_id: String,
    },
    SetMode {
        session_id: String,
        mode: String,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    SetModel {
        session_id: String,
        model_id: String,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    SetConfigOption {
        session_id: String,
        config_id: String,
        value: String,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
}

/// ACP protocol handle: wraps the SDK connection and provides typed operations.
///
/// All methods send commands to the SDK event loop via a channel. The event
/// loop runs inside `connect_with` where `block_task()` is safe to use.
pub struct AcpProtocol {
    /// Command sender to the SDK event loop.
    cmd_tx: mpsc::Sender<AcpCommand>,
    /// Background task handle (SDK transport + routing).
    _bg_task: JoinHandle<()>,
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

        let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());

        // Command channel: external methods → SDK event loop
        let (cmd_tx, cmd_rx) = mpsc::channel::<AcpCommand>(32);

        // Signal that init completed successfully
        let (init_tx, init_rx) = oneshot::channel::<Result<(), AcpError>>();

        let bg_task = tokio::spawn(async move {
            let result = Client
                .builder()
                .on_receive_notification(
                    {
                        let event_tx = event_tx.clone();
                        async move |notification: SessionNotification, _cx: ConnectionTo<Agent>| {
                            debug!(
                                session_id = %notification.session_id,
                                update = %format!("{:?}", notification.update),
                                "[ACP:notification] session/update"
                            );
                            let events =
                                stream_event::session_notification_to_events(&notification);
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
                                    responder: agent_client_protocol::Responder<
                            RequestPermissionResponse,
                        >,
                                    _cx: ConnectionTo<Agent>| {
                            let session_id = request.session_id.to_string();
                            debug!(
                                %session_id,
                                "[ACP:request] session/request_permission"
                            );
                            let tool_call =
                                serde_json::to_value(&request.tool_call).unwrap_or_default();
                            let options =
                                serde_json::to_value(&request.options).unwrap_or_default();
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
                                Ok(PermissionDecision::Selected { option_id }) => responder
                                    .respond(RequestPermissionResponse::new(
                                        RequestPermissionOutcome::Selected(
                                            SelectedPermissionOutcome::new(option_id),
                                        ),
                                    )),
                                Ok(PermissionDecision::Cancelled) | Err(_) => {
                                    responder.respond(RequestPermissionResponse::new(
                                        RequestPermissionOutcome::Cancelled,
                                    ))
                                }
                            }
                        }
                    },
                    on_receive_request!(),
                )
                .connect_with(transport, {
                    let mut cmd_rx = cmd_rx;
                    move |connection: ConnectionTo<Agent>| async move {
                        // Execute initialize handshake
                        debug!("Starting ACP initialize handshake");
                        let init_result = connection
                            .send_request(InitializeRequest::new(ProtocolVersion::LATEST))
                            .block_task()
                            .await;

                        if let Err(e) = init_result {
                            let _ = init_tx.send(Err(AcpError::from_sdk(e, "initialize")));
                            return Ok(());
                        }
                        debug!("ACP initialize handshake completed");
                        let _ = init_tx.send(Ok(()));

                        // Command loop: process requests from AcpProtocol methods
                        while let Some(cmd) = cmd_rx.recv().await {
                            match cmd {
                                AcpCommand::NewSession { workspace, reply } => {
                                    debug!(workspace = %workspace.display(), "[ACP:cmd] session/new ->");

                                    let req = NewSessionRequest::new(workspace);
                                    let result = connection
                                        .send_request(req)
                                        .block_task()
                                        .await
                                        .map(|r| r.session_id)
                                        .map_err(|e| AcpError::from_sdk(e, "session/new"));

                                    debug!(result = ?result.as_ref().map(|s| s.to_string()), "[ACP:cmd] session/new <-");
                                    let _ = reply.send(result);
                                }
                                AcpCommand::LoadSession {
                                    session_id,
                                    workspace,
                                    reply,
                                } => {
                                    debug!(%session_id, workspace = %workspace.display(), "[ACP:cmd] session/load ->");

                                    let req = LoadSessionRequest::new(
                                        SessionId::new(session_id.as_str()),
                                        workspace,
                                    );
                                    let result = connection
                                        .send_request(req)
                                        .block_task()
                                        .await
                                        .map(|_| ())
                                        .map_err(|e| AcpError::from_sdk(e, &session_id));

                                    debug!(?result, "[ACP:cmd] session/load <-");
                                    let _ = reply.send(result);
                                }
                                AcpCommand::Prompt {
                                    session_id,
                                    content,
                                    reply,
                                } => {
                                    debug!(%session_id, content_len = content.len(), "[ACP:cmd] session/prompt ->");

                                    let req = PromptRequest::new(
                                        SessionId::new(session_id.as_str()),
                                        vec![ContentBlock::from(content)],
                                    );
                                    let result = connection
                                        .send_request(req)
                                        .block_task()
                                        .await
                                        .map(|_| ())
                                        .map_err(|e| AcpError::from_sdk(e, &session_id));

                                    debug!(?result, "[ACP:cmd] session/prompt <-");
                                    let _ = reply.send(result);
                                }
                                AcpCommand::Cancel { session_id } => {
                                    debug!(%session_id, "[ACP:cmd] session/cancel");
                                    let _ = connection.send_notification(CancelNotification::new(
                                        SessionId::new(session_id.as_str()),
                                    ));
                                }
                                AcpCommand::SetMode {
                                    session_id,
                                    mode,
                                    reply,
                                } => {
                                    debug!(%session_id, %mode, "[ACP:cmd] session/set_mode ->");
                                    let req = SetSessionModeRequest::new(
                                        SessionId::new(session_id.as_str()),
                                        mode,
                                    );
                                    let result = connection
                                        .send_request(req)
                                        .block_task()
                                        .await
                                        .map(|_| ())
                                        .map_err(|e| AcpError::from_sdk(e, &session_id));
                                    debug!(?result, "[ACP:cmd] session/set_mode <-");
                                    let _ = reply.send(result);
                                }
                                AcpCommand::SetModel {
                                    session_id,
                                    model_id,
                                    reply,
                                } => {
                                    debug!(%session_id, %model_id, "[ACP:cmd] session/set_model ->");
                                    let req = SetSessionModelRequest::new(
                                        SessionId::new(session_id.as_str()),
                                        model_id,
                                    );
                                    let result = connection
                                        .send_request(req)
                                        .block_task()
                                        .await
                                        .map(|_| ())
                                        .map_err(|e| AcpError::from_sdk(e, &session_id));
                                    debug!(?result, "[ACP:cmd] session/set_model <-");
                                    let _ = reply.send(result);
                                }
                                AcpCommand::SetConfigOption {
                                    session_id,
                                    config_id,
                                    value,
                                    reply,
                                } => {
                                    debug!(%session_id, %config_id, %value, "[ACP:cmd] session/set_config_option ->");
                                    let req = SetSessionConfigOptionRequest::new(
                                        SessionId::new(session_id.as_str()),
                                        config_id,
                                        value,
                                    );
                                    let result = connection
                                        .send_request(req)
                                        .block_task()
                                        .await
                                        .map(|_| ())
                                        .map_err(|e| AcpError::from_sdk(e, &session_id));
                                    debug!(?result, "[ACP:cmd] session/set_config_option <-");
                                    let _ = reply.send(result);
                                }
                            }
                        }

                        debug!("ACP command channel closed, connection ending");
                        Ok(())
                    }
                })
                .await;

            // Mark connection as dead
            alive_clone.store(false, Ordering::Release);

            match result {
                Ok(_) => debug!("ACP SDK connection closed normally"),
                Err(e) => warn!(error = %e, "ACP SDK connection closed with error"),
            }
        });

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
            _bg_task: bg_task,
            alive,
        })
    }

    /// Create a new ACP session.
    pub async fn new_session(&self, workspace: &str) -> Result<SessionId, AcpError> {
        self.ensure_connected()?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::NewSession {
                workspace: PathBuf::from(workspace),
                reply: tx,
            })
            .await
            .map_err(|_| AcpError::NotConnected)?;
        rx.await.map_err(|_| AcpError::NotConnected)?
    }

    /// Load (resume) an existing ACP session.
    pub async fn load_session(&self, session_id: &str, workspace: &str) -> Result<(), AcpError> {
        self.ensure_connected()?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::LoadSession {
                session_id: session_id.to_owned(),
                workspace: PathBuf::from(workspace),
                reply: tx,
            })
            .await
            .map_err(|_| AcpError::NotConnected)?;
        rx.await.map_err(|_| AcpError::NotConnected)?
    }

    /// Send a prompt to the agent in an active session.
    ///
    /// Blocks until the agent returns a `PromptResponse` (turn completed).
    /// Streaming events arrive via the `event_tx` broadcast channel.
    pub async fn prompt(&self, session_id: &str, content: &str) -> Result<(), AcpError> {
        self.ensure_connected()?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::Prompt {
                session_id: session_id.to_owned(),
                content: content.to_owned(),
                reply: tx,
            })
            .await
            .map_err(|_| AcpError::NotConnected)?;
        rx.await.map_err(|_| AcpError::NotConnected)?
    }

    /// Cancel the current prompt in a session (fire-and-forget notification).
    pub fn cancel(&self, session_id: &str) {
        if !self.is_connected() {
            return;
        }
        let _ = self.cmd_tx.try_send(AcpCommand::Cancel {
            session_id: session_id.to_owned(),
        });
    }

    /// Set the session mode.
    pub async fn set_mode(&self, session_id: &str, mode: &str) -> Result<(), AcpError> {
        self.ensure_connected()?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::SetMode {
                session_id: session_id.to_owned(),
                mode: mode.to_owned(),
                reply: tx,
            })
            .await
            .map_err(|_| AcpError::NotConnected)?;
        rx.await.map_err(|_| AcpError::NotConnected)?
    }

    /// Set the session model.
    pub async fn set_model(&self, session_id: &str, model_id: &str) -> Result<(), AcpError> {
        self.ensure_connected()?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::SetModel {
                session_id: session_id.to_owned(),
                model_id: model_id.to_owned(),
                reply: tx,
            })
            .await
            .map_err(|_| AcpError::NotConnected)?;
        rx.await.map_err(|_| AcpError::NotConnected)?
    }

    /// Set a session config option.
    pub async fn set_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: &str,
    ) -> Result<(), AcpError> {
        self.ensure_connected()?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::SetConfigOption {
                session_id: session_id.to_owned(),
                config_id: config_id.to_owned(),
                value: value.to_owned(),
                reply: tx,
            })
            .await
            .map_err(|_| AcpError::NotConnected)?;
        rx.await.map_err(|_| AcpError::NotConnected)?
    }

    /// Check whether the SDK connection is still alive.
    pub fn is_connected(&self) -> bool {
        self.alive.load(Ordering::Acquire)
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

impl std::fmt::Debug for AcpProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpProtocol")
            .field("alive", &self.is_connected())
            .finish_non_exhaustive()
    }
}
