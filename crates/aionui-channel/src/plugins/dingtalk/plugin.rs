use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::constants::DINGTALK_MESSAGE_LIMIT;
use crate::error::ChannelError;
use crate::plugin::{ChannelPlugin, PluginCallbacks};
use crate::types::{
    ActionCategory, ActionContext, BotInfo, MessageContentType, PluginConfig, PluginStatus, PluginType, UnifiedAction,
    UnifiedIncomingMessage, UnifiedMessageContent, UnifiedOutgoingMessage, UnifiedUser,
};

use super::api::DingtalkApi;
use super::types::{
    BotMessageCallback, CardActionCallback, CardData, CreateCardInstanceRequest, DeliverCardRequest,
    ImGroupDeliverModel, ImRobotDeliverModel, SendRobotMessageRequest, StreamAck, StreamFrame, StreamingWriteRequest,
    SystemEvent, UpdateCardRequest, build_open_space_id, decode_chat_id, encode_chat_id, format_dingtalk_callback,
    parse_dingtalk_callback,
};

/// Maximum reconnect attempts before giving up.
const MAX_RECONNECT_ATTEMPTS: u32 = 10;

/// Maximum backoff delay between reconnection attempts.
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

/// DingTalk standard AI card template ID.
const AI_CARD_TEMPLATE_ID: &str = "StandardCard";

/// DingTalk platform plugin.
///
/// Connects via WebSocket Stream, handles bot message callbacks and
/// card action callbacks. Uses AI Card for streaming message updates
/// with fallback to session webhook or Open API.
pub struct DingtalkPlugin {
    status: PluginStatus,
    bot_info: Option<BotInfo>,
    last_error: Option<String>,
    api: Option<Arc<DingtalkApi>>,
    ws_handle: Option<JoinHandle<()>>,
    shutdown_tx: Option<watch::Sender<bool>>,
}

impl Default for DingtalkPlugin {
    fn default() -> Self {
        Self {
            status: PluginStatus::Created,
            bot_info: None,
            last_error: None,
            api: None,
            ws_handle: None,
            shutdown_tx: None,
        }
    }
}

impl DingtalkPlugin {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl ChannelPlugin for DingtalkPlugin {
    async fn initialize(&mut self, config: PluginConfig, callbacks: PluginCallbacks) -> Result<(), ChannelError> {
        self.status = PluginStatus::Initializing;

        let client_id = config
            .credentials
            .client_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                self.status = PluginStatus::Error;
                self.last_error = Some("Missing DingTalk client_id".into());
                ChannelError::InvalidConfig("Missing DingTalk client_id".into())
            })?;

