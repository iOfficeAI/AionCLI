use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::constants::{WEIXIN_MAX_RETRIES, WEIXIN_RETRY_DELAY};
use crate::error::ChannelError;
use crate::plugin::{ChannelPlugin, PluginCallbacks};
use crate::types::{
    BotInfo, MessageContentType, PluginConfig, PluginStatus, PluginType,
    UnifiedAttachment, UnifiedIncomingMessage, UnifiedMessageContent,
    UnifiedOutgoingMessage, UnifiedUser,
};

use super::api::WeixinApi;
use super::types::{SendMessageRequest, WxMessage};

/// iLink Bot long-polling timeout in seconds.
const POLL_TIMEOUT: u32 = 25;

/// Default base URL for the iLink Bot API.
const DEFAULT_BASE_URL: &str = "https://api.ilink.bot";

/// WeChat (iLink Bot) platform plugin.
///
/// Connects via long-polling (`getupdates`), handles text/voice/image/
/// file/card messages. Does not support editing messages (WeChat
/// limitation); `edit_message` sends a new reply instead.
pub struct WeixinPlugin {
    status: PluginStatus,
    bot_info: Option<BotInfo>,
    last_error: Option<String>,
    api: Option<Arc<WeixinApi>>,
    poll_handle: Option<JoinHandle<()>>,
    shutdown_tx: Option<watch::Sender<bool>>,
}

impl Default for WeixinPlugin {
    fn default() -> Self {
        Self {
            status: PluginStatus::Created,
            bot_info: None,
            last_error: None,
            api: None,
            poll_handle: None,
            shutdown_tx: None,
        }
    }
}

impl WeixinPlugin {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl ChannelPlugin for WeixinPlugin {
    async fn initialize(
        &mut self,
        config: PluginConfig,
        callbacks: PluginCallbacks,
    ) -> Result<(), ChannelError> {
        self.status = PluginStatus::Initializing;

        let bot_token = config
            .credentials
            .bot_token
            .as_deref()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                self.status = PluginStatus::Error;
                self.last_error = Some("Missing WeChat bot_token".into());
                ChannelError::InvalidConfig("Missing WeChat bot_token".into())
            })?;

