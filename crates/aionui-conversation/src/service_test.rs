use std::sync::{Arc, Mutex};

use aionui_ai_agent::agent_manager::{AgentManagerHandle, IAgentManager};
use aionui_ai_agent::stream_event::AgentStreamEvent;
use aionui_ai_agent::types::{BuildTaskOptions, SendMessageData};
use aionui_ai_agent::IWorkerTaskManager;
use aionui_api_types::{
    CloneConversationRequest, CreateConversationRequest, ListConversationsQuery,
    SearchMessagesQuery, SendMessageRequest, UpdateConversationRequest, WebSocketMessage,
};
use aionui_common::{
    AgentKillReason, AgentType, AppError, Confirmation, ConversationSource, ConversationStatus,
    PaginatedResult, TimestampMs,
};
use aionui_db::models::{ConversationRow, MessageRow};
use aionui_db::{
    ConversationFilters, ConversationRowUpdate, IConversationRepository, MessageRowUpdate,
    MessageSearchRow, SortOrder,
};
use aionui_realtime::EventBroadcaster;
use serde_json::json;
use tokio::sync::broadcast;

use crate::service::ConversationService;

// ── Mock EventBroadcaster ──────────────────────────────────────────

struct MockBroadcaster {
    events: Mutex<Vec<WebSocketMessage<serde_json::Value>>>,
}

impl MockBroadcaster {
    fn new() -> Self {
        Self {
            events: Mutex::new(vec![]),
        }
    }

    fn take_events(&self) -> Vec<WebSocketMessage<serde_json::Value>> {
        std::mem::take(&mut self.events.lock().unwrap())
    }
}

impl EventBroadcaster for MockBroadcaster {
    fn broadcast(&self, event: WebSocketMessage<serde_json::Value>) {
        self.events.lock().unwrap().push(event);
    }
}

// ── Mock Repository ────────────────────────────────────────────────

struct MockRepo {
    rows: Mutex<Vec<ConversationRow>>,
}

impl MockRepo {
    fn new() -> Self {
        Self {
            rows: Mutex::new(vec![]),
        }
    }

}

#[async_trait::async_trait]
impl IConversationRepository for MockRepo {
    async fn get(&self, id: &str) -> Result<Option<ConversationRow>, aionui_db::DbError> {
        let rows = self.rows.lock().unwrap();
        Ok(rows.iter().find(|r| r.id == id).cloned())
    }

    async fn create(&self, row: &ConversationRow) -> Result<(), aionui_db::DbError> {
        self.rows.lock().unwrap().push(row.clone());
        Ok(())
    }

    async fn update(
        &self,
        id: &str,
        updates: &ConversationRowUpdate,
    ) -> Result<(), aionui_db::DbError> {
        let mut rows = self.rows.lock().unwrap();
        let row = rows
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| aionui_db::DbError::NotFound(format!("Conversation {id}")))?;

        if let Some(name) = &updates.name {
            row.name = name.clone();
        }
        if let Some(pinned) = updates.pinned {
            row.pinned = pinned;
        }
        if let Some(pinned_at) = &updates.pinned_at {
            row.pinned_at = *pinned_at;
        }
        if let Some(model) = &updates.model {
            row.model = model.clone();
        }
        if let Some(extra) = &updates.extra {
            row.extra = extra.clone();
        }
        if let Some(status) = &updates.status {
            row.status = status.clone();
        }
        if let Some(updated_at) = updates.updated_at {
            row.updated_at = updated_at;
        }
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<(), aionui_db::DbError> {
        let mut rows = self.rows.lock().unwrap();
        let len_before = rows.len();
        rows.retain(|r| r.id != id);
        if rows.len() == len_before {
            return Err(aionui_db::DbError::NotFound(format!("Conversation {id}")));
        }
        Ok(())
    }

    async fn list_paginated(
        &self,
        user_id: &str,
        filters: &ConversationFilters,
    ) -> Result<PaginatedResult<ConversationRow>, aionui_db::DbError> {
        let rows = self.rows.lock().unwrap();
        let matched: Vec<_> = rows
            .iter()
            .filter(|r| r.user_id == user_id)
            .filter(|r| {
                filters
                    .source
                    .as_ref()
                    .is_none_or(|s| r.source.as_deref() == Some(s.as_str()))
            })
            .filter(|r| {
                filters
                    .pinned
                    .as_ref()
                    .is_none_or(|&p| r.pinned == p)
            })
            .cloned()
            .collect();
        let total = matched.len() as u64;
        let limit = filters.effective_limit() as usize;
        let items: Vec<_> = matched.into_iter().take(limit).collect();
        let has_more = (total as usize) > limit;
        Ok(PaginatedResult {
            items,
            total,
            has_more,
        })
    }

