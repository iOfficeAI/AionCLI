use std::net::SocketAddr;
use std::sync::{Arc, Weak};

use aionui_common::generate_id;
use tokio::net::TcpListener;
use tokio::sync::{RwLock, oneshot};
use tracing::{debug, info, warn};

use crate::service::TeamSessionService;

pub struct GuideMcpServer {
    http_addr: SocketAddr,
    auth_token: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Shared slot so the accept-loop can see the service after it is wired up.
    service_slot: Arc<RwLock<Weak<TeamSessionService>>>,
}

impl GuideMcpServer {
    pub async fn start() -> Result<Self, String> {
        let auth_token = generate_id();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("Failed to bind guide MCP HTTP listener: {e}"))?;
        let http_addr = listener
            .local_addr()
            .map_err(|e| format!("Failed to read guide MCP local addr: {e}"))?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let service_slot: Arc<RwLock<Weak<TeamSessionService>>> = Arc::new(RwLock::new(Weak::new()));

        tokio::spawn(accept_loop(
            listener,
            auth_token.clone(),
            shutdown_rx,
            service_slot.clone(),
        ));

        debug!(http_port = http_addr.port(), "Guide MCP Server started");

        Ok(Self {
            http_addr,
            auth_token,
            shutdown_tx: Some(shutdown_tx),
            service_slot,
        })
    }

    /// Wire the TeamSessionService after it is constructed.
    /// Must be called once before the first `aion_create_team` request arrives.
    pub async fn set_service(&self, service: Weak<TeamSessionService>) {
        *self.service_slot.write().await = service;
    }

    pub fn http_port(&self) -> u16 {
        self.http_addr.port()
    }

    pub fn http_addr(&self) -> SocketAddr {
        self.http_addr
    }

    pub fn auth_token(&self) -> &str {
        &self.auth_token
    }

    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
            debug!(http_port = self.http_addr.port(), "Guide MCP Server stop requested");
        }
    }
}

impl Drop for GuideMcpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn handle_aion_create_team(
    request_body: &serde_json::Value,
    args: &serde_json::Value,
    service: Arc<RwLock<Weak<TeamSessionService>>>,
) -> serde_json::Value {
    use crate::guide::handlers::parse_create_team_args;
    use aionui_api_types::{CreateTeamRequest, TeamAgentInput};

    let svc = match service.read().await.upgrade() {
        Some(s) => s,
        None => {
            warn!("Guide HTTP: aion_create_team — service not available");
            return serde_json::json!({"error": "service_unavailable"});
        }
    };

    let caller_workspace: Option<&str> = None;
    let params = match parse_create_team_args(args, caller_workspace) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Guide HTTP: aion_create_team parse error");
            return serde_json::json!({"error": e});
        }
    };

    let backend = request_body
        .get("backend")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("claude")
        .to_owned();

    let model = request_body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();

    let req = CreateTeamRequest {
        name: params.name.clone(),
        agents: vec![TeamAgentInput {
            name: "Leader".to_owned(),
            role: "leader".to_owned(),
            backend: backend.clone(),
            model: model.clone(),
            custom_agent_id: None,
        }],
    };

    let user_id = "system_default_user";

    let team = match svc.create_team(user_id, req).await {
        Ok(t) => t,
        Err(e) => {
            warn!(error = %e, "Guide HTTP: aion_create_team create_team failed");
            return serde_json::json!({"error": e.to_string()});
        }
    };

    // ensure_session is already called inside create_team, but call send_message separately.
    // Fire-and-forget: send summary to leader so it can plan/spawn teammates.
    let team_id = team.id.clone();
    let summary = params.summary.clone();
    let svc2 = svc.clone();
    tokio::spawn(async move {
        if let Err(e) = svc2.send_message(&team_id, &summary, None).await {
            warn!(team_id = %team_id, error = %e, "Guide HTTP: failed to send summary to leader");
        }
    });

    let route = format!("/team/{}", team.id);
    info!(team_id = %team.id, "Guide HTTP: aion_create_team succeeded");
    serde_json::json!({
        "teamId": team.id,
        "name": team.name,
        "route": route,
        "status": "team_created",
        "next_step": "The team page has been opened automatically. End your turn now — do not add extra commentary."
    })
}