        let client_secret = config
            .credentials
            .client_secret
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                self.status = PluginStatus::Error;
                self.last_error = Some("Missing DingTalk client_secret".into());
                ChannelError::InvalidConfig("Missing DingTalk client_secret".into())
            })?;

        let http_client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| {
                self.status = PluginStatus::Error;
                self.last_error = Some(format!("HTTP client init failed: {e}"));
                ChannelError::ConnectionFailed(format!("HTTP client init failed: {e}"))
            })?;

        let api = Arc::new(DingtalkApi::new(http_client, client_id, client_secret));

        // Validate credentials by getting bot info
        let bot_data = api.get_bot_info().await.map_err(|e| {
            self.status = PluginStatus::Error;
            self.last_error = Some(format!("Credential validation failed: {e}"));
            e
        })?;

        self.bot_info = Some(BotInfo {
            id: bot_data.robot_user_id.clone().unwrap_or_default(),
            username: None,
            display_name: bot_data.nick.clone().unwrap_or_default(),
        });

        info!(
            bot_name = bot_data.nick.as_deref().unwrap_or(""),
            bot_id = bot_data.robot_user_id.as_deref().unwrap_or(""),
            "DingTalk bot initialized"
        );

        self.api = Some(api);

        // Set up shutdown channel
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        self.shutdown_tx = Some(shutdown_tx);

        // Spawn the WebSocket Stream connection loop
        let api_clone = Arc::clone(self.api.as_ref().expect("api just set"));
        self.ws_handle = Some(tokio::spawn(ws_stream_loop(
            api_clone,
            callbacks.message_tx,
            callbacks.confirm_tx,
            shutdown_rx,
        )));

        self.status = PluginStatus::Ready;
        Ok(())
    }

    async fn start(&mut self) -> Result<(), ChannelError> {
        self.status = PluginStatus::Starting;
        self.status = PluginStatus::Running;
        info!("DingTalk plugin started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), ChannelError> {
        self.status = PluginStatus::Stopping;

        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }

        if let Some(handle) = self.ws_handle.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }

        self.api = None;
        self.status = PluginStatus::Stopped;
        info!("DingTalk plugin stopped");
        Ok(())
    }

    async fn send_message(&self, chat_id: &str, message: UnifiedOutgoingMessage) -> Result<String, ChannelError> {
        let api = self
            .api
            .as_ref()
            .ok_or_else(|| ChannelError::PlatformApi("Plugin not initialized".into()))?;

        let text = truncate_message(message.text.as_deref().unwrap_or(""), DINGTALK_MESSAGE_LIMIT);

        // Try AI Card first
        match send_via_ai_card(api, chat_id, &text, message.buttons.as_deref()).await {
            Ok(card_id) => return Ok(card_id),
            Err(e) => {
                warn!(error = %e, "AI Card send failed, falling back to Open API");
            }
        }

        // Fallback: send via Open API
        send_via_open_api(api, chat_id, &text).await
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        message_id: &str,
        message: UnifiedOutgoingMessage,
    ) -> Result<(), ChannelError> {
        let api = self
            .api
            .as_ref()
            .ok_or_else(|| ChannelError::PlatformApi("Plugin not initialized".into()))?;

        let text = truncate_message(message.text.as_deref().unwrap_or(""), DINGTALK_MESSAGE_LIMIT);

        // Presence of buttons signals the final message in a streaming sequence.
        let is_final = message.buttons.is_some();

        // AI Card streaming write
        let req = StreamingWriteRequest {
            out_track_id: message_id.to_string(),
            key: "content".into(),
            content: text,
            is_eof: is_final,
            is_last: is_final,
        };

        api.streaming_write(&req).await?;

        // When finalizing, update the card with button actions.
        if let Some(ref button_rows) = message.buttons {
            let card_param_map = build_card_param_map("", Some(button_rows));
            let update_req = UpdateCardRequest {
                out_track_id: message_id.to_string(),
                card_data: CardData {
                    card_param_map: Some(card_param_map),
                },
            };
            api.update_card(&update_req).await?;
        }

        Ok(())
    }

    fn active_user_count(&self) -> usize {
        0
    }

    fn bot_info(&self) -> Option<&BotInfo> {
        self.bot_info.as_ref()
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Dingtalk
    }

    fn status(&self) -> PluginStatus {
        self.status
    }

    fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

// ---------------------------------------------------------------------------
// AI Card operations
// ---------------------------------------------------------------------------

/// Send a message via AI Card (create + deliver).
async fn send_via_ai_card(
    api: &Arc<DingtalkApi>,
    chat_id: &str,
    text: &str,
    buttons: Option<&[Vec<crate::types::ActionButton>]>,
) -> Result<String, ChannelError> {
    let (is_group, _) = decode_chat_id(chat_id);

    let card_param_map = build_card_param_map(text, buttons);

    let create_req = CreateCardInstanceRequest {
        card_template_id: AI_CARD_TEMPLATE_ID.into(),
        card_data: CardData {
            card_param_map: Some(card_param_map),
        },
        im_group_open_deliver_model: if is_group {
            Some(ImGroupDeliverModel {
                robot_code: api.client_id().to_string(),
            })
        } else {
            None
        },
        im_robot_open_deliver_model: if !is_group {
            Some(ImRobotDeliverModel {
                robot_code: api.client_id().to_string(),
            })
        } else {
            None
        },
    };

    let create_resp = api.create_card_instance(&create_req).await?;
    let card_id = create_resp
        .result
        .and_then(|r| r.out_track_id)
        .ok_or_else(|| ChannelError::MessageSendFailed("DingTalk card create returned no outTrackId".into()))?;

    // Deliver the card
    let open_space_id = build_open_space_id(chat_id);

    let deliver_req = DeliverCardRequest {
        out_track_id: card_id.clone(),
        open_space_id,
        im_group_open_deliver_model: if is_group {
            Some(ImGroupDeliverModel {
                robot_code: api.client_id().to_string(),
            })
        } else {
            None
        },
        im_robot_open_deliver_model: if !is_group {
            Some(ImRobotDeliverModel {
                robot_code: api.client_id().to_string(),
            })
        } else {
            None
        },
    };

    api.deliver_card(&deliver_req).await?;

    debug!(card_id = %card_id, "DingTalk AI Card delivered");
    Ok(card_id)
}