    async fn find_by_source_and_chat(
        &self,
        _user_id: &str,
        _source: &str,
        _chat_id: &str,
        _agent_type: &str,
    ) -> Result<Option<ConversationRow>, aionui_db::DbError> {
        Ok(None)
    }

    async fn list_by_cron_job(
        &self,
        _user_id: &str,
        _cron_job_id: &str,
    ) -> Result<Vec<ConversationRow>, aionui_db::DbError> {
        Ok(vec![])
    }

    async fn list_associated(
        &self,
        _user_id: &str,
        _conversation_id: &str,
    ) -> Result<Vec<ConversationRow>, aionui_db::DbError> {
        Ok(vec![])
    }

    async fn get_messages(
        &self,
        _conv_id: &str,
        _page: u32,
        _page_size: u32,
        _order: SortOrder,
    ) -> Result<PaginatedResult<MessageRow>, aionui_db::DbError> {
        Ok(PaginatedResult {
            items: vec![],
            total: 0,
            has_more: false,
        })
    }

    async fn insert_message(&self, _message: &MessageRow) -> Result<(), aionui_db::DbError> {
        Ok(())
    }

    async fn update_message(
        &self,
        _id: &str,
        _updates: &MessageRowUpdate,
    ) -> Result<(), aionui_db::DbError> {
        Ok(())
    }

    async fn delete_messages_by_conversation(
        &self,
        _conv_id: &str,
    ) -> Result<(), aionui_db::DbError> {
        Ok(())
    }

    async fn get_message_by_msg_id(
        &self,
        _conv_id: &str,
        _msg_id: &str,
        _msg_type: &str,
    ) -> Result<Option<MessageRow>, aionui_db::DbError> {
        Ok(None)
    }

    async fn search_messages(
        &self,
        _user_id: &str,
        _keyword: &str,
        _page: u32,
        _page_size: u32,
    ) -> Result<PaginatedResult<MessageSearchRow>, aionui_db::DbError> {
        Ok(PaginatedResult {
            items: vec![],
            total: 0,
            has_more: false,
        })
    }
}

// ── Helpers ────────────────────────────────────────────────────────

fn make_service() -> (ConversationService, Arc<MockBroadcaster>, Arc<MockRepo>) {
    let repo = Arc::new(MockRepo::new());
    let broadcaster = Arc::new(MockBroadcaster::new());
    let svc = ConversationService::new(repo.clone(), broadcaster.clone());
    (svc, broadcaster, repo)
}

fn make_create_req() -> CreateConversationRequest {
    serde_json::from_value(json!({
        "type": "gemini",
        "model": { "providerId": "p1", "model": "m1" },
        "extra": { "workspace": "/project" }
    }))
    .unwrap()
}

// ── Create tests ───────────────────────────────────────────────────

#[tokio::test]
async fn create_returns_conversation_with_defaults() {
    let (svc, broadcaster, _repo) = make_service();

    let resp = svc.create("user_1", make_create_req()).await.unwrap();

    assert!(!resp.id.is_empty());
    assert_eq!(resp.r#type, AgentType::Gemini);
    assert_eq!(resp.status, ConversationStatus::Pending);
    assert_eq!(resp.source, Some(ConversationSource::Aionui));
    assert!(!resp.pinned);
    assert!(resp.pinned_at.is_none());
    assert_eq!(resp.extra["workspace"], "/project");
    assert!(resp.created_at > 0);
    assert_eq!(resp.created_at, resp.modified_at);

    // Should have broadcast a listChanged(created) event
    let events = broadcaster.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "conversation.listChanged");
    assert_eq!(events[0].data["action"], "created");
    assert_eq!(events[0].data["conversationId"], resp.id);
    assert_eq!(events[0].data["source"], "aionui");
}

#[tokio::test]
async fn create_with_custom_name_and_source() {
    let (svc, _broadcaster, _repo) = make_service();

    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "acp",
        "name": "Custom Name",
        "model": { "providerId": "p1", "model": "m1" },
        "source": "telegram",
        "channelChatId": "chat:123",
        "extra": {}
    }))
    .unwrap();

    let resp = svc.create("user_1", req).await.unwrap();

    assert_eq!(resp.name, "Custom Name");
    assert_eq!(resp.r#type, AgentType::Acp);
    assert_eq!(resp.source, Some(ConversationSource::Telegram));
    assert_eq!(resp.channel_chat_id.as_deref(), Some("chat:123"));
}

#[tokio::test]
async fn create_stores_model_as_json() {
    let (svc, _broadcaster, _repo) = make_service();

    let resp = svc.create("user_1", make_create_req()).await.unwrap();

    let model = resp.model.unwrap();
    assert_eq!(model.provider_id, "p1");
    assert_eq!(model.model, "m1");
}

// ── Get tests ──────────────────────────────────────────────────────

