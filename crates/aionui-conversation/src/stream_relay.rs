use std::sync::Arc;

use aionui_ai_agent::{
    AgentStreamEvent, ICronService, MessageMiddleware, MiddlewareResult, stream_event::ThinkingEventData,
};
use aionui_api_types::WebSocketMessage;
use aionui_common::{normalize_keys_to_snake_case, now_ms};

use crate::service::ConversationService;
use aionui_db::IConversationRepository;
use aionui_db::models::MessageRow;
use aionui_realtime::EventBroadcaster;
use serde_json::json;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

/// Number of text chunks to accumulate before flushing to the database.
const FLUSH_INTERVAL: u32 = 20;

/// Result returned after a relay turn has fully drained and finalized.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelayOutcome {
    pub system_responses: Vec<String>,
}

/// Relays agent stream events to WebSocket and persists messages.
///
/// This struct is created for each `send_message` call and runs as a
/// background tokio task until the agent finishes or errors out.
pub struct StreamRelay {
    conversation_id: String,
    msg_id: String,
    user_id: String,
    repo: Arc<dyn IConversationRepository>,
    broadcaster: Arc<dyn EventBroadcaster>,
    cron_service: Option<Arc<dyn ICronService>>,
    complete_turn: bool,
}

impl StreamRelay {
    pub fn new(
        conversation_id: String,
        msg_id: String,
        user_id: String,
        repo: Arc<dyn IConversationRepository>,
        broadcaster: Arc<dyn EventBroadcaster>,
        cron_service: Option<Arc<dyn ICronService>>,
    ) -> Self {
        Self {
            conversation_id,
            msg_id,
            user_id,
            repo,
            broadcaster,
            cron_service,
            complete_turn: true,
        }
    }

    pub fn with_turn_completion(mut self, enabled: bool) -> Self {
        self.complete_turn = enabled;
        self
    }