/// Send a message via DingTalk Open API (fallback).
async fn send_via_open_api(api: &Arc<DingtalkApi>, chat_id: &str, text: &str) -> Result<String, ChannelError> {
    let (is_group, raw_id) = decode_chat_id(chat_id);

    let req = SendRobotMessageRequest {
        msg_key: "sampleText".into(),
        msg_param: serde_json::json!({ "content": text }).to_string(),
        robot_code: api.client_id().to_string(),
        open_conversation_id: if is_group { Some(raw_id.to_string()) } else { None },
        user_ids: if !is_group {
            Some(vec![raw_id.to_string()])
        } else {
            None
        },
    };

    let resp = api.send_robot_message(&req).await?;
    let msg_id = resp
        .process_query_key
        .unwrap_or_else(|| format!("dt_msg_{}", chrono_now()));
    Ok(msg_id)
}

/// Build the card_param_map for an AI Card.
fn build_card_param_map(text: &str, buttons: Option<&[Vec<crate::types::ActionButton>]>) -> serde_json::Value {
    let mut map = serde_json::json!({
        "content": text
    });

    if let Some(button_rows) = buttons {
        let mut action_list = Vec::new();
        for row in button_rows {
            for btn in row {
                let callback_value = format_dingtalk_callback(&btn.action, btn.params.as_ref());
                action_list.push(serde_json::json!({
                    "label": btn.label,
                    "action": callback_value
                }));
            }
        }
        if !action_list.is_empty() {
            map["actions"] = serde_json::json!(action_list);
        }
    }

    map
}

// ---------------------------------------------------------------------------
// WebSocket Stream connection loop
// ---------------------------------------------------------------------------

/// Background task that maintains a WebSocket Stream connection to DingTalk.
///
/// On disconnect, implements exponential backoff reconnection up to
/// `MAX_RECONNECT_ATTEMPTS`.
async fn ws_stream_loop(
    api: Arc<DingtalkApi>,
    message_tx: mpsc::Sender<UnifiedIncomingMessage>,
    confirm_tx: mpsc::Sender<(String, String)>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut consecutive_errors: u32 = 0;

    loop {
        if *shutdown_rx.borrow() {
            debug!("DingTalk WS loop received shutdown signal");
            break;
        }

        // Register and get stream endpoint
        let stream_info = match api.register_stream().await {
            Ok(info) => {
                consecutive_errors = 0;
                info!(endpoint = %info.endpoint, "DingTalk stream registered successfully");
                info
            }
            Err(e) => {
                consecutive_errors += 1;
                warn!(error = %e, consecutive_errors, "DingTalk stream registration failed");
                if consecutive_errors >= MAX_RECONNECT_ATTEMPTS {
                    error!("DingTalk max reconnect attempts reached");
                    break;
                }
                let delay = backoff_delay(consecutive_errors);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => continue,
                    _ = shutdown_rx.changed() => break,
                }
            }
        };

        let ws_url = format!("{}?ticket={}", stream_info.endpoint, stream_info.ticket);

        info!(url = %ws_url, "Connecting to DingTalk WebSocket Stream");

        match connect_and_listen(&ws_url, &message_tx, &confirm_tx, &mut shutdown_rx).await {
            Ok(()) => {
                debug!("DingTalk WS connection closed cleanly");
                break;
            }
            Err(e) => {
                consecutive_errors += 1;
                warn!(
                    error = %e,
                    consecutive_errors,
                    "DingTalk WS connection error"
                );
                if consecutive_errors >= MAX_RECONNECT_ATTEMPTS {
                    error!("DingTalk max reconnect attempts reached");
                    break;
                }
                let delay = backoff_delay(consecutive_errors);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shutdown_rx.changed() => break,
                }
            }
        }
    }

    debug!("DingTalk WS loop exited");
}