#[tokio::test]
async fn get_existing_conversation() {
    let (svc, _broadcaster, _repo) = make_service();
    let created = svc.create("user_1", make_create_req()).await.unwrap();

    let fetched = svc.get("user_1", &created.id).await.unwrap();
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.name, created.name);
}

#[tokio::test]
async fn get_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let err = svc.get("user_1", "non-existent").await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

// ── List tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn list_empty() {
    let (svc, _broadcaster, _repo) = make_service();
    let result = svc
        .list("user_1", ListConversationsQuery::default())
        .await
        .unwrap();
    assert!(result.items.is_empty());
    assert_eq!(result.total, 0);
    assert!(!result.has_more);
}

#[tokio::test]
async fn list_returns_created_conversations() {
    let (svc, _broadcaster, _repo) = make_service();
    svc.create("user_1", make_create_req()).await.unwrap();
    svc.create("user_1", make_create_req()).await.unwrap();

    let result = svc
        .list("user_1", ListConversationsQuery::default())
        .await
        .unwrap();
    assert_eq!(result.items.len(), 2);
    assert_eq!(result.total, 2);
}

#[tokio::test]
async fn list_filters_by_user() {
    let (svc, _broadcaster, _repo) = make_service();
    svc.create("user_1", make_create_req()).await.unwrap();
    svc.create("user_2", make_create_req()).await.unwrap();

    let result = svc
        .list("user_1", ListConversationsQuery::default())
        .await
        .unwrap();
    assert_eq!(result.items.len(), 1);
}

#[tokio::test]
async fn list_with_source_filter() {
    let (svc, _broadcaster, _repo) = make_service();
    svc.create("user_1", make_create_req()).await.unwrap();

    let telegram_req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "gemini",
        "model": { "providerId": "p1", "model": "m1" },
        "source": "telegram",
        "extra": {}
    }))
    .unwrap();
    svc.create("user_1", telegram_req).await.unwrap();

    let query = ListConversationsQuery {
        source: Some("telegram".into()),
        ..Default::default()
    };
    let result = svc.list("user_1", query).await.unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(
        result.items[0].source,
        Some(ConversationSource::Telegram)
    );
}

#[tokio::test]
async fn list_with_pinned_filter() {
    let (svc, _broadcaster, _repo) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    svc.create("user_1", make_create_req()).await.unwrap();

    // Pin the first one
    let update_req: UpdateConversationRequest =
        serde_json::from_value(json!({ "pinned": true })).unwrap();
    svc.update("user_1", &conv.id, update_req).await.unwrap();

    let query = ListConversationsQuery {
        pinned: Some(true),
        ..Default::default()
    };
    let result = svc.list("user_1", query).await.unwrap();
    assert_eq!(result.items.len(), 1);
    assert!(result.items[0].pinned);
}

// ── Update tests ───────────────────────────────────────────────────

#[tokio::test]
async fn update_name() {
    let (svc, broadcaster, _repo) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    broadcaster.take_events(); // clear create event

    let req: UpdateConversationRequest =
        serde_json::from_value(json!({ "name": "New Name" })).unwrap();
    let updated = svc.update("user_1", &conv.id, req).await.unwrap();

    assert_eq!(updated.name, "New Name");
    assert!(updated.modified_at >= conv.modified_at);

    let events = broadcaster.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data["action"], "updated");
}

#[tokio::test]
async fn update_pin() {
    let (svc, _broadcaster, _repo) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    assert!(!conv.pinned);

    let req: UpdateConversationRequest =
        serde_json::from_value(json!({ "pinned": true })).unwrap();
    let updated = svc.update("user_1", &conv.id, req).await.unwrap();
    assert!(updated.pinned);
    assert!(updated.pinned_at.is_some());
}

#[tokio::test]
async fn update_unpin_clears_pinned_at() {
    let (svc, _broadcaster, _repo) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    // Pin first
    let pin_req: UpdateConversationRequest =
        serde_json::from_value(json!({ "pinned": true })).unwrap();
    let pinned = svc.update("user_1", &conv.id, pin_req).await.unwrap();
    assert!(pinned.pinned);
    assert!(pinned.pinned_at.is_some());

    // Unpin
    let unpin_req: UpdateConversationRequest =
        serde_json::from_value(json!({ "pinned": false })).unwrap();
    let unpinned = svc.update("user_1", &conv.id, unpin_req).await.unwrap();
    assert!(!unpinned.pinned);
    assert!(unpinned.pinned_at.is_none());
}

