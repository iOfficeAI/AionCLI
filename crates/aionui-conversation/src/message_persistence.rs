use aionui_common::{AppError, ErrorChain, now_ms};
use aionui_db::models::MessageRow;
use tracing::warn;

use crate::service::ConversationService;

impl ConversationService {
    pub(crate) async fn persist_send_failure_tip(&self, conversation_id: &str, err: &AppError) {
        let row = MessageRow {
            id: Self::mint_msg_id(),
            conversation_id: conversation_id.to_owned(),
            msg_id: None,
            r#type: "tips".into(),
            content: serde_json::json!({
                "content": err.to_string(),
                "type": "error",
                "source": "send_failed",
                "code": err.error_code(),
            })
            .to_string(),
            position: Some("center".into()),
            status: Some("error".into()),
            hidden: false,
            created_at: now_ms(),
        };

        if let Err(store_err) = self.conversation_repo().insert_message(&row).await {
            warn!(
                conversation_id,
                error = %ErrorChain(&store_err),
                "Failed to persist send failure error tip"
            );
        }
    }
}