/// Connect to the WebSocket and listen for frames until disconnected.
async fn connect_and_listen(
    ws_url: &str,
    message_tx: &mpsc::Sender<UnifiedIncomingMessage>,
    confirm_tx: &mpsc::Sender<(String, String)>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> Result<(), ChannelError> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let (ws_stream, _) = connect_async(ws_url)
        .await
        .map_err(|e| ChannelError::ConnectionFailed(format!("DingTalk WS connect failed: {e}")))?;

    info!("DingTalk WebSocket Stream connected");

    let (mut write, mut read) = ws_stream.split();

    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        if let Some(ack) = handle_stream_frame(
                            &text,
                            message_tx,
                            confirm_tx,
                        ).await {
                            let ack_json = serde_json::to_string(&ack)
                                .unwrap_or_default();
                            if let Err(e) = write.send(WsMessage::Text(ack_json.into())).await {
                                warn!(error = %e, "Failed to send DingTalk stream ack");
                            }
                        }
                    }
                    Some(Ok(WsMessage::Ping(data))) => {
                        if let Err(e) = write.send(WsMessage::Pong(data)).await {
                            warn!(error = %e, "Failed to send DingTalk pong");
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) => {
                        debug!("DingTalk WS received close frame");
                        return Ok(());
                    }
                    Some(Err(e)) => {
                        return Err(ChannelError::ConnectionFailed(
                            format!("DingTalk WS read error: {e}")
                        ));
                    }
                    None => {
                        return Err(ChannelError::ConnectionFailed(
                            "DingTalk WS stream ended unexpectedly".into()
                        ));
                    }
                    _ => {} // Binary, Frame — ignore
                }
            }
            _ = shutdown_rx.changed() => {
                debug!("DingTalk WS shutdown during listen");
                return Ok(());
            }
        }
    }
}

