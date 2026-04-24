use std::collections::HashSet;
use std::sync::Arc;

use aionui_ai_agent::AgentStreamEvent;
use aionui_api_types::WebSocketMessage;
use aionui_common::{Confirmation, ConversationStatus, generate_id, now_ms};
use aionui_db::IConversationRepository;
use aionui_db::models::MessageRow;
use aionui_realtime::EventBroadcaster;
use serde_json::json;
use tokio::sync::broadcast;
use tracing::{debug, warn};

/// Number of text chunks to accumulate before flushing to the database.
const FLUSH_INTERVAL: u32 = 20;

/// Relays agent stream events to WebSocket and persists messages.
///
/// This struct is created for each `send_message` call and runs as a
/// background tokio task until the agent finishes or errors out.
pub struct StreamRelay {
    conversation_id: String,
    assistant_msg_id: String,
    repo: Arc<dyn IConversationRepository>,
    broadcaster: Arc<dyn EventBroadcaster>,
}

impl StreamRelay {
    pub fn new(
        conversation_id: String,
        assistant_msg_id: String,
        repo: Arc<dyn IConversationRepository>,
        broadcaster: Arc<dyn EventBroadcaster>,
    ) -> Self {
        Self {
            conversation_id,
            assistant_msg_id,
            repo,
            broadcaster,
        }
    }

    /// Run the relay loop. Consumes `self` and runs until the agent stream ends.
    pub async fn run(self, mut rx: broadcast::Receiver<AgentStreamEvent>) {
        let mut text_buffer = String::new();
        let mut record_created = false;
        let mut flush_counter: u32 = 0;
        let mut seen_confirmations: HashSet<String> = HashSet::new();

        loop {
            match rx.recv().await {
                Ok(event) => {
                    self.forward_to_websocket(&event);
                    self.maybe_broadcast_confirmation(&event, &mut seen_confirmations);

                    if let AgentStreamEvent::Text(ref data) = event {
                        text_buffer.push_str(&data.content);
                        flush_counter += 1;
                        if flush_counter >= FLUSH_INTERVAL {
                            self.flush_text(&text_buffer, &mut record_created).await;
                            flush_counter = 0;
                        }
                    }

                    if self.is_terminal(&event) {
                        self.finalize(&text_buffer, &record_created, &event).await;
                        self.send_turn_completed(&event);
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    // Channel closed without finish/error — still finalize
                    self.finalize(
                        &text_buffer,
                        &record_created,
                        &AgentStreamEvent::Finish(
                            aionui_ai_agent::stream_event::FinishEventData::default(),
                        ),
                    )
                    .await;
                    self.send_turn_completed_status("finished");
                    break;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        conversation_id = %self.conversation_id,
                        lagged = n,
                        "Stream relay lagged, some events dropped"
                    );
                }
            }
        }
    }

    /// Broadcast a confirmation WebSocket event when permission events arrive.
    ///
    /// Sends `confirmation.add` for new confirmations and `confirmation.update`
    /// for confirmations whose ID has already been seen.
    fn maybe_broadcast_confirmation(&self, event: &AgentStreamEvent, seen: &mut HashSet<String>) {
        let data = match event {
            AgentStreamEvent::AcpPermission(d) | AgentStreamEvent::CodexPermission(d) => d,
            _ => return,
        };

        // Try to parse as Confirmation to build a well-typed event payload
        if let Ok(conf) = serde_json::from_value::<Confirmation>(data.clone()) {
            let event_name = if seen.contains(&conf.id) {
                "confirmation.update"
            } else {
                seen.insert(conf.id.clone());
                "confirmation.add"
            };
            let payload = json!({
                "conversation_id": self.conversation_id,
                "id": conf.id,
                "call_id": conf.call_id,
                "title": conf.title,
                "action": conf.action,
                "description": conf.description,
                "command_type": conf.command_type,
                "options": conf.options,
            });
            let msg = WebSocketMessage::new(event_name, payload);
            self.broadcaster.broadcast(msg);
        } else {
            // Fallback: broadcast raw data with conversation ID
            let payload = json!({
                "conversation_id": self.conversation_id,
                "data": data,
            });
            let msg = WebSocketMessage::new("confirmation.add", payload);
            self.broadcaster.broadcast(msg);
        }
    }