#[tokio::test]
async fn update_extra_merge() {
    let (svc, _broadcaster, _repo) = make_service();

    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "gemini",
        "model": { "providerId": "p1", "model": "m1" },
        "extra": { "workspace": "/old", "contextFileName": "ctx.md" }
    }))
    .unwrap();
    let conv = svc.create("user_1", req).await.unwrap();

    // Update only workspace — contextFileName should be preserved
    let update_req: UpdateConversationRequest =
        serde_json::from_value(json!({ "extra": { "workspace": "/new" } })).unwrap();
    let updated = svc.update("user_1", &conv.id, update_req).await.unwrap();

    assert_eq!(updated.extra["workspace"], "/new");
    assert_eq!(updated.extra["contextFileName"], "ctx.md");
}

#[tokio::test]
async fn update_model() {
    let (svc, _broadcaster, _repo) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let req: UpdateConversationRequest = serde_json::from_value(json!({
        "model": { "providerId": "p2", "model": "new-model" }
    }))
    .unwrap();
    let updated = svc.update("user_1", &conv.id, req).await.unwrap();

    let model = updated.model.unwrap();
    assert_eq!(model.provider_id, "p2");
    assert_eq!(model.model, "new-model");
}

#[tokio::test]
async fn update_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let req: UpdateConversationRequest =
        serde_json::from_value(json!({ "name": "x" })).unwrap();
    let err = svc.update("user_1", "non-existent", req).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

// ── Delete tests ───────────────────────────────────────────────────

#[tokio::test]
async fn delete_conversation() {
    let (svc, broadcaster, _repo) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    broadcaster.take_events();

    svc.delete("user_1", &conv.id).await.unwrap();

    // Should be gone
    let err = svc.get("user_1", &conv.id).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));

    // Should broadcast deleted
    let events = broadcaster.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data["action"], "deleted");
    assert_eq!(events[0].data["conversationId"], conv.id);
}

#[tokio::test]
async fn delete_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let err = svc.delete("user_1", "non-existent").await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

// ── Broadcast payload tests ────────────────────────────────────────

#[tokio::test]
async fn broadcast_includes_source_on_delete() {
    let (svc, broadcaster, _repo) = make_service();

    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "gemini",
        "model": { "providerId": "p1", "model": "m1" },
        "source": "telegram",
        "extra": {}
    }))
    .unwrap();
    let conv = svc.create("user_1", req).await.unwrap();
    broadcaster.take_events();

    svc.delete("user_1", &conv.id).await.unwrap();
    let events = broadcaster.take_events();
    assert_eq!(events[0].data["source"], "telegram");
}

#[tokio::test]
async fn all_crud_operations_broadcast() {
    let (svc, broadcaster, _repo) = make_service();

    // Create
    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let events = broadcaster.take_events();
    assert_eq!(events[0].data["action"], "created");

    // Update
    let req: UpdateConversationRequest =
        serde_json::from_value(json!({ "name": "x" })).unwrap();
    svc.update("user_1", &conv.id, req).await.unwrap();
    let events = broadcaster.take_events();
    assert_eq!(events[0].data["action"], "updated");

    // Delete
    svc.delete("user_1", &conv.id).await.unwrap();
    let events = broadcaster.take_events();
    assert_eq!(events[0].data["action"], "deleted");
}

// ── Ownership tests (M-3) ─────────────────────────────────────────

#[tokio::test]
async fn get_wrong_user_returns_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let err = svc.get("user_2", &conv.id).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn update_wrong_user_returns_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let req: UpdateConversationRequest =
        serde_json::from_value(json!({ "name": "hacked" })).unwrap();
    let err = svc.update("user_2", &conv.id, req).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));

    // Original should be unchanged
    let original = svc.get("user_1", &conv.id).await.unwrap();
    assert_ne!(original.name, "hacked");
}

#[tokio::test]
async fn delete_wrong_user_returns_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let err = svc.delete("user_2", &conv.id).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));

    // Should still exist
    let still_exists = svc.get("user_1", &conv.id).await.unwrap();
    assert_eq!(still_exists.id, conv.id);
}

// ── Clone tests ───────────────────────────────────────────────────

#[tokio::test]
async fn clone_without_source_creates_new() {
    let (svc, broadcaster, _repo) = make_service();

    let req: CloneConversationRequest = serde_json::from_value(json!({
        "conversation": {
            "type": "gemini",
            "name": "Cloned",
            "model": { "providerId": "p1", "model": "m1" },
            "extra": { "workspace": "/new" }
        }
    }))
    .unwrap();

    let resp = svc.clone_create("user_1", req).await.unwrap();
    assert_eq!(resp.name, "Cloned");
    assert_eq!(resp.extra["workspace"], "/new");

    let events = broadcaster.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data["action"], "created");
}