        let account_id = config
            .credentials
            .account_id
            .as_deref()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| {
                self.status = PluginStatus::Error;
                self.last_error = Some("Missing WeChat account_id".into());
                ChannelError::InvalidConfig("Missing WeChat account_id".into())
            })?;

        // Use base URL from extra config, or the default iLink Bot URL.
        let base_url = config
            .credentials
            .extra
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_BASE_URL);

        let http_client = Client::builder()
            .timeout(Duration::from_secs(POLL_TIMEOUT as u64 + 10))
            .build()
            .map_err(|e| {
                self.status = PluginStatus::Error;
                self.last_error = Some(format!("HTTP client init failed: {e}"));
                ChannelError::ConnectionFailed(format!("HTTP client init failed: {e}"))
            })?;

        let api = Arc::new(WeixinApi::new(http_client, base_url, bot_token));

        self.bot_info = Some(BotInfo {
            id: account_id.to_string(),
            username: None,
            display_name: format!("WeChat Bot ({account_id})"),
        });

        info!(account_id, "WeChat bot initialized");

        self.api = Some(api);

        // Set up shutdown channel and spawn the long-polling task
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        self.shutdown_tx = Some(shutdown_tx);

        let api_clone = Arc::clone(self.api.as_ref().expect("api just set"));
        self.poll_handle = Some(tokio::spawn(poll_loop(
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
        // Polling task was spawned in initialize; start just transitions status.
        self.status = PluginStatus::Running;
        info!("WeChat plugin started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), ChannelError> {
        self.status = PluginStatus::Stopping;

        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }

        if let Some(handle) = self.poll_handle.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }

        self.api = None;
        self.status = PluginStatus::Stopped;
        info!("WeChat plugin stopped");
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        message: UnifiedOutgoingMessage,
    ) -> Result<String, ChannelError> {
        let api = self
            .api
            .as_ref()
            .ok_or_else(|| ChannelError::PlatformApi("Plugin not initialized".into()))?;

        let text = message.text.as_deref().unwrap_or("").to_string();

        let req = SendMessageRequest {
            chat_id: chat_id.to_string(),
            text: Some(text),
            msg_type: Some("text".into()),
        };

        let data = api.send_message(&req).await?;
        Ok(data.message_id.unwrap_or_default())
    }

    /// WeChat does not support editing messages.
    /// Fallback: send a new message as a reply.
    async fn edit_message(
        &self,
        chat_id: &str,
        _message_id: &str,
        message: UnifiedOutgoingMessage,
    ) -> Result<(), ChannelError> {
        let _ = self.send_message(chat_id, message).await?;
        Ok(())
    }

    fn active_user_count(&self) -> usize {
        // Tracked externally by ChannelManager via SessionManager
        0
    }

    fn bot_info(&self) -> Option<&BotInfo> {
        self.bot_info.as_ref()
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Weixin
    }

    fn status(&self) -> PluginStatus {
        self.status
    }

    fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

// ---------------------------------------------------------------------------
// Long-polling loop
// ---------------------------------------------------------------------------

/// Background task that continuously polls iLink Bot for updates.
///
/// Retries on error up to `WEIXIN_MAX_RETRIES` consecutive failures
/// with `WEIXIN_RETRY_DELAY` between attempts.
async fn poll_loop(
    api: Arc<WeixinApi>,
    message_tx: tokio::sync::mpsc::Sender<UnifiedIncomingMessage>,
    _confirm_tx: tokio::sync::mpsc::Sender<(String, String)>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut offset: Option<i64> = None;
    let mut consecutive_errors: u32 = 0;

    loop {
        if *shutdown_rx.borrow() {
            debug!("WeChat poll loop received shutdown signal");
            break;
        }

        match api.get_updates(offset, POLL_TIMEOUT).await {
            Ok(updates) => {
                consecutive_errors = 0;

                for update in updates {
                    offset = Some(update.update_id + 1);

                    if let Some(msg) = update.message {
                        handle_message(&msg, &message_tx).await;
                    }
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                warn!(
                    error = %e,
                    consecutive_errors,
                    "WeChat poll error"
                );

                if consecutive_errors >= WEIXIN_MAX_RETRIES {
                    error!("WeChat max retry attempts reached, stopping poll loop");
                    break;
                }

                tokio::select! {
                    _ = tokio::time::sleep(WEIXIN_RETRY_DELAY) => {}
                    _ = shutdown_rx.changed() => {
                        debug!("WeChat poll loop shutdown during backoff");
                        break;
                    }
                }
            }
        }
    }

    debug!("WeChat poll loop exited");
}

// ---------------------------------------------------------------------------
// Message handling
// ---------------------------------------------------------------------------

/// Convert a WeChat message into a `UnifiedIncomingMessage` and forward it.
async fn handle_message(
    msg: &WxMessage,
    message_tx: &tokio::sync::mpsc::Sender<UnifiedIncomingMessage>,
) {
    let from = match &msg.from {
        Some(u) => u,
        None => return, // system messages without a sender
    };

    let user = UnifiedUser {
        id: from.id.clone(),
        username: None,
        display_name: from.name.clone().unwrap_or_else(|| from.id.clone()),
        avatar_url: from.avatar.clone(),
    };

    let (content_type, text, attachments) = extract_content(msg);

    let unified = UnifiedIncomingMessage {
        id: msg.message_id.clone(),
        platform: PluginType::Weixin,
        chat_id: msg.chat_id.clone(),
        user,
        content: UnifiedMessageContent {
            content_type,
            text,
            attachments,
        },
        timestamp: msg.date,
        reply_to_message_id: None,
        action: None,
        raw: None,
    };

    let _ = message_tx.send(unified).await;
}

/// Extract content type, text, and attachments from a WeChat message.
fn extract_content(
    msg: &WxMessage,
) -> (MessageContentType, String, Option<Vec<UnifiedAttachment>>) {
    let msg_type = msg.msg_type.as_deref().unwrap_or("text");

    match msg_type {
        "image" => {
            let attachments = msg.file.as_ref().map(|f| {
                vec![UnifiedAttachment {
                    file_id: f.file_id.clone(),
                    file_name: f.file_name.clone(),
                    mime_type: f.mime_type.clone().or(Some("image/jpeg".into())),
                    file_size: f.file_size,
                    url: f.url.clone(),
                }]
            });
            let text = msg.text.clone().unwrap_or_default();
            (MessageContentType::Photo, text, attachments)
        }
        "voice" => {
            let attachments = msg.file.as_ref().map(|f| {
                vec![UnifiedAttachment {
                    file_id: f.file_id.clone(),
                    file_name: f.file_name.clone(),
                    mime_type: f.mime_type.clone(),
                    file_size: f.file_size,
                    url: f.url.clone(),
                }]
            });
            let text = msg.text.clone().unwrap_or_default();
            (MessageContentType::Voice, text, attachments)
        }
        "file" => {
            let attachments = msg.file.as_ref().map(|f| {
                vec![UnifiedAttachment {
                    file_id: f.file_id.clone(),
                    file_name: f.file_name.clone(),
                    mime_type: f.mime_type.clone(),
                    file_size: f.file_size,
                    url: f.url.clone(),
                }]
            });
            let text = msg.text.clone().unwrap_or_default();
            (MessageContentType::Document, text, attachments)
        }
        "card" => {
            let text = msg
                .card
                .as_ref()
                .and_then(|c| c.description.clone())
                .or_else(|| msg.card.as_ref().and_then(|c| c.title.clone()))
                .unwrap_or_default();
            (MessageContentType::Text, text, None)
        }
        // Default: text
        _ => {
            let text = msg.text.clone().unwrap_or_default();
            if text.starts_with('/') {
                return (MessageContentType::Command, text, None);
            }
            (MessageContentType::Text, text, None)
        }
    }
}

/// Current unix timestamp in seconds.
fn _chrono_now() -> i64 {
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
    use crate::types::PluginCredentials;
    use std::collections::HashMap;

    // -- extract_content -------------------------------------------------------

    #[test]
    fn extract_text_message() {
        let msg = make_wx_message("text", Some("Hello world"), None, None);
        let (ct, text, att) = extract_content(&msg);
        assert_eq!(ct, MessageContentType::Text);
        assert_eq!(text, "Hello world");
        assert!(att.is_none());
    }

    #[test]
    fn extract_command_message() {
        let msg = make_wx_message("text", Some("/start"), None, None);
        let (ct, text, _) = extract_content(&msg);
        assert_eq!(ct, MessageContentType::Command);
        assert_eq!(text, "/start");
    }

    #[test]
    fn extract_image_message() {
        let file = super::super::types::WxFile {
            file_id: Some("img_1".into()),
            file_name: Some("photo.jpg".into()),
            mime_type: Some("image/jpeg".into()),
            file_size: Some(5000),
            url: Some("https://example.com/img_1".into()),
        };
        let msg = make_wx_message("image", None, Some(file), None);
        let (ct, _, att) = extract_content(&msg);
        assert_eq!(ct, MessageContentType::Photo);
        let atts = att.unwrap();
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].file_id.as_deref(), Some("img_1"));
    }

    #[test]
    fn extract_voice_message() {
        let file = super::super::types::WxFile {
            file_id: Some("voice_1".into()),
            file_name: None,
            mime_type: Some("audio/amr".into()),
            file_size: Some(3000),
            url: None,
        };
        let msg = make_wx_message("voice", None, Some(file), None);
        let (ct, _, att) = extract_content(&msg);
        assert_eq!(ct, MessageContentType::Voice);
        assert!(att.is_some());
    }

    #[test]
    fn extract_file_message() {
        let file = super::super::types::WxFile {
            file_id: Some("doc_1".into()),
            file_name: Some("report.pdf".into()),
            mime_type: Some("application/pdf".into()),
            file_size: Some(10240),
            url: None,
        };
        let msg = make_wx_message("file", None, Some(file), None);
        let (ct, _, att) = extract_content(&msg);
        assert_eq!(ct, MessageContentType::Document);
        let atts = att.unwrap();
        assert_eq!(atts[0].file_name.as_deref(), Some("report.pdf"));
    }

    #[test]
    fn extract_card_message() {
        let card = super::super::types::WxCard {
            title: Some("Alert".into()),
            description: Some("Something happened".into()),
        };
        let msg = make_wx_message("card", None, None, Some(card));
        let (ct, text, _) = extract_content(&msg);
        assert_eq!(ct, MessageContentType::Text);
        assert_eq!(text, "Something happened");
    }

    #[test]
    fn extract_card_message_fallback_to_title() {
        let card = super::super::types::WxCard {
            title: Some("Alert Title".into()),
            description: None,
        };
        let msg = make_wx_message("card", None, None, Some(card));
        let (_, text, _) = extract_content(&msg);
        assert_eq!(text, "Alert Title");
    }

    #[test]
    fn extract_unknown_type_defaults_to_text() {
        let msg = make_wx_message("unknown_type", Some("data"), None, None);
        let (ct, text, _) = extract_content(&msg);
        assert_eq!(ct, MessageContentType::Text);
        assert_eq!(text, "data");
    }

    #[test]
    fn extract_image_without_file_has_no_attachments() {
        let msg = make_wx_message("image", None, None, None);
        let (ct, _, att) = extract_content(&msg);
        assert_eq!(ct, MessageContentType::Photo);
        assert!(att.is_none());
    }

    // -- WeixinPlugin constructor -----------------------------------------------

    #[test]
    fn new_plugin_initial_state() {
        let plugin = WeixinPlugin::new();
        assert_eq!(plugin.status(), PluginStatus::Created);
        assert!(plugin.bot_info().is_none());
        assert!(plugin.last_error().is_none());
        assert_eq!(plugin.plugin_type(), PluginType::Weixin);
        assert_eq!(plugin.active_user_count(), 0);
    }

    // -- initialize validation --------------------------------------------------

    #[tokio::test]
    async fn initialize_missing_bot_token_fails() {
        let mut plugin = WeixinPlugin::new();
        let config = make_config(None, Some("acc_1"));
        let callbacks = make_callbacks();
        let result = plugin.initialize(config, callbacks).await;
        assert!(result.is_err());
        assert_eq!(plugin.status(), PluginStatus::Error);
        assert_eq!(plugin.last_error(), Some("Missing WeChat bot_token"));
    }

    #[tokio::test]
    async fn initialize_missing_account_id_fails() {
        let mut plugin = WeixinPlugin::new();
        let config = make_config(Some("tok_1"), None);
        let callbacks = make_callbacks();
        let result = plugin.initialize(config, callbacks).await;
        assert!(result.is_err());
        assert_eq!(plugin.status(), PluginStatus::Error);
        assert_eq!(plugin.last_error(), Some("Missing WeChat account_id"));
    }

    #[tokio::test]
    async fn initialize_empty_bot_token_fails() {
        let mut plugin = WeixinPlugin::new();
        let config = make_config(Some(""), Some("acc_1"));
        let callbacks = make_callbacks();
        let result = plugin.initialize(config, callbacks).await;
        assert!(result.is_err());
        assert_eq!(plugin.status(), PluginStatus::Error);
    }

    // -- Test helpers -----------------------------------------------------------

    fn make_wx_message(
        msg_type: &str,
        text: Option<&str>,
        file: Option<super::super::types::WxFile>,
        card: Option<super::super::types::WxCard>,
    ) -> WxMessage {
        WxMessage {
            message_id: "msg_test".into(),
            chat_id: "chat_test".into(),
            from: Some(super::super::types::WxUser {
                id: "user_1".into(),
                name: Some("TestUser".into()),
                avatar: None,
            }),
            date: 1700000000,
            text: text.map(String::from),
            msg_type: Some(msg_type.into()),
            file,
            card,
        }
    }

    fn make_config(bot_token: Option<&str>, account_id: Option<&str>) -> PluginConfig {
        PluginConfig {
            credentials: PluginCredentials {
                token: None,
                app_id: None,
                app_secret: None,
                encrypt_key: None,
                verification_token: None,
                client_id: None,
                client_secret: None,
                account_id: account_id.map(String::from),
                bot_token: bot_token.map(String::from),
                extra: HashMap::new(),
            },
            config: None,
        }
    }

    fn make_callbacks() -> PluginCallbacks {
        let (message_tx, _) = tokio::sync::mpsc::channel(16);
        let (confirm_tx, _) = tokio::sync::mpsc::channel(16);
        PluginCallbacks {
            message_tx,
            confirm_tx,
        }
    }
}