async fn accept_loop(
    listener: TcpListener,
    auth_token: String,
    mut shutdown_rx: oneshot::Receiver<()>,
    service: Arc<RwLock<Weak<TeamSessionService>>>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                debug!("Guide MCP Server shutting down");
                break;
            }
            accept = listener.accept() => {
                let Ok((mut stream, peer)) = accept else { continue };
                info!(?peer, "Guide HTTP: new connection accepted");
                let token = auth_token.clone();
                let svc = service.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let n = match stream.read(&mut buf).await {
                        Ok(n) if n > 0 => n,
                        _ => return,
                    };
                    let request = String::from_utf8_lossy(&buf[..n]);

                    // Auth: extract Bearer token from Authorization header
                    let provided_token = request
                        .lines()
                        .find(|l| l.to_lowercase().starts_with("authorization:"))
                        .and_then(|l| l.split_once(':').map(|(_, v)| v.trim()))
                        .and_then(|v| v.strip_prefix("Bearer "))
                        .unwrap_or("");

                    if provided_token != token {
                        warn!(?peer, "Guide HTTP: unauthorized request (bad or missing token)");
                        let resp = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n";
                        let _ = stream.write_all(resp.as_bytes()).await;
                        return;
                    }

                    // Extract JSON body (after \r\n\r\n)
                    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
                    debug!(?peer, body_preview = &body[..body.len().min(200)], "Guide HTTP: request body");

                    let value: serde_json::Value = match serde_json::from_str(body) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(?peer, error = %e, "Guide HTTP: JSON parse failed");
                            let resp = "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n";
                            let _ = stream.write_all(resp.as_bytes()).await;
                            return;
                        }
                    };

                    let tool = value.get("tool").and_then(serde_json::Value::as_str).unwrap_or("");
                    let args = value.get("args").cloned().unwrap_or(serde_json::Value::Null);

                    info!(?peer, tool, "Guide HTTP: dispatching tool");

                    let response_body = match tool {
                        "aion_create_team" => {
                            handle_aion_create_team(&value, &args, svc).await
                        }
                        "aion_list_models" => {
                            let result = crate::guide::handlers::handle_aion_list_models();
                            info!(?peer, "Guide HTTP: aion_list_models succeeded");
                            serde_json::json!({"result": serde_json::to_string(&result).unwrap_or_default()})
                        }
                        unknown => {
                            warn!(?peer, tool = unknown, "Guide HTTP: unknown tool");
                            serde_json::json!({"error": format!("Unknown tool: {unknown}")})
                        }
                    };

                    let body_bytes = serde_json::to_vec(&response_body).unwrap_or_default();
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                        body_bytes.len()
                    );
                    let _ = stream.write_all(header.as_bytes()).await;
                    let _ = stream.write_all(&body_bytes).await;
                    info!(?peer, tool, "Guide HTTP: response sent");
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpStream;
    use tokio::time::timeout;

    #[tokio::test]
    async fn start_returns_positive_port_and_token() {
        let server = GuideMcpServer::start().await.expect("start should succeed");
        assert!(server.http_port() > 0, "http_port should be assigned");
        assert!(!server.auth_token().is_empty(), "auth_token should be generated");
    }

    #[tokio::test]
    async fn each_start_uses_a_fresh_auth_token() {
        let a = GuideMcpServer::start().await.unwrap();
        let b = GuideMcpServer::start().await.unwrap();
        assert_ne!(a.auth_token(), b.auth_token());
    }

    #[tokio::test]
    async fn stop_closes_the_listener() {
        let mut server = GuideMcpServer::start().await.unwrap();
        let port = server.http_port();
        server.stop();

        // Give the accept loop a moment to observe the shutdown signal.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Either the connect is refused outright, or it succeeds but the
        // listener-less port yields an immediate EOF on read. Both are
        // acceptable evidence that the server is no longer serving.
        match timeout(Duration::from_millis(200), TcpStream::connect(("127.0.0.1", port))).await {
            Ok(Ok(mut stream)) => {
                let mut buf = [0u8; 1];
                let read = timeout(Duration::from_millis(200), stream.read(&mut buf)).await;
                match read {
                    Ok(Ok(0)) => { /* EOF — expected */ }
                    Ok(Err(_)) => { /* connection error — expected */ }
                    Ok(Ok(_)) => panic!("unexpected data from stopped server"),
                    Err(_) => panic!("server still reading after stop"),
                }
            }
            Ok(Err(_)) => { /* connection refused — expected */ }
            Err(_) => panic!("connect timed out (expected refuse or EOF)"),
        }
    }

    #[tokio::test]
    async fn stop_is_idempotent() {
        let mut server = GuideMcpServer::start().await.unwrap();
        server.stop();
        server.stop();
    }
}