    /// Run the relay loop. Consumes `self` and runs until the agent stream ends.
    pub async fn consume(self, mut rx: broadcast::Receiver<AgentStreamEvent>) -> RelayOutcome {
        let started_at = now_ms();
        info!(
            conversation_id = %self.conversation_id,
            msg_id = %self.msg_id,
            "StreamRelay started"
        );

        let mut text_buffer = String::new();
        let mut thinking_buffer = String::new();
        let mut thinking_started_at: Option<i64> = None;
        let mut record_created = false;
        let mut flush_counter: u32 = 0;
        let mut has_thinking = false;

        loop {
            match rx.recv().await {
                Ok(event) => {
                    match &event {
                        AgentStreamEvent::Thinking(data) => {
                            has_thinking = true;
                            if data.status.as_deref() != Some("done") {
                                if thinking_started_at.is_none() {
                                    thinking_started_at = Some(now_ms());
                                }
                                thinking_buffer.push_str(&data.content);
                            }
                            self.forward_to_websocket(&event);
                        }
                        AgentStreamEvent::Text(data) => {
                            self.forward_to_websocket(&event);
                            text_buffer.push_str(&data.content);
                            flush_counter += 1;
                            if flush_counter >= FLUSH_INTERVAL {
                                self.flush_text(&text_buffer, &mut record_created).await;
                                flush_counter = 0;
                            }
                        }
                        AgentStreamEvent::Finish(_) | AgentStreamEvent::Error(_) => {
                            let elapsed_ms = now_ms() - started_at;
                            let event_type = if matches!(event, AgentStreamEvent::Finish(_)) {
                                "Finish"
                            } else {
                                "Error"
                            };
                            info!(
                                conversation_id = %self.conversation_id,
                                event_type,
                                elapsed_ms,
                                text_len = text_buffer.len(),
                                "StreamRelay received terminal event"
                            );
                            // Send thinking_done BEFORE the terminal event so the
                            // frontend receives it while still in "running" state.
                            // Otherwise the thinking_done arriving after finish
                            // re-activates the processing indicator.
                            if has_thinking {
                                self.send_thinking_done();
                            }
                            self.forward_to_websocket(&event);
                            self.persist_thinking(&thinking_buffer, thinking_started_at).await;
                            let outcome = self.finalize(&text_buffer, &record_created, &event).await;
                            if self.complete_turn {
                                Self::complete_conversation(&self.repo, &self.broadcaster, &self.conversation_id).await;
                            }
                            break outcome;
                        }
                        _ => {
                            self.forward_to_websocket(&event);
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    let elapsed_ms = now_ms() - started_at;
                    warn!(
                        conversation_id = %self.conversation_id,
                        elapsed_ms,
                        text_len = text_buffer.len(),
                        "StreamRelay channel closed without terminal event"
                    );
                    if has_thinking {
                        self.send_thinking_done();
                    }
                    self.persist_thinking(&thinking_buffer, thinking_started_at).await;
                    // Channel closed without finish/error — still finalize
                    let outcome = self
                        .finalize(
                            &text_buffer,
                            &record_created,
                            &AgentStreamEvent::Finish(aionui_ai_agent::stream_event::FinishEventData::default()),
                        )
                        .await;
                    if self.complete_turn {
                        Self::complete_conversation(&self.repo, &self.broadcaster, &self.conversation_id).await;
                    }
                    break outcome;
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

    /// Forward an agent event to connected WebSocket clients.
    fn forward_to_websocket(&self, event: &AgentStreamEvent) {
        let mut event_data = match serde_json::to_value(event) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "Failed to serialize agent event for WebSocket");
                return;
            }
        };
        // Nested ACP SDK payloads serialise as camelCase on their own;
        // force every object key down the tree to snake_case so the
        // wire contract stays uniform.
        normalize_keys_to_snake_case(&mut event_data);

        let payload = json!({
            "conversation_id": self.conversation_id,
            "msg_id": self.msg_id,
            "type": event_data.get("type").cloned().unwrap_or(json!("unknown")),
            "data": event_data.get("data").cloned().unwrap_or(json!({})),
            "hidden": false,
        });

        self.broadcast_stream_payload(payload);
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
            if let Err(e) = self.repo.update_message(&self.msg_id, &update).await {
                error!(error = %e, "Failed to update streaming message");
            }
        } else {
            // `id` and `msg_id` share the same value: primary key is the
            // legacy contract, while `msg_id` is what the WebSocket stream
            // and frontend message index use to correlate chunks to the
            // persisted row. Keeping them equal avoids a schema migration.
            let row = MessageRow {
                id: self.msg_id.clone(),
                conversation_id: self.conversation_id.clone(),
                msg_id: Some(self.msg_id.clone()),
                r#type: "text".into(),
                content,
                position: Some("left".into()),
                status: Some("work".into()),
                hidden: false,
                created_at: now_ms(),
            };
            if let Err(e) = self.repo.insert_message(&row).await {
                error!(error = %e, "Failed to create streaming message");
            }
            *record_created = true;
        }
    }

    /// Finalize the assistant message on stream end.
    async fn finalize(&self, text: &str, record_created: &bool, event: &AgentStreamEvent) -> RelayOutcome {
        let mut outcome = RelayOutcome::default();
        let status = match event {
            AgentStreamEvent::Error(_) => "error",
            _ => "finish",
        };

        if !text.is_empty() {
            let processed = self.process_final_text(text).await;
            let final_text = processed.message.trim().to_owned();
            let hidden = final_text.is_empty();
            let content = json!({ "content": final_text }).to_string();

            if *record_created || !hidden {
                if *record_created {
                    let update = aionui_db::MessageRowUpdate {
                        content: Some(content),
                        status: Some(Some(status.to_owned())),
                        hidden: Some(hidden),
                    };
                    if let Err(e) = self.repo.update_message(&self.msg_id, &update).await {
                        error!(error = %e, "Failed to finalize streaming message");
                    }
                } else {
                    let row = MessageRow {
                        id: self.msg_id.clone(),
                        conversation_id: self.conversation_id.clone(),
                        msg_id: Some(self.msg_id.clone()),
                        r#type: "text".into(),
                        content,
                        position: Some("left".into()),
                        status: Some(status.to_owned()),
                        hidden,
                        created_at: now_ms(),
                    };
                    if let Err(e) = self.repo.insert_message(&row).await {
                        error!(error = %e, "Failed to create final message");
                    }
                }

                if processed.message != text || hidden {
                    self.send_final_text_override(&processed.message, hidden);
                }
            }

            self.send_system_responses(&processed.system_responses);
            outcome.system_responses = processed.system_responses;
        } else if let AgentStreamEvent::Error(data) = event {
            // No text accumulated but got an error — store error as tips message
            let content = json!({ "content": data.message, "type": "error" }).to_string();
            let row = MessageRow {
                id: ConversationService::mint_msg_id(),
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
                error!(error = %e, "Failed to store error message");
            }
        }

        outcome
    }

    /// Persist accumulated thinking content as a message in the database.
    /// This ensures thinking blocks survive page refreshes.
    async fn persist_thinking(&self, thinking_buffer: &str, started_at: Option<i64>) {
        if thinking_buffer.is_empty() {
            return;
        }
        let content = json!({
            "content": thinking_buffer,
            "status": "done",
        })
        .to_string();
        let row = MessageRow {
            id: ConversationService::mint_msg_id(),
            conversation_id: self.conversation_id.clone(),
            msg_id: Some(self.msg_id.clone()),
            r#type: "thinking".into(),
            content,
            position: Some("left".into()),
            status: Some("finish".into()),
            hidden: false,
            created_at: started_at.unwrap_or_else(now_ms),
        };
        if let Err(e) = self.repo.insert_message(&row).await {
            error!(error = %e, "Failed to persist thinking message");
        }
    }

    /// Send a `thinking` event with `status: "done"` to close the thinking UI.
    fn send_thinking_done(&self) {
        let thinking_done = AgentStreamEvent::Thinking(ThinkingEventData {
            content: String::new(),
            subject: None,
            duration: None,
            status: Some("done".into()),
        });
        self.forward_to_websocket(&thinking_done);
    }

    async fn process_final_text(&self, text: &str) -> MiddlewareResult {
        let middleware = MessageMiddleware::new(
            self.cron_service
                .as_ref()
                .map(|service| Box::new(SharedCronService(Arc::clone(service))) as Box<dyn ICronService>),
        );

        middleware.process(text, &self.user_id, &self.conversation_id).await
    }

    fn send_final_text_override(&self, text: &str, hidden: bool) {
        self.broadcast_stream_payload(json!({
            "conversation_id": self.conversation_id,
            "msg_id": self.msg_id,
            "type": "content",
            "data": { "content": text },
            "hidden": hidden,
            "replace": true,
        }));
    }

    fn send_system_responses(&self, responses: &[String]) {
        for response in responses {
            self.broadcast_stream_payload(json!({
                "conversation_id": self.conversation_id,
                "msg_id": ConversationService::mint_msg_id(),
                "type": "system",
                "data": response,
                "hidden": true,
            }));
        }
    }

    fn broadcast_stream_payload(&self, payload: serde_json::Value) {
        let msg = WebSocketMessage::new("message.stream", payload);
        self.broadcaster.broadcast(msg);
    }

    pub async fn complete_conversation(
        repo: &Arc<dyn IConversationRepository>,
        broadcaster: &Arc<dyn EventBroadcaster>,
        conversation_id: &str,
    ) {
        let update = aionui_db::ConversationRowUpdate {
            status: Some("finished".to_owned()),
            updated_at: Some(now_ms()),
            ..Default::default()
        };
        if let Err(e) = repo.update(conversation_id, &update).await {
            error!(error = %e, "Failed to update conversation status");
        }

        let payload = json!({
            "conversation_id": conversation_id,
            "session_id": conversation_id,
            "status": "finished",
            "canSendMessage": true,
        });
        let msg = WebSocketMessage::new("turn.completed", payload);
        broadcaster.broadcast(msg);

        debug!(conversation_id, status = "finished", "Turn completed");
    }
}

struct SharedCronService(Arc<dyn ICronService>);

#[async_trait::async_trait]
impl ICronService for SharedCronService {
    async fn create_job(
        &self,
        user_id: &str,
        conversation_id: &str,
        params: &aionui_ai_agent::CronCreateParams,
    ) -> aionui_ai_agent::CronCommandResult {
        self.0.create_job(user_id, conversation_id, params).await
    }

    async fn update_job(
        &self,
        user_id: &str,
        conversation_id: &str,
        params: &aionui_ai_agent::CronUpdateParams,
    ) -> aionui_ai_agent::CronCommandResult {
        self.0.update_job(user_id, conversation_id, params).await
    }

    async fn list_jobs(&self, user_id: &str, conversation_id: &str) -> aionui_ai_agent::CronCommandResult {
        self.0.list_jobs(user_id, conversation_id).await
    }

    async fn delete_job(&self, user_id: &str, job_id: &str) -> aionui_ai_agent::CronCommandResult {
        self.0.delete_job(user_id, job_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_ai_agent::stream_event::{ErrorEventData, FinishEventData, TextEventData};
    use aionui_db::DbError;
    use std::sync::Mutex;

    // ── run() async tests ─────────────────────────────────────────

    #[tokio::test]
    async fn run_text_then_finish_persists_message() {
        let repo = Arc::new(RecordingRepo::new());
        let bus = Arc::new(aionui_realtime::BroadcastEventBus::new(64));
        let (tx, _) = broadcast::channel(64);

        let relay = StreamRelay::new(
            "conv-1".into(),
            "asst-1".into(),
            "user-1".into(),
            repo.clone(),
            bus.clone(),
            None,
        );

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
        tx.send(AgentStreamEvent::Finish(FinishEventData::default())).unwrap();

        let outcome = relay.consume(rx).await;
        assert!(outcome.system_responses.is_empty());

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

        let relay = StreamRelay::new(
            "conv-1".into(),
            "asst-1".into(),
            "user-1".into(),
            repo.clone(),
            bus.clone(),
            None,
        );

        let rx = tx.subscribe();

        tx.send(AgentStreamEvent::Error(ErrorEventData {
            message: "Something went wrong".into(),
            code: None,
        }))
        .unwrap();

        let outcome = relay.consume(rx).await;
        assert!(outcome.system_responses.is_empty());

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

        let relay = StreamRelay::new(
            "conv-1".into(),
            "asst-1".into(),
            "user-1".into(),
            repo.clone(),
            bus.clone(),
            None,
        );

        let rx = tx.subscribe();

        // Send text then drop sender (channel closes without Finish)
        tx.send(AgentStreamEvent::Text(TextEventData {
            content: "partial".into(),
        }))
        .unwrap();
        drop(tx);

        let outcome = relay.consume(rx).await;
        assert!(outcome.system_responses.is_empty());

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

        let relay = StreamRelay::new(
            "conv-1".into(),
            "asst-1".into(),
            "user-1".into(),
            repo.clone(),
            bus.clone(),
            None,
        );

        // Subscribe to the bus before relay runs
        let mut ws_rx = bus.subscribe();
        let rx = tx.subscribe();

        tx.send(AgentStreamEvent::Finish(FinishEventData::default())).unwrap();

        let outcome = relay.consume(rx).await;
        assert!(outcome.system_responses.is_empty());

        // Collect WebSocket events
        let mut ws_events = vec![];
        while let Ok(evt) = ws_rx.try_recv() {
            ws_events.push(evt);
        }

        // Should have turn.completed event
        let turn_event = ws_events.iter().find(|e| e.name == "turn.completed");
        assert!(turn_event.is_some());
        let data = &turn_event.unwrap().data;
        assert_eq!(data["conversation_id"], "conv-1");
        assert_eq!(data["session_id"], "conv-1");
        assert_eq!(data["status"], "finished");
        assert_eq!(data["canSendMessage"], true);
    }

    #[tokio::test]
    async fn run_finalizes_with_cleaned_replacement_event() {
        let repo = Arc::new(RecordingRepo::new());
        let bus = Arc::new(aionui_realtime::BroadcastEventBus::new(64));
        let (tx, _) = broadcast::channel(64);
        let relay = StreamRelay::new(
            "conv-1".into(),
            "asst-1".into(),
            "user-1".into(),
            repo.clone(),
            bus.clone(),
            Some(Arc::new(MockCronService)),
        );

        let mut ws_rx = bus.subscribe();
        let rx = tx.subscribe();
        tx.send(AgentStreamEvent::Text(TextEventData {
            content: "Hello [CRON_LIST]".into(),
        }))
        .unwrap();
        tx.send(AgentStreamEvent::Finish(FinishEventData::default())).unwrap();

        let outcome = relay.consume(rx).await;
        assert_eq!(outcome.system_responses, vec!["[System: listed]".to_string()]);

        let inserts = repo.take_inserts();
        assert_eq!(inserts.len(), 1);
        let content: serde_json::Value = serde_json::from_str(&inserts[0].content).unwrap();
        assert_eq!(content["content"].as_str().map(str::trim), Some("Hello"));

        let mut ws_events = vec![];
        while let Ok(evt) = ws_rx.try_recv() {
            ws_events.push(evt);
        }

        let replacement = ws_events
            .iter()
            .find(|evt| evt.name == "message.stream" && evt.data["type"] == "content" && evt.data["replace"] == true);
        assert!(replacement.is_some());
        assert_eq!(
            replacement.unwrap().data["data"]["content"].as_str().map(str::trim),
            Some("Hello")
        );
    }

    // ── Helpers ──────────────────────────────────────────────────

    struct MockCronService;

    #[async_trait::async_trait]
    impl ICronService for MockCronService {
        async fn create_job(
            &self,
            _user_id: &str,
            _conversation_id: &str,
            _params: &aionui_ai_agent::CronCreateParams,
        ) -> aionui_ai_agent::CronCommandResult {
            aionui_ai_agent::CronCommandResult {
                success: true,
                message: "created".into(),
            }
        }

        async fn update_job(
            &self,
            _user_id: &str,
            _conversation_id: &str,
            _params: &aionui_ai_agent::CronUpdateParams,
        ) -> aionui_ai_agent::CronCommandResult {
            aionui_ai_agent::CronCommandResult {
                success: true,
                message: "updated".into(),
            }
        }

        async fn list_jobs(&self, _user_id: &str, _conversation_id: &str) -> aionui_ai_agent::CronCommandResult {
            aionui_ai_agent::CronCommandResult {
                success: true,
                message: "listed".into(),
            }
        }

        async fn delete_job(&self, _user_id: &str, _job_id: &str) -> aionui_ai_agent::CronCommandResult {
            aionui_ai_agent::CronCommandResult {
                success: true,
                message: "deleted".into(),
            }
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
        async fn get(&self, _id: &str) -> Result<Option<aionui_db::models::ConversationRow>, DbError> {
            Ok(None)
        }
        async fn create(&self, _row: &aionui_db::models::ConversationRow) -> Result<(), DbError> {
            Ok(())
        }
        async fn update(&self, _id: &str, _updates: &aionui_db::ConversationRowUpdate) -> Result<(), DbError> {
            Ok(())
        }
        async fn delete(&self, _id: &str) -> Result<(), DbError> {
            Ok(())
        }
        async fn list_paginated(
            &self,
            _user_id: &str,
            _filters: &aionui_db::ConversationFilters,
        ) -> Result<aionui_common::PaginatedResult<aionui_db::models::ConversationRow>, DbError> {
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
        async fn update_message(&self, id: &str, updates: &aionui_db::MessageRowUpdate) -> Result<(), DbError> {
            self.updates.lock().unwrap().push((id.to_owned(), updates.clone()));
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