/// Handle a stream frame and optionally return an acknowledgment.
async fn handle_stream_frame(
    text: &str,
    message_tx: &mpsc::Sender<UnifiedIncomingMessage>,
    confirm_tx: &mpsc::Sender<(String, String)>,
) -> Option<StreamAck> {
    let frame: StreamFrame = match serde_json::from_str(text) {
        Ok(f) => f,
        Err(e) => {
            warn!(error = %e, "Failed to parse DingTalk stream frame");
            return None;
        }
    };

    let message_id = frame.headers.message_id.clone().unwrap_or_default();

    match frame.frame_type.as_str() {
        "SYSTEM" => {
            let topic = frame.headers.topic.as_deref().unwrap_or("");
            match topic {
                "ping" => {
                    debug!("DingTalk system ping received, sending pong");
                    Some(StreamAck {
                        code: 200,
                        headers: super::types::AckHeaders {
                            content_type: "application/json".into(),
                            message_id: message_id.clone(),
                        },
                        message: "OK".into(),
                        data: frame.data.unwrap_or_else(|| "{}".into()),
                    })
                }
                _ => {
                    if let Some(ref data_str) = frame.data
                        && let Ok(sys) = serde_json::from_str::<SystemEvent>(data_str)
                    {
                        debug!(
                            code = sys.code,
                            message = sys.message.as_deref().unwrap_or(""),
                            topic,
                            "DingTalk system event"
                        );
                    }
                    None
                }
            }
        }
        "CALLBACK" => {
            let topic = frame.headers.topic.as_deref().unwrap_or("");
            let data_str = frame.data.as_deref().unwrap_or("");

            match topic {
                "/v1.0/im/bot/messages/get" => {
                    handle_bot_message(data_str, message_tx).await;
                }
                "/v1.0/card/instances/callback" => {
                    handle_card_action(data_str, message_tx, confirm_tx).await;
                }
                _ => {
                    debug!(topic, "DingTalk unhandled callback topic");
                }
            }

            // Always ack CALLBACK frames
            Some(build_ack(&message_id))
        }
        "EVENT" => {
            // Ack event frames
            Some(build_ack(&message_id))
        }
        other => {
            debug!(frame_type = other, "DingTalk unhandled stream frame type");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Event handlers
// ---------------------------------------------------------------------------

/// Handle a bot message callback.
async fn handle_bot_message(data_str: &str, message_tx: &mpsc::Sender<UnifiedIncomingMessage>) {
    let cb: BotMessageCallback = match serde_json::from_str(data_str) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, raw = %&data_str[..data_str.len().min(200)], "Failed to parse DingTalk bot message");
            return;
        }
    };

    debug!(
        msg_id = cb.msg_id.as_deref().unwrap_or(""),
        sender = cb.sender_nick.as_deref().unwrap_or(""),
        msgtype = cb.msgtype.as_deref().unwrap_or(""),
        "DingTalk bot message received"
    );

    let sender_staff_id = cb
        .sender_staff_id
        .as_deref()
        .or(cb.sender_id.as_deref())
        .unwrap_or("unknown");

    let chat_id = encode_chat_id(cb.conversation_id.as_deref(), sender_staff_id);

    let user = UnifiedUser {
        id: sender_staff_id.to_string(),
        username: None,
        display_name: cb.sender_nick.clone().unwrap_or_default(),
        avatar_url: None,
    };

    let (content_type, text) = extract_message_content(cb.msgtype.as_deref().unwrap_or("text"), &cb);

    let timestamp = cb.create_at.map(|ms| ms / 1000).unwrap_or_else(chrono_now);

    let unified = UnifiedIncomingMessage {
        id: cb.msg_id.clone().unwrap_or_default(),
        platform: PluginType::Dingtalk,
        chat_id,
        user,
        content: UnifiedMessageContent {
            content_type,
            text,
            attachments: None,
        },
        timestamp,
        reply_to_message_id: None,
        action: None,
        raw: None,
    };

    let _ = message_tx.send(unified).await;
}

/// Handle a card action callback.
async fn handle_card_action(
    data_str: &str,
    message_tx: &mpsc::Sender<UnifiedIncomingMessage>,
    confirm_tx: &mpsc::Sender<(String, String)>,
) {
    let cb: CardActionCallback = match serde_json::from_str(data_str) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "Failed to parse DingTalk card action");
            return;
        }
    };

    debug!(
        user_id = cb.user_id.as_deref().unwrap_or(""),
        "DingTalk card action received"
    );

    // Extract action string from content field
    let action_str = cb
        .content
        .as_deref()
        .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
        .and_then(|v| v.get("action").and_then(|a| a.as_str()).map(String::from))
        .unwrap_or_default();

    let parsed = parse_dingtalk_callback(&action_str);

    // Check if this is a tool confirmation
    if let Some((_, ref action, ref params)) = parsed
        && action == "system.confirm"
        && let Some(p) = params
    {
        let call_id = p.get("callId").cloned().unwrap_or_default();
        let value = p.get("value").cloned().unwrap_or_default();
        if !call_id.is_empty() {
            let _ = confirm_tx.send((call_id, value)).await;
        }
    }

    let user_id = cb.user_id.clone().unwrap_or_default();
    let chat_id = match cb.open_conversation_id.as_deref() {
        Some(cid) if !cid.is_empty() => format!("group:{cid}"),
        _ => format!("user:{}", user_id),
    };

    let user = UnifiedUser {
        id: user_id.clone(),
        username: None,
        display_name: user_id.clone(),
        avatar_url: None,
    };

    let unified_action = parsed.map(|(cat_str, action, params)| {
        let category = match cat_str.as_str() {
            "platform" => ActionCategory::Platform,
            "chat" => ActionCategory::Chat,
            _ => ActionCategory::System,
        };
        UnifiedAction {
            action,
            category,
            params,
            context: ActionContext {
                platform: PluginType::Dingtalk,
                user_id: user_id.clone(),
                chat_id: chat_id.clone(),
                message_id: None,
                session_id: None,
            },
        }
    });

    let msg = UnifiedIncomingMessage {
        id: format!("card_{}", chrono_now()),
        platform: PluginType::Dingtalk,
        chat_id,
        user,
        content: UnifiedMessageContent {
            content_type: MessageContentType::Action,
            text: action_str,
            attachments: None,
        },
        timestamp: chrono_now(),
        reply_to_message_id: None,
        action: unified_action,
        raw: None,
    };

    let _ = message_tx.send(msg).await;
}