    /// Forward an agent event to connected WebSocket clients.
    fn forward_to_websocket(&self, event: &AgentStreamEvent) {
        let event_data = match serde_json::to_value(event) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "Failed to serialize agent event for WebSocket");
                return;
            }
        };

        let payload = json!({
            "conversation_id": self.conversation_id,
            "msg_id": self.assistant_msg_id,
            "type": event_data.get("type").cloned().unwrap_or(json!("unknown")),
            "data": event_data.get("data").cloned().unwrap_or(json!({})),
            "hidden": false,
        });

        let msg = WebSocketMessage::new("message.stream", payload);
        self.broadcaster.broadcast(msg);
    }

    /// Flush accumulated text to the database (create or update).
    async fn flush_text(&self, text: &str, record_created: &mut bool) {
        if text.is_empty() {
            return;
        }

        let content = json!({ "content": text }).to_string();

        if *record_created {
            let update = aionui_db::MessageRowUpdate {
                content: Some(content),
                status: Some(Some("work".into())),
                hidden: None,
            };
            if let Err(e) = self
                .repo
                .update_message(&self.assistant_msg_id, &update)
                .await
            {
                warn!(error = %e, "Failed to update streaming message");
            }
        } else {
            let row = MessageRow {
                id: self.assistant_msg_id.clone(),
                conversation_id: self.conversation_id.clone(),
                msg_id: None,
                r#type: "text".into(),
                content,
                position: Some("left".into()),
                status: Some("work".into()),
                hidden: false,
                created_at: now_ms(),
            };
            if let Err(e) = self.repo.insert_message(&row).await {
                warn!(error = %e, "Failed to create streaming message");
            }
            *record_created = true;
        }
    }

    /// Finalize the assistant message on stream end.
    async fn finalize(&self, text: &str, record_created: &bool, event: &AgentStreamEvent) {
        let status = match event {
            AgentStreamEvent::Error(_) => "error",
            _ => "finish",
        };

        if !text.is_empty() {
            let content = json!({ "content": text }).to_string();
            if *record_created {
                let update = aionui_db::MessageRowUpdate {
                    content: Some(content),
                    status: Some(Some(status.to_owned())),
                    hidden: None,
                };
                if let Err(e) = self
                    .repo
                    .update_message(&self.assistant_msg_id, &update)
                    .await
                {
                    warn!(error = %e, "Failed to finalize streaming message");
                }
            } else {
                let row = MessageRow {
                    id: self.assistant_msg_id.clone(),
                    conversation_id: self.conversation_id.clone(),
                    msg_id: None,
                    r#type: "text".into(),
                    content,
                    position: Some("left".into()),
                    status: Some(status.to_owned()),
                    hidden: false,
                    created_at: now_ms(),
                };
                if let Err(e) = self.repo.insert_message(&row).await {
                    warn!(error = %e, "Failed to create final message");
                }
            }
        } else if let AgentStreamEvent::Error(data) = event {
            // No text accumulated but got an error — store error as tips message
            let content = json!({ "content": data.message, "type": "error" }).to_string();
            let row = MessageRow {
                id: generate_id(),
                conversation_id: self.conversation_id.clone(),
                msg_id: None,
                r#type: "tips".into(),
                content,
                position: Some("left".into()),
                status: Some("error".into()),
                hidden: false,
                created_at: now_ms(),
            };
            if let Err(e) = self.repo.insert_message(&row).await {
                warn!(error = %e, "Failed to store error message");
            }
        }

        // Update conversation status — all terminal events resolve to Finished
        self.update_conversation_status(ConversationStatus::Finished)
            .await;
    }

    /// Send a `turn.completed` WebSocket event.
    fn send_turn_completed(&self, _event: &AgentStreamEvent) {
        // All terminal events resolve to "finished" status
        self.send_turn_completed_status("finished");
    }

    fn send_turn_completed_status(&self, status: &str) {
        let payload = json!({
            "conversation_id": self.conversation_id,
            "status": status,
        });
        let msg = WebSocketMessage::new("turn.completed", payload);
        self.broadcaster.broadcast(msg);

        debug!(
            conversation_id = %self.conversation_id,
            status,
            "Turn completed"
        );
    }

    /// Update the conversation status in the database.
    async fn update_conversation_status(&self, status: ConversationStatus) {
        let status_str = serde_json::to_value(status)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_owned()));

        if let Some(status_str) = status_str {
            let update = aionui_db::ConversationRowUpdate {
                status: Some(status_str),
                updated_at: Some(now_ms()),
                ..Default::default()
            };
            if let Err(e) = self.repo.update(&self.conversation_id, &update).await {
                warn!(error = %e, "Failed to update conversation status");
            }
        }
    }

    fn is_terminal(&self, event: &AgentStreamEvent) -> bool {
        matches!(
            event,
            AgentStreamEvent::Finish(_) | AgentStreamEvent::Error(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_ai_agent::stream_event::{
        ErrorEventData, FinishEventData, StartEventData, TextEventData,
    };
    use aionui_db::DbError;
    use std::sync::Mutex;

    // ── is_terminal tests ─────────────────────────────────────────

    #[test]
    fn is_terminal_finish() {
        let relay = make_relay();
        let event = AgentStreamEvent::Finish(FinishEventData::default());
        assert!(relay.is_terminal(&event));
    }

    #[test]
    fn is_terminal_error() {
        let relay = make_relay();
        let event = AgentStreamEvent::Error(ErrorEventData {
            message: "fail".into(),
            code: None,
        });
        assert!(relay.is_terminal(&event));
    }

    #[test]
    fn is_terminal_text() {
        let relay = make_relay();
        let event = AgentStreamEvent::Text(TextEventData {
            content: "hi".into(),
        });
        assert!(!relay.is_terminal(&event));
    }

    #[test]
    fn is_terminal_start() {
        let relay = make_relay();
        let event = AgentStreamEvent::Start(StartEventData { session_id: None });
        assert!(!relay.is_terminal(&event));
    }

    // ── run() async tests ─────────────────────────────────────────

    #[tokio::test]
    async fn run_text_then_finish_persists_message() {
        let repo = Arc::new(RecordingRepo::new());
        let bus = Arc::new(aionui_realtime::BroadcastEventBus::new(64));
        let (tx, _) = broadcast::channel(64);

        let relay = StreamRelay::new("conv-1".into(), "asst-1".into(), repo.clone(), bus.clone());

        let rx = tx.subscribe();

        // Send text events then finish
        tx.send(AgentStreamEvent::Text(TextEventData {
            content: "Hello ".into(),
        }))
        .unwrap();
        tx.send(AgentStreamEvent::Text(TextEventData {
            content: "World".into(),
        }))
        .unwrap();
        tx.send(AgentStreamEvent::Finish(FinishEventData::default()))
            .unwrap();

        relay.run(rx).await;

        // Should have inserted a message with accumulated text
        let inserts = repo.take_inserts();
        assert_eq!(inserts.len(), 1);
        let msg = &inserts[0];
        assert_eq!(msg.conversation_id, "conv-1");
        assert_eq!(msg.id, "asst-1");
        assert_eq!(msg.r#type, "text");
        assert_eq!(msg.status.as_deref(), Some("finish"));

        let content: serde_json::Value = serde_json::from_str(&msg.content).unwrap();
        assert_eq!(content["content"], "Hello World");
    }

    #[tokio::test]
    async fn run_error_with_no_text_stores_tips_message() {
        let repo = Arc::new(RecordingRepo::new());
        let bus = Arc::new(aionui_realtime::BroadcastEventBus::new(64));
        let (tx, _) = broadcast::channel(64);

        let relay = StreamRelay::new("conv-1".into(), "asst-1".into(), repo.clone(), bus.clone());

        let rx = tx.subscribe();

        tx.send(AgentStreamEvent::Error(ErrorEventData {
            message: "Something went wrong".into(),
            code: None,
        }))
        .unwrap();

        relay.run(rx).await;

        let inserts = repo.take_inserts();
        assert_eq!(inserts.len(), 1);
        let msg = &inserts[0];
        assert_eq!(msg.r#type, "tips");
        assert_eq!(msg.status.as_deref(), Some("error"));

        let content: serde_json::Value = serde_json::from_str(&msg.content).unwrap();
        assert_eq!(content["content"], "Something went wrong");
        assert_eq!(content["type"], "error");
    }

    #[tokio::test]
    async fn run_channel_closed_finalizes() {
        let repo = Arc::new(RecordingRepo::new());
        let bus = Arc::new(aionui_realtime::BroadcastEventBus::new(64));
        let (tx, _) = broadcast::channel(64);

        let relay = StreamRelay::new("conv-1".into(), "asst-1".into(), repo.clone(), bus.clone());

        let rx = tx.subscribe();

        // Send text then drop sender (channel closes without Finish)
        tx.send(AgentStreamEvent::Text(TextEventData {
            content: "partial".into(),
        }))
        .unwrap();
        drop(tx);

        relay.run(rx).await;

        // Should still persist the partial text
        let inserts = repo.take_inserts();
        assert_eq!(inserts.len(), 1);
        let content: serde_json::Value = serde_json::from_str(&inserts[0].content).unwrap();
        assert_eq!(content["content"], "partial");
    }

    #[tokio::test]
    async fn run_broadcasts_turn_completed() {
        let repo = Arc::new(RecordingRepo::new());
        let bus = Arc::new(aionui_realtime::BroadcastEventBus::new(64));
        let (tx, _) = broadcast::channel(64);

        let relay = StreamRelay::new("conv-1".into(), "asst-1".into(), repo.clone(), bus.clone());

        // Subscribe to the bus before relay runs
        let mut ws_rx = bus.subscribe();
        let rx = tx.subscribe();

        tx.send(AgentStreamEvent::Finish(FinishEventData::default()))
            .unwrap();

        relay.run(rx).await;

        // Collect WebSocket events
        let mut ws_events = vec![];
        while let Ok(evt) = ws_rx.try_recv() {
            ws_events.push(evt);
        }

        // Should have turn.completed event
        let turn_event = ws_events.iter().find(|e| e.name == "turn.completed");
        assert!(turn_event.is_some());
        assert_eq!(turn_event.unwrap().data["conversation_id"], "conv-1");
        assert_eq!(turn_event.unwrap().data["status"], "finished");
    }

    // ── Helpers ──────────────────────────────────────────────────

    fn make_relay() -> StreamRelay {
        let bus = Arc::new(aionui_realtime::BroadcastEventBus::new(16));
        StreamRelay {
            conversation_id: "conv-1".into(),
            assistant_msg_id: "msg-1".into(),
            repo: Arc::new(NoopRepo),
            broadcaster: bus,
        }
    }

    /// Noop repo for tests that don't check DB interactions.
    struct NoopRepo;

    #[async_trait::async_trait]
    impl IConversationRepository for NoopRepo {
        async fn get(
            &self,
            _id: &str,
        ) -> Result<Option<aionui_db::models::ConversationRow>, DbError> {
            Ok(None)
        }
        async fn create(&self, _row: &aionui_db::models::ConversationRow) -> Result<(), DbError> {
            Ok(())
        }
        async fn update(
            &self,
            _id: &str,
            _updates: &aionui_db::ConversationRowUpdate,
        ) -> Result<(), DbError> {
            Ok(())
        }
        async fn delete(&self, _id: &str) -> Result<(), DbError> {
            Ok(())
        }
        async fn list_paginated(
            &self,
            _user_id: &str,
            _filters: &aionui_db::ConversationFilters,
        ) -> Result<aionui_common::PaginatedResult<aionui_db::models::ConversationRow>, DbError>
        {
            Ok(aionui_common::PaginatedResult {
                items: vec![],
                total: 0,
                has_more: false,
            })
        }
        async fn find_by_source_and_chat(
            &self,
            _user_id: &str,
            _source: &str,
            _chat_id: &str,
            _agent_type: &str,
        ) -> Result<Option<aionui_db::models::ConversationRow>, DbError> {
            Ok(None)
        }
        async fn list_by_cron_job(
            &self,
            _user_id: &str,
            _cron_job_id: &str,
        ) -> Result<Vec<aionui_db::models::ConversationRow>, DbError> {
            Ok(vec![])
        }
        async fn list_associated(
            &self,
            _user_id: &str,
            _conversation_id: &str,
        ) -> Result<Vec<aionui_db::models::ConversationRow>, DbError> {
            Ok(vec![])
        }
        async fn get_messages(
            &self,
            _conv_id: &str,
            _page: u32,
            _page_size: u32,
            _order: aionui_db::SortOrder,
        ) -> Result<aionui_common::PaginatedResult<MessageRow>, DbError> {
            Ok(aionui_common::PaginatedResult {
                items: vec![],
                total: 0,
                has_more: false,
            })
        }
        async fn insert_message(&self, _row: &MessageRow) -> Result<(), DbError> {
            Ok(())
        }
        async fn update_message(
            &self,
            _id: &str,
            _updates: &aionui_db::MessageRowUpdate,
        ) -> Result<(), DbError> {
            Ok(())
        }
        async fn delete_messages_by_conversation(&self, _conv_id: &str) -> Result<(), DbError> {
            Ok(())
        }
        async fn get_message_by_msg_id(
            &self,
            _conv_id: &str,
            _msg_id: &str,
            _msg_type: &str,
        ) -> Result<Option<MessageRow>, DbError> {
            Ok(None)
        }
        async fn search_messages(
            &self,
            _user_id: &str,
            _keyword: &str,
            _page: u32,
            _page_size: u32,
        ) -> Result<aionui_common::PaginatedResult<aionui_db::MessageSearchRow>, DbError> {
            Ok(aionui_common::PaginatedResult {
                items: vec![],
                total: 0,
                has_more: false,
            })
        }
    }

    /// Recording repo that captures insert/update calls for assertions.
    struct RecordingRepo {
        inserts: Mutex<Vec<MessageRow>>,
        updates: Mutex<Vec<(String, aionui_db::MessageRowUpdate)>>,
    }

    impl RecordingRepo {
        fn new() -> Self {
            Self {
                inserts: Mutex::new(vec![]),
                updates: Mutex::new(vec![]),
            }
        }

        fn take_inserts(&self) -> Vec<MessageRow> {
            std::mem::take(&mut self.inserts.lock().unwrap())
        }

        #[allow(dead_code)]
        fn take_updates(&self) -> Vec<(String, aionui_db::MessageRowUpdate)> {
            std::mem::take(&mut self.updates.lock().unwrap())
        }
    }

    #[async_trait::async_trait]
    impl IConversationRepository for RecordingRepo {
        async fn get(
            &self,
            _id: &str,
        ) -> Result<Option<aionui_db::models::ConversationRow>, DbError> {
            Ok(None)
        }
        async fn create(&self, _row: &aionui_db::models::ConversationRow) -> Result<(), DbError> {
            Ok(())
        }
        async fn update(
            &self,
            _id: &str,
            _updates: &aionui_db::ConversationRowUpdate,
        ) -> Result<(), DbError> {
            Ok(())
        }
        async fn delete(&self, _id: &str) -> Result<(), DbError> {
            Ok(())
        }
        async fn list_paginated(
            &self,
            _user_id: &str,
            _filters: &aionui_db::ConversationFilters,
        ) -> Result<aionui_common::PaginatedResult<aionui_db::models::ConversationRow>, DbError>
        {
            Ok(aionui_common::PaginatedResult {
                items: vec![],
                total: 0,
                has_more: false,
            })
        }
        async fn find_by_source_and_chat(
            &self,
            _user_id: &str,
            _source: &str,
            _chat_id: &str,
            _agent_type: &str,
        ) -> Result<Option<aionui_db::models::ConversationRow>, DbError> {
            Ok(None)
        }
        async fn list_by_cron_job(
            &self,
            _user_id: &str,
            _cron_job_id: &str,
        ) -> Result<Vec<aionui_db::models::ConversationRow>, DbError> {
            Ok(vec![])
        }
        async fn list_associated(
            &self,
            _user_id: &str,
            _conversation_id: &str,
        ) -> Result<Vec<aionui_db::models::ConversationRow>, DbError> {
            Ok(vec![])
        }
        async fn get_messages(
            &self,
            _conv_id: &str,
            _page: u32,
            _page_size: u32,
            _order: aionui_db::SortOrder,
        ) -> Result<aionui_common::PaginatedResult<MessageRow>, DbError> {
            Ok(aionui_common::PaginatedResult {
                items: vec![],
                total: 0,
                has_more: false,
            })
        }
        async fn insert_message(&self, row: &MessageRow) -> Result<(), DbError> {
            self.inserts.lock().unwrap().push(row.clone());
            Ok(())
        }
        async fn update_message(
            &self,
            id: &str,
            updates: &aionui_db::MessageRowUpdate,
        ) -> Result<(), DbError> {
            self.updates
                .lock()
                .unwrap()
                .push((id.to_owned(), updates.clone()));
            Ok(())
        }
        async fn delete_messages_by_conversation(&self, _conv_id: &str) -> Result<(), DbError> {
            Ok(())
        }
        async fn get_message_by_msg_id(
            &self,
            _conv_id: &str,
            _msg_id: &str,
            _msg_type: &str,
        ) -> Result<Option<MessageRow>, DbError> {
            Ok(None)
        }
        async fn search_messages(
            &self,
            _user_id: &str,
            _keyword: &str,
            _page: u32,
            _page_size: u32,
        ) -> Result<aionui_common::PaginatedResult<aionui_db::MessageSearchRow>, DbError> {
            Ok(aionui_common::PaginatedResult {
                items: vec![],
                total: 0,
                has_more: false,
            })
        }
    }
}
