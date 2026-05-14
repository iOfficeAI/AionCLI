use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info};

use crate::config::CliConfig;

/// Events received from the backend via WebSocket.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerEvent {
    Connected,
    StreamText { content: String },
    StreamThinking { content: String },
    StreamFinish,
    StreamError { message: String },
    Disconnected,
}

/// Response from POST /api/conversations.
#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    data: T,
}

#[derive(Debug, Deserialize)]
struct ConversationData {
    id: String,
}

/// Response from POST /api/conversations/:id/messages.
#[derive(Debug, Deserialize)]
struct SendMessageData {
    msg_id: String,
}

/// The backend communication client.
pub struct AionClient {
    config: CliConfig,
    http: Client,
}

impl AionClient {
    pub fn new(config: CliConfig) -> Self {
        Self {
            config,
            http: Client::new(),
        }
    }

    /// Create a new conversation. Returns the conversation ID.
    pub async fn create_conversation(&self) -> Result<String> {
        let url = self.config.api_url("/api/conversations");
        let body = json!({
            "type": self.config.agent_type,
            "source": "aionui",
            "extra": {}
        });

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to create conversation")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Create conversation failed ({status}): {text}");
        }

        let api_resp: ApiResponse<ConversationData> = resp.json().await?;
        Ok(api_resp.data.id)
    }

    /// Send a message to an existing conversation. Returns the msg_id.
    pub async fn send_message(&self, conversation_id: &str, content: &str) -> Result<String> {
        let url = self
            .config
            .api_url(&format!("/api/conversations/{conversation_id}/messages"));
        let body = json!({
            "content": content,
            "files": [],
            "inject_skills": [],
            "hidden": false
        });

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to send message")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Send message failed ({status}): {text}");
        }

        let api_resp: ApiResponse<SendMessageData> = resp.json().await?;
        Ok(api_resp.data.msg_id)
    }

    /// Connect to the WebSocket and spawn a reader task that sends events to the provided channel.
    pub async fn connect_ws(&self, tx: mpsc::UnboundedSender<ServerEvent>) -> Result<()> {
        let url = self.config.ws_url();
        info!("Connecting WebSocket to {url}");

        let (ws_stream, _) = connect_async(&url)
            .await
            .context("WebSocket connection failed")?;

        info!("WebSocket connected");
        let _ = tx.send(ServerEvent::Connected);

        let (_write, mut read) = ws_stream.split();

        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Some(event) = parse_ws_message(&text) {
                            if tx.send(event).is_err() {
                                break;
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        let _ = tx.send(ServerEvent::Disconnected);
                        break;
                    }
                    Err(e) => {
                        error!("WebSocket error: {e}");
                        let _ = tx.send(ServerEvent::Disconnected);
                        break;
                    }
                    _ => {}
                }
            }
        });

        Ok(())
    }
}

/// Parse a WebSocket text message into a ServerEvent.
pub fn parse_ws_message(raw: &str) -> Option<ServerEvent> {
    let msg: Value = serde_json::from_str(raw).ok()?;
    let name = msg.get("name")?.as_str()?;

    if name != "message.stream" {
        debug!("Ignoring WS event: {name}");
        return None;
    }

    let data = msg.get("data")?;
    let event_type = data.get("type")?.as_str()?;

    match event_type {
        "text" => {
            let content = data
                .get("data")
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            Some(ServerEvent::StreamText { content })
        }
        "thinking" => {
            let content = data
                .get("data")
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            Some(ServerEvent::StreamThinking { content })
        }
        "finish" => Some(ServerEvent::StreamFinish),
        "error" => {
            let message = data
                .get("data")
                .and_then(|d| d.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error")
                .to_string();
            Some(ServerEvent::StreamError { message })
        }
        _ => {
            debug!("Ignoring stream event type: {event_type}");
            None
        }
    }
}