// ---------------------------------------------------------------------------
// Message content extraction
// ---------------------------------------------------------------------------

/// Extract content type and text from a DingTalk bot message callback.
fn extract_message_content(msgtype: &str, cb: &BotMessageCallback) -> (MessageContentType, String) {
    match msgtype {
        "text" => {
            let text = cb
                .text
                .as_ref()
                .and_then(|t| t.content.as_deref())
                .unwrap_or("")
                .to_string();

            if text.starts_with('/') {
                (MessageContentType::Command, text)
            } else {
                (MessageContentType::Text, text)
            }
        }
        "picture" => (MessageContentType::Photo, "[Picture]".to_string()),
        "file" => (MessageContentType::Document, "[File]".to_string()),
        "audio" => (MessageContentType::Audio, "[Audio]".to_string()),
        "video" => (MessageContentType::Video, "[Video]".to_string()),
        _ => (
            MessageContentType::Text,
            format!("[Unsupported message type: {msgtype}]"),
        ),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a stream acknowledgment response.
fn build_ack(message_id: &str) -> StreamAck {
    StreamAck {
        code: 200,
        headers: super::types::AckHeaders {
            content_type: "application/json".into(),
            message_id: message_id.to_string(),
        },
        message: "OK".into(),
        data: r#"{"response":"SUCCESS"}"#.into(),
    }
}

/// Truncate a message to the platform limit, appending "..." if truncated.
fn truncate_message(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let truncated: String = text.chars().take(limit - 3).collect();
    format!("{truncated}...")
}

/// Calculate exponential backoff delay, capped at the maximum.
fn backoff_delay(attempt: u32) -> Duration {
    let delay_secs = 2u64.saturating_pow(attempt).min(MAX_RECONNECT_DELAY.as_secs());
    Duration::from_secs(delay_secs)
}

/// Current unix timestamp in seconds.
fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- truncate_message ---------------------------------------------------

    #[test]
    fn truncate_within_limit() {
        assert_eq!(truncate_message("Hello", 100), "Hello");
    }

    #[test]
    fn truncate_at_limit() {
        assert_eq!(truncate_message("abc", 3), "abc");
    }

    #[test]
    fn truncate_exceeds_limit() {
        let result = truncate_message("Hello, world!", 10);
        assert_eq!(result, "Hello, ...");
    }

    #[test]
    fn truncate_unicode() {
        let result = truncate_message("你好世界测试文本", 5);
        assert_eq!(result, "你好...");
    }

    // -- backoff_delay ------------------------------------------------------

    #[test]
    fn backoff_exponential() {
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(2), Duration::from_secs(4));
        assert_eq!(backoff_delay(3), Duration::from_secs(8));
    }

    #[test]
    fn backoff_capped() {
        assert_eq!(backoff_delay(5), Duration::from_secs(30));
        assert_eq!(backoff_delay(10), Duration::from_secs(30));
    }

    // -- extract_message_content --------------------------------------------

    #[test]
    fn extract_text_content() {
        let cb = BotMessageCallback {
            conversation_id: None,
            msg_id: None,
            msgtype: Some("text".into()),
            text: Some(super::super::types::TextPayload {
                content: Some("Hello".into()),
            }),
            sender_id: None,
            sender_nick: None,
            sender_staff_id: None,
            session_webhook: None,
            session_webhook_expired_time: None,
            conversation_type: None,
            is_in_at_list: None,
            at_users: None,
            create_at: None,
            robot_code: None,
        };
        let (ct, text) = extract_message_content("text", &cb);
        assert_eq!(ct, MessageContentType::Text);
        assert_eq!(text, "Hello");
    }

    #[test]
    fn extract_text_command() {
        let cb = BotMessageCallback {
            conversation_id: None,
            msg_id: None,
            msgtype: Some("text".into()),
            text: Some(super::super::types::TextPayload {
                content: Some("/start".into()),
            }),
            sender_id: None,
            sender_nick: None,
            sender_staff_id: None,
            session_webhook: None,
            session_webhook_expired_time: None,
            conversation_type: None,
            is_in_at_list: None,
            at_users: None,
            create_at: None,
            robot_code: None,
        };
        let (ct, text) = extract_message_content("text", &cb);
        assert_eq!(ct, MessageContentType::Command);
        assert_eq!(text, "/start");
    }

    #[test]
    fn extract_picture_content() {
        let cb = BotMessageCallback {
            conversation_id: None,
            msg_id: None,
            msgtype: Some("picture".into()),
            text: None,
            sender_id: None,
            sender_nick: None,
            sender_staff_id: None,
            session_webhook: None,
            session_webhook_expired_time: None,
            conversation_type: None,
            is_in_at_list: None,
            at_users: None,
            create_at: None,
            robot_code: None,
        };
        let (ct, _) = extract_message_content("picture", &cb);
        assert_eq!(ct, MessageContentType::Photo);
    }

    #[test]
    fn extract_unsupported_type() {
        let cb = BotMessageCallback {
            conversation_id: None,
            msg_id: None,
            msgtype: Some("richText".into()),
            text: None,
            sender_id: None,
            sender_nick: None,
            sender_staff_id: None,
            session_webhook: None,
            session_webhook_expired_time: None,
            conversation_type: None,
            is_in_at_list: None,
            at_users: None,
            create_at: None,
            robot_code: None,
        };
        let (ct, text) = extract_message_content("richText", &cb);
        assert_eq!(ct, MessageContentType::Text);
        assert!(text.contains("Unsupported"));
    }

    // -- build_card_param_map -----------------------------------------------

    #[test]
    fn build_card_param_map_text_only() {
        let map = build_card_param_map("Hello", None);
        assert_eq!(map["content"], "Hello");
        assert!(map.get("actions").is_none());
    }

    #[test]
    fn build_card_param_map_with_buttons() {
        use crate::types::ActionButton;
        let buttons = vec![vec![ActionButton {
            label: "Yes".into(),
            action: "system.confirm".into(),
            params: None,
        }]];
        let map = build_card_param_map("Choose:", Some(&buttons));
        assert_eq!(map["content"], "Choose:");
        let actions = map["actions"].as_array().unwrap();
        assert_eq!(actions[0]["label"], "Yes");
        assert!(actions[0]["action"].as_str().unwrap().contains("system.confirm"));
    }

    // -- build_ack ----------------------------------------------------------

    #[test]
    fn build_ack_structure() {
        let ack = build_ack("msg_123");
        assert_eq!(ack.code, 200);
        assert_eq!(ack.headers.message_id, "msg_123");
        assert_eq!(ack.message, "OK");
        assert_eq!(ack.data, r#"{"response":"SUCCESS"}"#);
    }

    #[test]
    fn build_ack_data_contains_response_success() {
        let ack = build_ack("msg_456");
        assert_eq!(ack.code, 200);
        assert_eq!(ack.headers.message_id, "msg_456");
        let data: serde_json::Value = serde_json::from_str(&ack.data).unwrap();
        assert_eq!(data["response"], "SUCCESS");
    }

    // -- build_card_param_map for update (empty text, buttons only) ----------

    #[test]
    fn build_card_param_map_empty_text_with_buttons() {
        use crate::types::ActionButton;
        let buttons = vec![vec![ActionButton {
            label: "Confirm".into(),
            action: "system.confirm".into(),
            params: None,
        }]];
        let map = build_card_param_map("", Some(&buttons));
        assert_eq!(map["content"], "");
        let actions = map["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["label"], "Confirm");
    }

    // -- edit_message: not initialized guard -----------------------------------

    #[tokio::test]
    async fn edit_message_not_initialized_returns_error() {
        let plugin = DingtalkPlugin::new();
        let msg = UnifiedOutgoingMessage {
            message_type: crate::types::OutgoingMessageType::Text,
            text: Some("hello".into()),
            parse_mode: None,
            buttons: None,
            keyboard: None,
            image_url: None,
            file_url: None,
            file_name: None,
            media_actions: None,
            reply_to_message_id: None,
            silent: None,
        };
        let result = plugin.edit_message("chat1", "msg1", msg).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not initialized"), "expected init error: {err}");
    }

    // -- send_message: not initialized guard -----------------------------------

    #[tokio::test]
    async fn send_message_not_initialized_returns_error() {
        let plugin = DingtalkPlugin::new();
        let msg = UnifiedOutgoingMessage {
            message_type: crate::types::OutgoingMessageType::Text,
            text: Some("hello".into()),
            parse_mode: None,
            buttons: None,
            keyboard: None,
            image_url: None,
            file_url: None,
            file_name: None,
            media_actions: None,
            reply_to_message_id: None,
            silent: None,
        };
        let result = plugin.send_message("chat1", msg).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not initialized"), "expected init error: {err}");
    }

    // -- handle_stream_frame: SYSTEM ping -----------------------------------

    #[tokio::test]
    async fn handle_stream_frame_system_ping_returns_ack() {
        let (msg_tx, _msg_rx) = tokio::sync::mpsc::channel(16);
        let (confirm_tx, _confirm_rx) = tokio::sync::mpsc::channel(16);

        let ping_frame = serde_json::json!({
            "type": "SYSTEM",
            "headers": {
                "contentType": "application/json",
                "messageId": "ping_001",
                "topic": "ping"
            },
            "data": "{}"
        });

        let result = handle_stream_frame(
            &ping_frame.to_string(),
            &msg_tx,
            &confirm_tx,
        ).await;

        assert!(result.is_some(), "SYSTEM ping should return an ack");
        let ack = result.unwrap();
        assert_eq!(ack.code, 200);
        assert_eq!(ack.headers.message_id, "ping_001");
    }

    #[tokio::test]
    async fn handle_stream_frame_system_connected_returns_none() {
        let (msg_tx, _msg_rx) = tokio::sync::mpsc::channel(16);
        let (confirm_tx, _confirm_rx) = tokio::sync::mpsc::channel(16);

        let connected_frame = serde_json::json!({
            "type": "SYSTEM",
            "headers": { "topic": "CONNECTED" },
            "data": "{\"code\":200,\"message\":\"OK\"}"
        });

        let result = handle_stream_frame(
            &connected_frame.to_string(),
            &msg_tx,
            &confirm_tx,
        ).await;

        assert!(result.is_none(), "Non-ping SYSTEM frames should not return ack");
    }

    // -- DingtalkPlugin constructor -----------------------------------------

    #[test]
    fn new_plugin_initial_state() {
        let plugin = DingtalkPlugin::new();
        assert_eq!(plugin.status(), PluginStatus::Created);
        assert!(plugin.bot_info().is_none());
        assert!(plugin.last_error().is_none());
        assert_eq!(plugin.plugin_type(), PluginType::Dingtalk);
        assert_eq!(plugin.active_user_count(), 0);
    }
}
