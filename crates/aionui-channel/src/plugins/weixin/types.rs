use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// iLink Bot API response envelope
// ---------------------------------------------------------------------------

/// Generic iLink Bot API response wrapper.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ILinkResponse<T> {
    pub code: i32,
    #[serde(default)]
    pub msg: Option<String>,
    #[serde(default)]
    pub data: Option<T>,
}

impl<T> ILinkResponse<T> {
    /// Returns `true` if the API call succeeded (code == 0).
    pub fn is_ok(&self) -> bool {
        self.code == 0
    }

    /// Extract the error message, falling back to a generic string.
    pub fn error_message(&self) -> String {
        self.msg
            .clone()
            .unwrap_or_else(|| format!("iLink API error (code={})", self.code))
    }
}

// ---------------------------------------------------------------------------
// QR code login
// ---------------------------------------------------------------------------

/// Response data from `get_bot_qrcode`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct QrCodeData {
    /// QR code ticket string (rendered by the frontend).
    #[serde(default)]
    pub qrcode: Option<String>,
}

/// Response data from `get_qrcode_status`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QrCodeStatusData {
    /// Status: "wait", "scanned", "confirmed", "expired".
    #[serde(default)]
    pub status: Option<String>,
    /// Account ID returned on confirmed status.
    #[serde(default)]
    pub account_id: Option<String>,
    /// Bot token returned on confirmed status.
    #[serde(default)]
    pub bot_token: Option<String>,
    /// Base URL for the iLink Bot API.
    #[serde(default)]
    pub base_url: Option<String>,
}

// ---------------------------------------------------------------------------
// getupdates (long-polling)
// ---------------------------------------------------------------------------

/// A single update from `getupdates`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WxUpdate {
    /// Update ID for offset tracking.
    #[serde(default)]
    pub update_id: i64,
    /// The message payload (present for incoming messages).
    #[serde(default)]
    pub message: Option<WxMessage>,
}

/// An incoming message from the WeChat iLink Bot API.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WxMessage {
    /// Unique message ID.
    #[serde(default)]
    pub message_id: String,
    /// Chat / conversation identifier.
    #[serde(default)]
    pub chat_id: String,
    /// Sender information.
    #[serde(default)]
    pub from: Option<WxUser>,
    /// Unix timestamp of the message.
    #[serde(default)]
    pub date: i64,
    /// Text content (for text messages).
    #[serde(default)]
    pub text: Option<String>,
    /// Message type: "text", "voice", "image", "file", "card".
    #[serde(default, rename = "type")]
    pub msg_type: Option<String>,
    /// File attachment (for file/image/voice messages).
    #[serde(default)]
    pub file: Option<WxFile>,
    /// Card content (for card messages).
    #[serde(default)]
    pub card: Option<WxCard>,
}

/// Sender identity from the WeChat iLink Bot API.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WxUser {
    /// Platform user ID.
    #[serde(default)]
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Avatar URL.
    #[serde(default)]
    pub avatar: Option<String>,
}

/// File attachment in a WeChat message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WxFile {
    /// File ID for download.
    #[serde(default)]
    pub file_id: Option<String>,
    /// Original file name.
    #[serde(default)]
    pub file_name: Option<String>,
    /// MIME type.
    #[serde(default)]
    pub mime_type: Option<String>,
    /// File size in bytes.
    #[serde(default)]
    pub file_size: Option<u64>,
    /// Download URL (may require decryption).
    #[serde(default)]
    pub url: Option<String>,
}

/// Card content in a WeChat message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WxCard {
    /// Card title.
    #[serde(default)]
    pub title: Option<String>,
    /// Card description / body text.
    #[serde(default)]
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// sendmessage
// ---------------------------------------------------------------------------

/// Request body for sending a message via the iLink Bot API.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SendMessageRequest {
    pub chat_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "type")]
    pub msg_type: Option<String>,
}

/// Response data from `sendmessage`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SendMessageData {
    /// The message ID of the sent message.
    #[serde(default)]
    pub message_id: Option<String>,
}

// ---------------------------------------------------------------------------
// SSE event payloads
// ---------------------------------------------------------------------------

/// SSE event for QR code login flow.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SseQrEvent {
    pub qrcode_data: String,
}

/// SSE event for login completion.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SseDoneEvent {
    pub account_id: String,
    pub bot_token: String,
    pub base_url: String,
}