#[tokio::test]
async fn clone_from_source_inherits_config() {
    let (svc, _broadcaster, _repo) = make_service();

    // Create source with name and extra
    let source_req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "gemini",
        "name": "Source Conv",
        "model": { "providerId": "p1", "model": "m1" },
        "extra": { "workspace": "/source", "contextFileName": "ctx.md" }
    }))
    .unwrap();
    let source = svc.create("user_1", source_req).await.unwrap();

    // Clone with override on workspace only
    let clone_req: CloneConversationRequest = serde_json::from_value(json!({
        "sourceConversationId": source.id,
        "conversation": {
            "type": "gemini",
            "model": { "providerId": "p1", "model": "m1" },
            "extra": { "workspace": "/cloned" }
        }
    }))
    .unwrap();
    let cloned = svc.clone_create("user_1", clone_req).await.unwrap();

    // Name inherited from source (no name in clone request)
    assert_eq!(cloned.name, "Source Conv");
    // Workspace overridden, contextFileName inherited
    assert_eq!(cloned.extra["workspace"], "/cloned");
    assert_eq!(cloned.extra["contextFileName"], "ctx.md");
    // New ID
    assert_ne!(cloned.id, source.id);
}

#[tokio::test]
async fn clone_source_not_found() {
    let (svc, _broadcaster, _repo) = make_service();

    let req: CloneConversationRequest = serde_json::from_value(json!({
        "sourceConversationId": "no-such-id",
        "conversation": {
            "type": "gemini",
            "model": { "providerId": "p1", "model": "m1" },
            "extra": {}
        }
    }))
    .unwrap();

    let err = svc.clone_create("user_1", req).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn clone_source_wrong_user() {
    let (svc, _broadcaster, _repo) = make_service();

    let source = svc.create("user_1", make_create_req()).await.unwrap();

    let req: CloneConversationRequest = serde_json::from_value(json!({
        "sourceConversationId": source.id,
        "conversation": {
            "type": "gemini",
            "model": { "providerId": "p1", "model": "m1" },
            "extra": {}
        }
    }))
    .unwrap();

    let err = svc.clone_create("user_2", req).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn clone_strips_cron_job_id_by_default() {
    let (svc, _broadcaster, _repo) = make_service();

    let source_req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "gemini",
        "model": { "providerId": "p1", "model": "m1" },
        "extra": { "workspace": "/p", "cronJobId": "cron_1" }
    }))
    .unwrap();
    let source = svc.create("user_1", source_req).await.unwrap();

    let clone_req: CloneConversationRequest = serde_json::from_value(json!({
        "sourceConversationId": source.id,
        "conversation": {
            "type": "gemini",
            "model": { "providerId": "p1", "model": "m1" },
            "extra": {}
        }
    }))
    .unwrap();
    let cloned = svc.clone_create("user_1", clone_req).await.unwrap();

    // cronJobId should not be carried over
    assert!(cloned.extra.get("cronJobId").is_none());
}

#[tokio::test]
async fn clone_with_migrate_cron_preserves_cron_job_id() {
    let (svc, _broadcaster, _repo) = make_service();

    let source_req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "gemini",
        "model": { "providerId": "p1", "model": "m1" },
        "extra": { "workspace": "/p", "cronJobId": "cron_1" }
    }))
    .unwrap();
    let source = svc.create("user_1", source_req).await.unwrap();

    let clone_req: CloneConversationRequest = serde_json::from_value(json!({
        "sourceConversationId": source.id,
        "conversation": {
            "type": "gemini",
            "model": { "providerId": "p1", "model": "m1" },
            "extra": {}
        },
        "migrateCron": true
    }))
    .unwrap();
    let cloned = svc.clone_create("user_1", clone_req).await.unwrap();

    assert_eq!(cloned.extra["cronJobId"], "cron_1");
}

// ── Reset tests ───────────────────────────────────────────────────

#[tokio::test]
async fn reset_sets_status_to_pending() {
    let (svc, _broadcaster, _repo) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    svc.reset("user_1", &conv.id).await.unwrap();

    let fetched = svc.get("user_1", &conv.id).await.unwrap();
    assert_eq!(fetched.status, ConversationStatus::Pending);
}

