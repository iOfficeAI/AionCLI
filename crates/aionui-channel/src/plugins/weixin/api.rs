use reqwest::Client;
use tracing::{debug, warn};

use crate::error::ChannelError;

use super::types::{
    ILinkResponse, QrCodeData, QrCodeStatusData, SendMessageData, SendMessageRequest, WxUpdate,
};

/// HTTP client for the WeChat iLink Bot API.
///
/// Provides typed methods for QR code login, long-polling updates,
/// and message sending.
pub(crate) struct WeixinApi {
    client: Client,
    base_url: String,
    bot_token: String,
}

impl WeixinApi {
    /// Create a new WeChat iLink Bot API client.
    pub fn new(client: Client, base_url: &str, bot_token: &str) -> Self {
        // Normalize: strip trailing slash
        let base = base_url.trim_end_matches('/');
        Self {
            client,
            base_url: base.to_string(),
            bot_token: bot_token.to_string(),
        }
    }

    /// Get bot token (for tests / debug).
    #[cfg(test)]
    pub fn bot_token(&self) -> &str {
        &self.bot_token
    }

    // -----------------------------------------------------------------------
    // QR code login
    // -----------------------------------------------------------------------

    /// Fetch a QR code for bot login.
    ///
    /// `GET /ilink/bot/get_bot_qrcode?bot_type=3`
    pub async fn get_bot_qrcode(&self) -> Result<QrCodeData, ChannelError> {
        let url = format!("{}/ilink/bot/get_bot_qrcode", self.base_url);
        debug!("Fetching WeChat QR code");

        let resp: ILinkResponse<QrCodeData> = self
            .client
            .get(&url)
            .query(&[("bot_type", "3")])
            .send()
            .await
            .map_err(|e| {
                ChannelError::PlatformApi(format!("get_bot_qrcode request failed: {e}"))
            })?
            .json()
            .await
            .map_err(|e| {
                ChannelError::PlatformApi(format!("get_bot_qrcode parse failed: {e}"))
            })?;

        if !resp.is_ok() {
            return Err(ChannelError::PlatformApi(format!(
                "get_bot_qrcode failed: {}",
                resp.error_message()
            )));
        }

        resp.data.ok_or_else(|| {
            ChannelError::PlatformApi("get_bot_qrcode returned no data".into())
        })
    }

    /// Check the status of a QR code scan.
    ///
    /// `GET /ilink/bot/get_qrcode_status?qrcode=<ticket>`
    pub async fn get_qrcode_status(
        &self,
        qrcode: &str,
    ) -> Result<QrCodeStatusData, ChannelError> {
        let url = format!("{}/ilink/bot/get_qrcode_status", self.base_url);

        let resp: ILinkResponse<QrCodeStatusData> = self
            .client
            .get(&url)
            .query(&[("qrcode", qrcode)])
            .send()
            .await
            .map_err(|e| {
                ChannelError::PlatformApi(format!("get_qrcode_status request failed: {e}"))
            })?
            .json()
            .await
            .map_err(|e| {
                ChannelError::PlatformApi(format!("get_qrcode_status parse failed: {e}"))
            })?;

        if !resp.is_ok() {
            return Err(ChannelError::PlatformApi(format!(
                "get_qrcode_status failed: {}",
                resp.error_message()
            )));
        }

        resp.data.ok_or_else(|| {
            ChannelError::PlatformApi("get_qrcode_status returned no data".into())
        })
    }

    // -----------------------------------------------------------------------
    // Long-polling
    // -----------------------------------------------------------------------

    /// Long-poll for new updates.
    ///
    /// `POST /ilink/bot/getupdates`
    ///
    /// - `offset`: return updates with update_id >= offset
    /// - `timeout`: long-polling timeout in seconds
    pub async fn get_updates(
        &self,
        offset: Option<i64>,
        timeout: u32,
    ) -> Result<Vec<WxUpdate>, ChannelError> {
        let url = format!("{}/ilink/bot/getupdates", self.base_url);

        let mut body = serde_json::json!({
            "botToken": self.bot_token,
            "timeout": timeout,
        });

        if let Some(off) = offset {
            body["offset"] = serde_json::json!(off);
        }

        let resp: ILinkResponse<Vec<WxUpdate>> = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                ChannelError::PlatformApi(format!("getupdates request failed: {e}"))
            })?
            .json()
            .await
            .map_err(|e| {
                ChannelError::PlatformApi(format!("getupdates parse failed: {e}"))
            })?;

        if !resp.is_ok() {
            let msg = resp.error_message();
            warn!("WeChat getupdates error: {msg}");
            return Err(ChannelError::PlatformApi(format!(
                "getupdates failed: {msg}"
            )));
        }

        Ok(resp.data.unwrap_or_default())
    }

    // -----------------------------------------------------------------------
    // Send message
    // -----------------------------------------------------------------------

    /// Send a text message.
    ///
    /// `POST /ilink/bot/sendmessage`
    pub async fn send_message(
        &self,
        req: &SendMessageRequest,
    ) -> Result<SendMessageData, ChannelError> {
        let url = format!("{}/ilink/bot/sendmessage", self.base_url);
        debug!(chat_id = %req.chat_id, "Sending WeChat message");

        let mut body = serde_json::to_value(req).map_err(ChannelError::Json)?;
        // Inject bot token into the request body
        body["botToken"] = serde_json::json!(self.bot_token);

        let resp: ILinkResponse<SendMessageData> = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                ChannelError::MessageSendFailed(format!("sendmessage request failed: {e}"))
            })?
            .json()
            .await
            .map_err(|e| {
                ChannelError::MessageSendFailed(format!("sendmessage parse failed: {e}"))
            })?;

        if !resp.is_ok() {
            return Err(ChannelError::MessageSendFailed(format!(
                "sendmessage failed: {}",
                resp.error_message()
            )));
        }

        resp.data.ok_or_else(|| {
            ChannelError::MessageSendFailed("sendmessage returned no data".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_stores_credentials() {
        let client = Client::new();
        let api = WeixinApi::new(client, "https://api.example.com/", "tok_abc");
        assert_eq!(api.base_url, "https://api.example.com");
        assert_eq!(api.bot_token(), "tok_abc");
    }

    #[test]
    fn api_normalizes_trailing_slash() {
        let client = Client::new();
        let api = WeixinApi::new(client, "https://api.example.com///", "tok");
        // strips only trailing slashes
        assert!(api.base_url.ends_with("com"));
    }
}