/// SSE event for errors.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SseErrorEvent {
    pub message: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ilink_response_success() {
        let resp: ILinkResponse<String> = ILinkResponse {
            code: 0,
            msg: Some("ok".into()),
            data: Some("result".into()),
        };
        assert!(resp.is_ok());
    }

    #[test]
    fn ilink_response_error() {
        let resp: ILinkResponse<String> = ILinkResponse {
            code: 1001,
            msg: Some("invalid token".into()),
            data: None,
        };
        assert!(!resp.is_ok());
        assert_eq!(resp.error_message(), "invalid token");
    }

    #[test]
    fn ilink_response_error_no_message() {
        let resp: ILinkResponse<()> = ILinkResponse {
            code: 500,
            msg: None,
            data: None,
        };
        assert_eq!(resp.error_message(), "iLink API error (code=500)");
    }

    #[test]
    fn deserialize_qr_code_data() {
        let json = r#"{"code": 0, "data": {"qrcode": "ticket_123"}}"#;
        let resp: ILinkResponse<QrCodeData> = serde_json::from_str(json).unwrap();
        assert!(resp.is_ok());
        assert_eq!(resp.data.unwrap().qrcode.unwrap(), "ticket_123");
    }

    #[test]
    fn deserialize_qr_status_confirmed() {
        let json = r#"{
            "code": 0,
            "data": {
                "status": "confirmed",
                "accountId": "acc_1",
                "botToken": "tok_1",
                "baseUrl": "https://api.ilink.bot"
            }
        }"#;
        let resp: ILinkResponse<QrCodeStatusData> = serde_json::from_str(json).unwrap();
        assert!(resp.is_ok());
        let data = resp.data.unwrap();
        assert_eq!(data.status.as_deref(), Some("confirmed"));
        assert_eq!(data.account_id.as_deref(), Some("acc_1"));
        assert_eq!(data.bot_token.as_deref(), Some("tok_1"));
        assert_eq!(data.base_url.as_deref(), Some("https://api.ilink.bot"));
    }

    #[test]
    fn deserialize_wx_update() {
        let json = r#"{
            "updateId": 42,
            "message": {
                "messageId": "msg_1",
                "chatId": "chat_1",
                "from": { "id": "user_1", "name": "Alice" },
                "date": 1700000000,
                "text": "Hello",
                "type": "text"
            }
        }"#;
        let update: WxUpdate = serde_json::from_str(json).unwrap();
        assert_eq!(update.update_id, 42);
        let msg = update.message.unwrap();
        assert_eq!(msg.message_id, "msg_1");
        assert_eq!(msg.chat_id, "chat_1");
        assert_eq!(msg.text.as_deref(), Some("Hello"));
        assert_eq!(msg.msg_type.as_deref(), Some("text"));
        let from = msg.from.unwrap();
        assert_eq!(from.id, "user_1");
        assert_eq!(from.name.as_deref(), Some("Alice"));
    }

    #[test]
    fn deserialize_wx_update_with_file() {
        let json = r#"{
            "updateId": 43,
            "message": {
                "messageId": "msg_2",
                "chatId": "chat_1",
                "date": 1700000001,
                "type": "file",
                "file": {
                    "fileId": "f_1",
                    "fileName": "report.pdf",
                    "mimeType": "application/pdf",
                    "fileSize": 1024,
                    "url": "https://example.com/f_1"
                }
            }
        }"#;
        let update: WxUpdate = serde_json::from_str(json).unwrap();
        let msg = update.message.unwrap();
        assert_eq!(msg.msg_type.as_deref(), Some("file"));
        let file = msg.file.unwrap();
        assert_eq!(file.file_id.as_deref(), Some("f_1"));
        assert_eq!(file.file_name.as_deref(), Some("report.pdf"));
        assert_eq!(file.file_size, Some(1024));
    }

    #[test]
    fn deserialize_wx_update_with_card() {
        let json = r#"{
            "updateId": 44,
            "message": {
                "messageId": "msg_3",
                "chatId": "chat_1",
                "date": 1700000002,
                "type": "card",
                "card": {
                    "title": "Alert",
                    "description": "Something happened"
                }
            }
        }"#;
        let update: WxUpdate = serde_json::from_str(json).unwrap();
        let msg = update.message.unwrap();
        let card = msg.card.unwrap();
        assert_eq!(card.title.as_deref(), Some("Alert"));
        assert_eq!(card.description.as_deref(), Some("Something happened"));
    }

    #[test]
    fn serialize_send_message_request() {
        let req = SendMessageRequest {
            chat_id: "chat_1".into(),
            text: Some("Hello".into()),
            msg_type: Some("text".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""chatId":"chat_1"#));
        assert!(json.contains(r#""text":"Hello"#));
        assert!(json.contains(r#""type":"text"#));
    }

    #[test]
    fn serialize_sse_qr_event() {
        let evt = SseQrEvent {
            qrcode_data: "ticket_abc".into(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(json.contains(r#""qrcodeData":"ticket_abc"#));
    }

    #[test]
    fn serialize_sse_done_event() {
        let evt = SseDoneEvent {
            account_id: "acc_1".into(),
            bot_token: "tok_1".into(),
            base_url: "https://api.example.com".into(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(json.contains(r#""accountId":"acc_1"#));
        assert!(json.contains(r#""botToken":"tok_1"#));
        assert!(json.contains(r#""baseUrl":"https://api.example.com"#));
    }

    #[test]
    fn serialize_sse_error_event() {
        let evt = SseErrorEvent {
            message: "timeout".into(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(json.contains(r#""message":"timeout"#));
    }
}