#[tokio::test]
async fn reset_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let err = svc.reset("user_1", "no-such-id").await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn reset_wrong_user() {
    let (svc, _broadcaster, _repo) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let err = svc.reset("user_2", &conv.id).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

// ── Search validation tests ───────────────────────────────────────

#[tokio::test]
async fn search_messages_empty_keyword_returns_bad_request() {
    let (svc, _broadcaster, _repo) = make_service();

    let query = SearchMessagesQuery {
        keyword: "".into(),
        page: None,
        page_size: None,
    };
    let err = svc.search_messages("user_1", query).await.unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

#[tokio::test]
async fn search_messages_whitespace_keyword_returns_bad_request() {
    let (svc, _broadcaster, _repo) = make_service();

    let query = SearchMessagesQuery {
        keyword: "   ".into(),
        page: None,
        page_size: None,
    };
    let err = svc.search_messages("user_1", query).await.unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

// ── Mock Agent ───────────────────────────────────────────────────

struct MockAgent {
    conversation_id: String,
    event_tx: broadcast::Sender<AgentStreamEvent>,
    stopped: Mutex<bool>,
    confirmations: Mutex<Vec<Confirmation>>,
    approval_memory: Mutex<std::collections::HashMap<String, bool>>,
}

impl MockAgent {
    fn new(conversation_id: &str) -> Self {
        let (event_tx, _) = broadcast::channel(64);
        Self {
            conversation_id: conversation_id.to_owned(),
            event_tx,
            stopped: Mutex::new(false),
            confirmations: Mutex::new(vec![]),
            approval_memory: Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn with_confirmations(conversation_id: &str, confirmations: Vec<Confirmation>) -> Self {
        let (event_tx, _) = broadcast::channel(64);
        Self {
            conversation_id: conversation_id.to_owned(),
            event_tx,
            stopped: Mutex::new(false),
            confirmations: Mutex::new(confirmations),
            approval_memory: Mutex::new(std::collections::HashMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl IAgentManager for MockAgent {
    fn agent_type(&self) -> AgentType {
        AgentType::Acp
    }
    fn status(&self) -> Option<ConversationStatus> {
        None
    }
    fn workspace(&self) -> &str {
        "/tmp/test"
    }
    fn conversation_id(&self) -> &str {
        &self.conversation_id
    }
    fn last_activity_at(&self) -> TimestampMs {
        0
    }
    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.event_tx.subscribe()
    }
    async fn send_message(&self, _data: SendMessageData) -> Result<(), AppError> {
        // Emit finish event so the relay task completes
        let _ = self.event_tx.send(AgentStreamEvent::Finish(
            aionui_ai_agent::stream_event::FinishEventData::default(),
        ));
        Ok(())
    }
    async fn stop(&self) -> Result<(), AppError> {
        *self.stopped.lock().unwrap() = true;
        Ok(())
    }
    fn confirm(
        &self,
        _msg_id: &str,
        call_id: &str,
        _data: serde_json::Value,
        always_allow: bool,
    ) -> Result<(), AppError> {
        let mut confs = self.confirmations.lock().unwrap();
        if always_allow {
            if let Some(conf) = confs.iter().find(|c| c.call_id == call_id) {
                let key = match (conf.action.as_deref(), conf.command_type.as_deref()) {
                    (Some(a), Some(ct)) => format!("{a}:{ct}"),
                    (Some(a), None) => a.to_owned(),
                    _ => String::new(),
                };
                self.approval_memory.lock().unwrap().insert(key, true);
            }
        }
        confs.retain(|c| c.call_id != call_id);
        Ok(())
    }
    fn get_confirmations(&self) -> Vec<Confirmation> {
        self.confirmations.lock().unwrap().clone()
    }
    fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool {
        let key = match command_type {
            Some(ct) => format!("{action}:{ct}"),
            None => action.to_owned(),
        };
        self.approval_memory
            .lock()
            .unwrap()
            .get(&key)
            .copied()
            .unwrap_or(false)
    }
    fn kill(&self, _reason: Option<AgentKillReason>) -> Result<(), AppError> {
        Ok(())
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ── Mock WorkerTaskManager ──────────────────────────────────────

struct MockTaskManager {
    agents: Mutex<std::collections::HashMap<String, AgentManagerHandle>>,
}

impl MockTaskManager {
    fn new() -> Self {
        Self {
            agents: Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn insert_agent(&self, conversation_id: &str, agent: AgentManagerHandle) {
        self.agents
            .lock()
            .unwrap()
            .insert(conversation_id.to_owned(), agent);
    }
}

impl IWorkerTaskManager for MockTaskManager {
    fn get_task(&self, conversation_id: &str) -> Option<AgentManagerHandle> {
        self.agents.lock().unwrap().get(conversation_id).cloned()
    }

    fn get_or_build_task(
        &self,
        conversation_id: &str,
        _options: BuildTaskOptions,
    ) -> Result<AgentManagerHandle, AppError> {
        let mut agents = self.agents.lock().unwrap();
        if let Some(existing) = agents.get(conversation_id) {
            return Ok(existing.clone());
        }
        let agent: AgentManagerHandle = Arc::new(MockAgent::new(conversation_id));
        agents.insert(conversation_id.to_owned(), agent.clone());
        Ok(agent)
    }

    fn kill(&self, conversation_id: &str, _reason: Option<AgentKillReason>) -> Result<(), AppError> {
        self.agents.lock().unwrap().remove(conversation_id);
        Ok(())
    }

    fn clear(&self) {
        self.agents.lock().unwrap().clear();
    }

    fn active_count(&self) -> usize {
        self.agents.lock().unwrap().len()
    }

    fn collect_idle(&self, _idle_threshold_ms: TimestampMs) -> Vec<String> {
        vec![]
    }
}

// ── send_message tests ──────────────────────────────────────────

fn make_send_req() -> SendMessageRequest {
    serde_json::from_value(json!({
        "content": "Hello",
        "msgId": "msg-1"
    }))
    .unwrap()
}

#[tokio::test]
async fn send_message_returns_accepted() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let result = svc
        .send_message("user_1", &conv.id, make_send_req(), &task_mgr)
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn send_message_empty_content_returns_bad_request() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let req: SendMessageRequest = serde_json::from_value(json!({
        "content": "",
        "msgId": "msg-1"
    }))
    .unwrap();

    let err = svc
        .send_message("user_1", &conv.id, req, &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

#[tokio::test]
async fn send_message_whitespace_content_returns_bad_request() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let req: SendMessageRequest = serde_json::from_value(json!({
        "content": "   ",
        "msgId": "msg-1"
    }))
    .unwrap();

    let err = svc
        .send_message("user_1", &conv.id, req, &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

#[tokio::test]
async fn send_message_conversation_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let err = svc
        .send_message("user_1", "no-such-id", make_send_req(), &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn send_message_wrong_user_returns_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let err = svc
        .send_message("user_2", &conv.id, make_send_req(), &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn send_message_running_conversation_returns_conflict() {
    let (svc, _broadcaster, repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    // Manually set status to running
    let update = ConversationRowUpdate {
        status: Some("running".into()),
        ..Default::default()
    };
    repo.update(&conv.id, &update).await.unwrap();

    let err = svc
        .send_message("user_1", &conv.id, make_send_req(), &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)));
}

// ── stop_stream tests ───────────────────────────────────────────

#[tokio::test]
async fn stop_stream_with_active_agent() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    // Build agent via send_message
    svc.send_message("user_1", &conv.id, make_send_req(), &(task_mgr.clone() as Arc<dyn IWorkerTaskManager>))
        .await
        .unwrap();

    // Stop should succeed since agent exists
    let result = svc
        .stop_stream("user_1", &conv.id, &(task_mgr as Arc<dyn IWorkerTaskManager>))
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn stop_stream_conversation_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let err = svc
        .stop_stream("user_1", "no-such-id", &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn stop_stream_no_active_agent_returns_conflict() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let err = svc
        .stop_stream("user_1", &conv.id, &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)));
}

#[tokio::test]
async fn stop_stream_wrong_user_returns_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let err = svc
        .stop_stream("user_2", &conv.id, &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

// ── warmup tests ────────────────────────────────────────────────

#[tokio::test]
async fn warmup_creates_agent_task() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let result = svc
        .warmup("user_1", &conv.id, &(task_mgr.clone() as Arc<dyn IWorkerTaskManager>))
        .await;
    assert!(result.is_ok());

    // Agent should now exist
    assert!(task_mgr.get_task(&conv.id).is_some());
}

#[tokio::test]
async fn warmup_conversation_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let err = svc
        .warmup("user_1", "no-such-id", &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn warmup_wrong_user_returns_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let err = svc
        .warmup("user_2", &conv.id, &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

// ── Confirmation system tests ────────────────────────────────────

fn make_test_confirmations() -> Vec<Confirmation> {
    vec![
        Confirmation {
            id: "c1".into(),
            call_id: "call-1".into(),
            title: Some("Allow file edit".into()),
            action: Some("edit_file".into()),
            description: "Edit main.rs".into(),
            command_type: Some("bash".into()),
            options: vec![],
        },
        Confirmation {
            id: "c2".into(),
            call_id: "call-2".into(),
            title: Some("Read file".into()),
            action: Some("read_file".into()),
            description: "Read config.toml".into(),
            command_type: None,
            options: vec![],
        },
    ]
}

#[tokio::test]
async fn list_confirmations_empty_when_no_agent() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let result = svc
        .list_confirmations("user_1", &conv.id, &task_mgr)
        .await
        .unwrap();
    assert!(result.is_empty());
}

#[tokio::test]
async fn list_confirmations_returns_items() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let agent: AgentManagerHandle =
        Arc::new(MockAgent::with_confirmations(&conv.id, make_test_confirmations()));
    task_mgr.insert_agent(&conv.id, agent);

    let result = svc
        .list_confirmations("user_1", &conv.id, &(task_mgr as Arc<dyn IWorkerTaskManager>))
        .await
        .unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].call_id, "call-1");
    assert_eq!(result[1].call_id, "call-2");
}

#[tokio::test]
async fn list_confirmations_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let err = svc
        .list_confirmations("user_1", "no-such-id", &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn list_confirmations_wrong_user() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let err = svc
        .list_confirmations("user_2", &conv.id, &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn confirm_removes_confirmation_and_broadcasts() {
    let (svc, broadcaster, _repo) = make_service();
    let task_mgr = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    broadcaster.take_events(); // clear create event

    let agent: AgentManagerHandle =
        Arc::new(MockAgent::with_confirmations(&conv.id, make_test_confirmations()));
    task_mgr.insert_agent(&conv.id, agent);

    let req = aionui_api_types::ConfirmRequest {
        msg_id: "msg-1".into(),
        data: json!({ "value": "allow" }),
        always_allow: false,
    };
    svc.confirm(
        "user_1",
        &conv.id,
        "call-1",
        req,
        &(task_mgr.clone() as Arc<dyn IWorkerTaskManager>),
    )
    .await
    .unwrap();

    // Confirmation should be removed from the agent
    let remaining = task_mgr
        .get_task(&conv.id)
        .unwrap()
        .get_confirmations();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].call_id, "call-2");

    // Should broadcast confirmation.remove event
    let events = broadcaster.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "confirmation.remove");
    assert_eq!(events[0].data["conversationId"], conv.id);
    assert_eq!(events[0].data["id"], "c1");
}

#[tokio::test]
async fn confirm_with_always_allow_stores_approval() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let agent: AgentManagerHandle =
        Arc::new(MockAgent::with_confirmations(&conv.id, make_test_confirmations()));
    task_mgr.insert_agent(&conv.id, agent);

    let req = aionui_api_types::ConfirmRequest {
        msg_id: "msg-1".into(),
        data: json!({ "value": "allow" }),
        always_allow: true,
    };
    let task_mgr_arc: Arc<dyn IWorkerTaskManager> = task_mgr.clone();
    svc.confirm("user_1", &conv.id, "call-1", req, &task_mgr_arc)
        .await
        .unwrap();

    // check_approval should now return true for edit_file:bash
    let agent = task_mgr.get_task(&conv.id).unwrap();
    assert!(agent.check_approval("edit_file", Some("bash")));
    assert!(!agent.check_approval("delete_file", None));
}

#[tokio::test]
async fn confirm_nonexistent_call_id_returns_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let agent: AgentManagerHandle =
        Arc::new(MockAgent::with_confirmations(&conv.id, make_test_confirmations()));
    task_mgr.insert_agent(&conv.id, agent);

    let req = aionui_api_types::ConfirmRequest {
        msg_id: "msg-1".into(),
        data: json!({ "value": "allow" }),
        always_allow: false,
    };
    let err = svc
        .confirm(
            "user_1",
            &conv.id,
            "nonexistent-call",
            req,
            &(task_mgr as Arc<dyn IWorkerTaskManager>),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn confirm_no_agent_returns_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let req = aionui_api_types::ConfirmRequest {
        msg_id: "msg-1".into(),
        data: json!({ "value": "allow" }),
        always_allow: false,
    };
    let err = svc
        .confirm("user_1", &conv.id, "call-1", req, &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn check_approval_returns_false_when_not_set() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let agent: AgentManagerHandle = Arc::new(MockAgent::new(&conv.id));
    task_mgr.insert_agent(&conv.id, agent);

    let result = svc
        .check_approval(
            "user_1",
            &conv.id,
            "edit_file",
            None,
            &(task_mgr as Arc<dyn IWorkerTaskManager>),
        )
        .await
        .unwrap();
    assert!(!result.approved);
}

#[tokio::test]
async fn check_approval_returns_true_after_always_allow() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let agent: AgentManagerHandle =
        Arc::new(MockAgent::with_confirmations(&conv.id, make_test_confirmations()));
    task_mgr.insert_agent(&conv.id, agent);

    // Confirm with always_allow
    let req = aionui_api_types::ConfirmRequest {
        msg_id: "msg-1".into(),
        data: json!({ "value": "allow" }),
        always_allow: true,
    };
    let task_mgr_arc: Arc<dyn IWorkerTaskManager> = task_mgr.clone();
    svc.confirm("user_1", &conv.id, "call-1", req, &task_mgr_arc)
        .await
        .unwrap();

    // Now check_approval should return true
    let result = svc
        .check_approval("user_1", &conv.id, "edit_file", Some("bash"), &task_mgr_arc)
        .await
        .unwrap();
    assert!(result.approved);
}

#[tokio::test]
async fn check_approval_returns_false_when_no_agent() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let result = svc
        .check_approval("user_1", &conv.id, "edit_file", None, &task_mgr)
        .await
        .unwrap();
    assert!(!result.approved);
}

#[tokio::test]
async fn check_approval_not_found() {
    let (svc, _broadcaster, _repo) = make_service();
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());

    let err = svc
        .check_approval("user_1", "no-such-id", "edit_file", None, &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}
