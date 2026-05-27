// Tests construct and read `ConversationRow.status` (Phase 2 deprecated)
// to verify legacy fixtures and to ensure runtime hot paths do NOT
// rely on it. Module-level allow keeps the deprecation lint active
// everywhere outside this file.
#![allow(deprecated)]

use std::sync::{Arc, Mutex};

use aionui_ai_agent::protocol::events::{AgentStreamEvent, FinishEventData, TextEventData};
use aionui_ai_agent::test_support::{MockConnector, MockConnectorFactory};
use aionui_ai_agent::{IAgentConnector, IAgentConnectorFactory};

use crate::response_middleware::{CronCommandResult, CronCreateParams, CronUpdateParams, ICronService};
use aionui_api_types::ConversationArtifactKind;
use aionui_api_types::{
    CloneConversationRequest, ConversationStatus, CreateConversationRequest, ListConversationsQuery,
    SearchMessagesQuery, SendMessageRequest, UpdateConversationRequest, WebSocketMessage,
};
use aionui_common::{AgentType, AppError, Confirmation, ConversationSource, PaginatedResult, TimestampMs};
use aionui_db::models::{
    AcpSessionRow, AgentMetadataRow, ConversationArtifactRow, ConversationRow, MessageRow, UpdateAgentHandshakeParams,
    UpsertAgentMetadataParams,
};
use aionui_db::{
    ConversationFilters, ConversationRowUpdate, CreateAcpSessionParams, DbError, IAcpSessionRepository,
    IAgentMetadataRepository, IConversationRepository, MessageRowUpdate, MessageSearchRow, PersistedSessionState,
    SaveRuntimeStateParams, SortOrder,
};
use aionui_realtime::EventBroadcaster;
use serde_json::json;

use crate::conv_service_trait::{ConversationEvent, IConversationService};
use crate::service::ConversationService;
use crate::skill_resolver::FixedSkillResolver;

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
    messages: Mutex<Vec<MessageRow>>,
    artifacts: Mutex<Vec<ConversationArtifactRow>>,
}

impl MockRepo {
    fn new() -> Self {
        Self {
            rows: Mutex::new(vec![]),
            messages: Mutex::new(vec![]),
            artifacts: Mutex::new(vec![]),
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

    async fn update(&self, id: &str, updates: &ConversationRowUpdate) -> Result<(), aionui_db::DbError> {
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
            row.status = Some(status.clone());
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
            .filter(|r| filters.pinned.as_ref().is_none_or(|&p| r.pinned == p))
            .cloned()
            .collect();
        let total = matched.len() as u64;
        let limit = filters.effective_limit() as usize;
        let items: Vec<_> = matched.into_iter().take(limit).collect();
        let has_more = (total as usize) > limit;
        Ok(PaginatedResult { items, total, has_more })
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
        conv_id: &str,
        page: u32,
        page_size: u32,
        order: SortOrder,
    ) -> Result<PaginatedResult<MessageRow>, aionui_db::DbError> {
        let messages = self.messages.lock().unwrap();
        let mut matched: Vec<_> = messages
            .iter()
            .filter(|message| message.conversation_id == conv_id)
            .cloned()
            .collect();
        matched.sort_by_key(|message| message.created_at);
        if matches!(order, SortOrder::Desc) {
            matched.reverse();
        }

        let start = page.saturating_sub(1) as usize * page_size as usize;
        let end = (start + page_size as usize).min(matched.len());
        let items = if start < matched.len() {
            matched[start..end].to_vec()
        } else {
            Vec::new()
        };
        Ok(PaginatedResult {
            items,
            total: matched.len() as u64,
            has_more: end < matched.len(),
        })
    }

    async fn insert_message(&self, message: &MessageRow) -> Result<(), aionui_db::DbError> {
        self.messages.lock().unwrap().push(message.clone());
        Ok(())
    }

    async fn update_message(&self, id: &str, updates: &MessageRowUpdate) -> Result<(), aionui_db::DbError> {
        let mut messages = self.messages.lock().unwrap();
        let message = messages
            .iter_mut()
            .find(|message| message.id == id)
            .ok_or_else(|| aionui_db::DbError::NotFound(format!("Message {id}")))?;

        if let Some(content) = &updates.content {
            message.content = content.clone();
        }
        if let Some(status) = &updates.status {
            message.status = status.clone();
        }
        if let Some(hidden) = updates.hidden {
            message.hidden = hidden;
        }
        Ok(())
    }

    async fn delete_messages_by_conversation(&self, conv_id: &str) -> Result<(), aionui_db::DbError> {
        self.messages
            .lock()
            .unwrap()
            .retain(|message| message.conversation_id != conv_id);
        Ok(())
    }

    async fn get_message_by_msg_id(
        &self,
        conv_id: &str,
        msg_id: &str,
        msg_type: &str,
    ) -> Result<Option<MessageRow>, aionui_db::DbError> {
        let messages = self.messages.lock().unwrap();
        Ok(messages
            .iter()
            .find(|message| {
                message.conversation_id == conv_id
                    && message.msg_id.as_deref() == Some(msg_id)
                    && message.r#type == msg_type
            })
            .cloned())
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

    async fn list_artifacts(&self, conversation_id: &str) -> Result<Vec<ConversationArtifactRow>, aionui_db::DbError> {
        Ok(self
            .artifacts
            .lock()
            .unwrap()
            .iter()
            .filter(|artifact| artifact.conversation_id == conversation_id)
            .cloned()
            .collect())
    }

    async fn get_artifact(
        &self,
        conversation_id: &str,
        artifact_id: &str,
    ) -> Result<Option<ConversationArtifactRow>, aionui_db::DbError> {
        Ok(self
            .artifacts
            .lock()
            .unwrap()
            .iter()
            .find(|artifact| artifact.conversation_id == conversation_id && artifact.id == artifact_id)
            .cloned())
    }

    async fn upsert_artifact(
        &self,
        artifact: &ConversationArtifactRow,
    ) -> Result<ConversationArtifactRow, aionui_db::DbError> {
        let mut artifacts = self.artifacts.lock().unwrap();
        if let Some(existing) = artifacts.iter_mut().find(|row| row.id == artifact.id) {
            *existing = artifact.clone();
            return Ok(existing.clone());
        }
        artifacts.push(artifact.clone());
        Ok(artifact.clone())
    }

    async fn update_artifact_status(
        &self,
        conversation_id: &str,
        artifact_id: &str,
        status: &str,
        updated_at: TimestampMs,
    ) -> Result<Option<ConversationArtifactRow>, aionui_db::DbError> {
        let mut artifacts = self.artifacts.lock().unwrap();
        let Some(existing) = artifacts
            .iter_mut()
            .find(|artifact| artifact.conversation_id == conversation_id && artifact.id == artifact_id)
        else {
            return Ok(None);
        };
        existing.status = status.to_owned();
        existing.updated_at = updated_at;
        Ok(Some(existing.clone()))
    }

    async fn mark_skill_suggest_artifacts_saved(
        &self,
        cron_job_id: &str,
        updated_at: TimestampMs,
    ) -> Result<Vec<ConversationArtifactRow>, aionui_db::DbError> {
        let mut artifacts = self.artifacts.lock().unwrap();
        let mut updated = Vec::new();
        for artifact in artifacts
            .iter_mut()
            .filter(|artifact| artifact.cron_job_id.as_deref() == Some(cron_job_id))
        {
            artifact.status = "saved".into();
            artifact.updated_at = updated_at;
            updated.push(artifact.clone());
        }
        Ok(updated)
    }

    async fn delete_artifacts_by_conversation(&self, conversation_id: &str) -> Result<(), aionui_db::DbError> {
        self.artifacts
            .lock()
            .unwrap()
            .retain(|artifact| artifact.conversation_id != conversation_id);
        Ok(())
    }

    async fn list_legacy_cron_trigger_messages(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<MessageRow>, aionui_db::DbError> {
        Ok(self
            .messages
            .lock()
            .unwrap()
            .iter()
            .filter(|message| message.conversation_id == conversation_id && message.r#type == "cron_trigger")
            .cloned()
            .collect())
    }
}

// ── Helpers ────────────────────────────────────────────────────────

/// Stub repository for tests — every lookup returns `None` so the
/// service falls back to `AgentType::native_skills_dirs()` paths.
struct StubAgentMetadataRepo;

#[async_trait::async_trait]
impl IAgentMetadataRepository for StubAgentMetadataRepo {
    async fn list_all(&self) -> Result<Vec<AgentMetadataRow>, DbError> {
        Ok(Vec::new())
    }
    async fn get(&self, _id: &str) -> Result<Option<AgentMetadataRow>, DbError> {
        Ok(None)
    }
    async fn find_by_source_and_name(
        &self,
        _agent_source: &str,
        _name: &str,
    ) -> Result<Option<AgentMetadataRow>, DbError> {
        Ok(None)
    }
    async fn find_builtin_by_backend(&self, _backend: &str) -> Result<Option<AgentMetadataRow>, DbError> {
        Ok(None)
    }
    async fn upsert(&self, _params: &UpsertAgentMetadataParams<'_>) -> Result<AgentMetadataRow, DbError> {
        Err(DbError::Init("stub".into()))
    }
    async fn apply_handshake(
        &self,
        _id: &str,
        _params: &UpdateAgentHandshakeParams<'_>,
    ) -> Result<Option<AgentMetadataRow>, DbError> {
        Ok(None)
    }
    async fn set_enabled(&self, _id: &str, _enabled: bool) -> Result<bool, DbError> {
        Ok(false)
    }
    async fn delete(&self, _id: &str) -> Result<bool, DbError> {
        Ok(false)
    }
}

struct StubAcpSessionRepo;

#[async_trait::async_trait]
impl IAcpSessionRepository for StubAcpSessionRepo {
    async fn get(&self, _conversation_id: &str) -> Result<Option<AcpSessionRow>, DbError> {
        Ok(None)
    }
    async fn create(&self, _params: &CreateAcpSessionParams<'_>) -> Result<AcpSessionRow, DbError> {
        // Return a synthetic row so `ConversationService::create` can
        // succeed for ACP conversations in unit tests.
        Ok(AcpSessionRow {
            conversation_id: "stub".into(),
            agent_backend: "stub".into(),
            agent_source: "stub".into(),
            agent_id: "stub".into(),
            session_id: None,
            session_status: "idle".into(),
            session_config: "{}".into(),
            last_active_at: None,
            suspended_at: None,
        })
    }
    async fn update_session_id(&self, _conversation_id: &str, _session_id: &str) -> Result<bool, DbError> {
        Ok(false)
    }
    async fn delete(&self, _conversation_id: &str) -> Result<bool, DbError> {
        Ok(false)
    }
    async fn load_runtime_state(&self, _conversation_id: &str) -> Result<Option<PersistedSessionState>, DbError> {
        Ok(None)
    }
    async fn save_runtime_state(
        &self,
        _conversation_id: &str,
        _params: &SaveRuntimeStateParams<'_>,
    ) -> Result<bool, DbError> {
        Ok(false)
    }
}

fn make_service() -> (
    ConversationService,
    Arc<MockBroadcaster>,
    Arc<MockRepo>,
    Arc<dyn IAgentConnectorFactory>,
) {
    make_service_with_resolver(Arc::new(FixedSkillResolver { names: vec![] }))
}

fn make_service_with_resolver(
    skill_resolver: Arc<dyn crate::skill_resolver::SkillResolver>,
) -> (
    ConversationService,
    Arc<MockBroadcaster>,
    Arc<MockRepo>,
    Arc<dyn IAgentConnectorFactory>,
) {
    let repo = Arc::new(MockRepo::new());
    let broadcaster = Arc::new(MockBroadcaster::new());
    let agent_metadata_repo: Arc<dyn IAgentMetadataRepository> = Arc::new(StubAgentMetadataRepo);
    let acp_session_repo: Arc<dyn IAcpSessionRepository> = Arc::new(StubAcpSessionRepo);
    let factory: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();
    let svc = ConversationService::new(
        std::env::temp_dir(),
        broadcaster.clone(),
        skill_resolver,
        factory.clone(),
        repo.clone(),
        agent_metadata_repo,
        acp_session_repo,
    );
    (svc, broadcaster, repo, factory)
}

fn make_create_req() -> CreateConversationRequest {
    serde_json::from_value(json!({
        "type": "acp",
        "extra": { "workspace": "/project" }
    }))
    .unwrap()
}

// ── Create tests ───────────────────────────────────────────────────

#[tokio::test]
async fn create_returns_conversation_with_defaults() {
    let (svc, broadcaster, _repo, _task_mgr) = make_service();

    let resp = svc.create("user_1", make_create_req()).await.unwrap();

    assert!(!resp.id.is_empty());
    assert_eq!(resp.r#type, AgentType::Acp);
    // A freshly created conversation has no ConvActor entry; the response
    // status falls back to the DB row's "pending" so the wire format keeps
    // signalling never-opened to the frontend. The actor-derived status
    // takes over on first turn.
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
    assert_eq!(events[0].data["conversation_id"], resp.id);
    assert_eq!(events[0].data["source"], "aionui");
}

#[tokio::test]
async fn create_with_custom_name_and_source() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();

    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "acp",
        "name": "Custom Name",
        "source": "telegram",
        "channel_chat_id": "chat:123",
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
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();

    // Top-level model is only valid for aionrs conversations.
    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "aionrs",
        "model": { "provider_id": "p1", "model": "m1" },
        "extra": { "workspace": "/project" }
    }))
    .unwrap();
    let resp = svc.create("user_1", req).await.unwrap();

    let model = resp.model.unwrap();
    assert_eq!(model.provider_id, "p1");
    assert_eq!(model.model, "m1");
}

// ── Get tests ──────────────────────────────────────────────────────

#[tokio::test]
async fn get_existing_conversation() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let created = svc.create("user_1", make_create_req()).await.unwrap();

    let fetched = svc.get("user_1", &created.id).await.unwrap();
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.name, created.name);
}

#[tokio::test]
async fn get_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let err = svc.get("user_1", "non-existent").await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

// ── List tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn list_empty() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let result = svc.list("user_1", ListConversationsQuery::default()).await.unwrap();
    assert!(result.items.is_empty());
    assert_eq!(result.total, 0);
    assert!(!result.has_more);
}

#[tokio::test]
async fn list_returns_created_conversations() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    svc.create("user_1", make_create_req()).await.unwrap();
    svc.create("user_1", make_create_req()).await.unwrap();

    let result = svc.list("user_1", ListConversationsQuery::default()).await.unwrap();
    assert_eq!(result.items.len(), 2);
    assert_eq!(result.total, 2);
}

#[tokio::test]
async fn list_filters_by_user() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    svc.create("user_1", make_create_req()).await.unwrap();
    svc.create("user_2", make_create_req()).await.unwrap();

    let result = svc.list("user_1", ListConversationsQuery::default()).await.unwrap();
    assert_eq!(result.items.len(), 1);
}

#[tokio::test]
async fn list_with_source_filter() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    svc.create("user_1", make_create_req()).await.unwrap();

    let telegram_req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "acp",
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
    assert_eq!(result.items[0].source, Some(ConversationSource::Telegram));
}

#[tokio::test]
async fn list_with_pinned_filter() {
    let (svc, _broadcaster, _repo, task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    svc.create("user_1", make_create_req()).await.unwrap();

    // Pin the first one
    let update_req: UpdateConversationRequest = serde_json::from_value(json!({ "pinned": true })).unwrap();
    svc.update("user_1", &conv.id, update_req, &task_mgr).await.unwrap();

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
    let (svc, broadcaster, _repo, task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    broadcaster.take_events(); // clear create event

    let req: UpdateConversationRequest = serde_json::from_value(json!({ "name": "New Name" })).unwrap();
    let updated = svc.update("user_1", &conv.id, req, &task_mgr).await.unwrap();

    assert_eq!(updated.name, "New Name");
    assert!(updated.modified_at >= conv.modified_at);

    let events = broadcaster.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data["action"], "updated");
}

#[tokio::test]
async fn update_pin() {
    let (svc, _broadcaster, _repo, task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    assert!(!conv.pinned);

    let req: UpdateConversationRequest = serde_json::from_value(json!({ "pinned": true })).unwrap();
    let updated = svc.update("user_1", &conv.id, req, &task_mgr).await.unwrap();
    assert!(updated.pinned);
    assert!(updated.pinned_at.is_some());
}

#[tokio::test]
async fn update_unpin_clears_pinned_at() {
    let (svc, _broadcaster, _repo, task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    // Pin first
    let pin_req: UpdateConversationRequest = serde_json::from_value(json!({ "pinned": true })).unwrap();
    let pinned = svc.update("user_1", &conv.id, pin_req, &task_mgr).await.unwrap();
    assert!(pinned.pinned);
    assert!(pinned.pinned_at.is_some());

    // Unpin
    let unpin_req: UpdateConversationRequest = serde_json::from_value(json!({ "pinned": false })).unwrap();
    let unpinned = svc.update("user_1", &conv.id, unpin_req, &task_mgr).await.unwrap();
    assert!(!unpinned.pinned);
    assert!(unpinned.pinned_at.is_none());
}

#[tokio::test]
async fn update_extra_merge() {
    let (svc, _broadcaster, _repo, task_mgr) = make_service();

    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "acp",
        "extra": { "workspace": "/old", "contextFileName": "ctx.md" }
    }))
    .unwrap();
    let conv = svc.create("user_1", req).await.unwrap();

    // Update only workspace — contextFileName should be preserved
    let update_req: UpdateConversationRequest =
        serde_json::from_value(json!({ "extra": { "workspace": "/new" } })).unwrap();
    let updated = svc.update("user_1", &conv.id, update_req, &task_mgr).await.unwrap();

    assert_eq!(updated.extra["workspace"], "/new");
    assert_eq!(updated.extra["contextFileName"], "ctx.md");
}

#[tokio::test]
async fn update_model() {
    let (svc, _broadcaster, _repo, task_mgr) = make_service();

    // Top-level model updates are only valid on aionrs conversations
    // (Task 8 enforces the aionrs-only rule in update).
    let create_req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "aionrs",
        "model": { "provider_id": "p1", "model": "m1" },
        "extra": { "workspace": "/project" }
    }))
    .unwrap();
    let conv = svc.create("user_1", create_req).await.unwrap();

    let req: UpdateConversationRequest = serde_json::from_value(json!({
        "model": { "provider_id": "p2", "model": "new-model" }
    }))
    .unwrap();
    let updated = svc.update("user_1", &conv.id, req, &task_mgr).await.unwrap();

    let model = updated.model.unwrap();
    assert_eq!(model.provider_id, "p2");
    assert_eq!(model.model, "new-model");
}

#[tokio::test]
async fn update_not_found() {
    let (svc, _broadcaster, _repo, task_mgr) = make_service();
    let req: UpdateConversationRequest = serde_json::from_value(json!({ "name": "x" })).unwrap();
    let err = svc.update("user_1", "non-existent", req, &task_mgr).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

// ── Delete tests ───────────────────────────────────────────────────

#[tokio::test]
async fn delete_conversation() {
    let (svc, broadcaster, _repo, _task_mgr) = make_service();
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
    assert_eq!(events[0].data["conversation_id"], conv.id);
}

#[tokio::test]
async fn delete_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let err = svc.delete("user_1", "non-existent").await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn delete_invokes_registered_hook() {
    use aionui_common::OnConversationDelete;

    struct RecordingHook(Mutex<Vec<String>>);
    #[async_trait::async_trait]
    impl OnConversationDelete for RecordingHook {
        async fn on_conversation_deleted(&self, conversation_id: &str) {
            self.0.lock().unwrap().push(conversation_id.to_owned());
        }
    }

    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let hook = Arc::new(RecordingHook(Mutex::new(vec![])));
    svc.with_delete_hook(hook.clone());

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    svc.delete("user_1", &conv.id).await.unwrap();

    let calls = hook.0.lock().unwrap();
    assert_eq!(calls.as_slice(), &[conv.id]);
}

// ── Broadcast payload tests ────────────────────────────────────────

#[tokio::test]
async fn broadcast_includes_source_on_delete() {
    let (svc, broadcaster, _repo, _task_mgr) = make_service();

    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "acp",
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
    let (svc, broadcaster, _repo, task_mgr) = make_service();

    // Create
    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let events = broadcaster.take_events();
    assert_eq!(events[0].data["action"], "created");

    // Update
    let req: UpdateConversationRequest = serde_json::from_value(json!({ "name": "x" })).unwrap();
    svc.update("user_1", &conv.id, req, &task_mgr).await.unwrap();
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
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let err = svc.get("user_2", &conv.id).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn update_wrong_user_returns_not_found() {
    let (svc, _broadcaster, _repo, task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let req: UpdateConversationRequest = serde_json::from_value(json!({ "name": "hacked" })).unwrap();
    let err = svc.update("user_2", &conv.id, req, &task_mgr).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));

    // Original should be unchanged
    let original = svc.get("user_1", &conv.id).await.unwrap();
    assert_ne!(original.name, "hacked");
}

#[tokio::test]
async fn delete_wrong_user_returns_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
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
    let (svc, broadcaster, _repo, _task_mgr) = make_service();

    let req: CloneConversationRequest = serde_json::from_value(json!({
        "conversation": {
            "type": "acp",
            "name": "Cloned",
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

// ── Reset tests ───────────────────────────────────────────────────

#[tokio::test]
async fn reset_sets_status_to_pending() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    svc.reset("user_1", &conv.id).await.unwrap();

    let fetched = svc.get("user_1", &conv.id).await.unwrap();
    // reset() writes DB.status="pending" and clears any actor; with no
    // actor entry the response status falls back to the DB column, so the
    // wire format reports `Pending`. The DTO enum lives in
    // `aionui_api_types::ConversationStatus`; the runtime truth is
    // `aionui_conversation::ConversationStatus` on `ConvActor`.
    assert_eq!(fetched.status, ConversationStatus::Pending);
}

#[tokio::test]
async fn reset_clears_conversation_artifacts() {
    let (svc, _broadcaster, repo, _task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    repo.upsert_artifact(&ConversationArtifactRow {
        id: format!("{}:skill_suggest:cron_1", conv.id),
        conversation_id: conv.id.clone(),
        cron_job_id: Some("cron_1".into()),
        kind: "skill_suggest".into(),
        status: "pending".into(),
        payload: json!({ "cron_job_id": "cron_1", "name": "daily-report" }).to_string(),
        created_at: 1000,
        updated_at: 1000,
    })
    .await
    .unwrap();

    svc.reset("user_1", &conv.id).await.unwrap();

    let artifacts = repo.list_artifacts(&conv.id).await.unwrap();
    assert!(artifacts.is_empty());
}

#[tokio::test]
async fn list_artifacts_includes_legacy_cron_trigger_messages() {
    let (svc, _broadcaster, repo, _task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    repo.insert_message(&MessageRow {
        id: "legacy-msg-1".into(),
        conversation_id: conv.id.clone(),
        msg_id: Some("legacy-trigger-1".into()),
        r#type: "cron_trigger".into(),
        content: json!({
            "cron_job_id": "cron_1",
            "cron_job_name": "Daily Report",
            "triggered_at": 1234
        })
        .to_string(),
        position: Some("center".into()),
        status: Some("finish".into()),
        hidden: false,
        created_at: 1234,
    })
    .await
    .unwrap();

    let artifacts = svc.list_artifacts("user_1", &conv.id).await.unwrap();

    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].kind, ConversationArtifactKind::CronTrigger);
    assert_eq!(artifacts[0].payload["cron_job_id"], "cron_1");
    assert_eq!(artifacts[0].payload["cron_job_name"], "Daily Report");
}

#[tokio::test]
async fn reset_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let err = svc.reset("user_1", "no-such-id").await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn reset_wrong_user() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let err = svc.reset("user_2", &conv.id).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

// ── Search validation tests ───────────────────────────────────────

#[tokio::test]
async fn search_messages_empty_keyword_returns_bad_request() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();

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
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();

    let query = SearchMessagesQuery {
        keyword: "   ".into(),
        page: None,
        page_size: None,
    };
    let err = svc.search_messages("user_1", query).await.unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

// ── Mock connector helpers ───────────────────────────────────────
//
// Phase 5: previously this file defined `MockAgent` (impl `IAgentTask` +
// `IMockAgent`), `MockTaskManager` / `MockTaskManagerWithWorkspace`
// (impl `IWorkerTaskManager`), and `ScriptedAgent`. With the legacy
// trait surface gone, those collapse into the shared `MockConnector`
// fixture in `aionui_ai_agent::test_support`. The thin helper functions
// below preserve the callsite ergonomics the existing tests rely on.

use aionui_ai_agent::test_support::MockConnectorBuilder;
use std::sync::Arc as StdArc;

fn insert_mock_connector(factory: &Arc<MockConnectorFactory>, conversation_id: &str) -> StdArc<MockConnector> {
    let connector = MockConnector::builder(conversation_id).build_arc();
    factory.insert(
        conversation_id,
        connector.clone() as Arc<dyn aionui_ai_agent::IAgentConnector>,
    );
    connector
}

fn insert_mock_with_confirmations(
    factory: &Arc<MockConnectorFactory>,
    conversation_id: &str,
    confirmations: Vec<Confirmation>,
) -> StdArc<MockConnector> {
    let connector = MockConnector::builder(conversation_id)
        .confirmations(confirmations)
        .build_arc();
    factory.insert(
        conversation_id,
        connector.clone() as Arc<dyn aionui_ai_agent::IAgentConnector>,
    );
    connector
}

fn insert_mock_with_direct_confirm(
    factory: &Arc<MockConnectorFactory>,
    conversation_id: &str,
) -> StdArc<MockConnector> {
    let connector = MockConnector::builder(conversation_id)
        .allow_direct_confirm()
        .build_arc();
    factory.insert(
        conversation_id,
        connector.clone() as Arc<dyn aionui_ai_agent::IAgentConnector>,
    );
    connector
}

fn insert_scripted_connector(
    factory: &Arc<MockConnectorFactory>,
    conversation_id: &str,
    scripts: Vec<Vec<AgentStreamEvent>>,
) -> StdArc<MockConnector> {
    let mut builder = MockConnectorBuilder::new(conversation_id).status(ConversationStatus::Finished);
    for script in scripts {
        builder = builder.script(script);
    }
    let connector = builder.build_arc();
    factory.insert(
        conversation_id,
        connector.clone() as Arc<dyn aionui_ai_agent::IAgentConnector>,
    );
    connector
}

struct MockCronContinuationService;

#[async_trait::async_trait]
impl ICronService for MockCronContinuationService {
    async fn create_job(&self, _user_id: &str, _conversation_id: &str, params: &CronCreateParams) -> CronCommandResult {
        CronCommandResult {
            success: true,
            message: format!("Created cron job '{}'", params.name),
        }
    }

    async fn update_job(
        &self,
        _user_id: &str,
        _conversation_id: &str,
        _params: &CronUpdateParams,
    ) -> CronCommandResult {
        CronCommandResult {
            success: true,
            message: "Updated cron job".into(),
        }
    }

    async fn list_jobs(&self, _user_id: &str, _conversation_id: &str) -> CronCommandResult {
        CronCommandResult {
            success: true,
            message: "No scheduled tasks".into(),
        }
    }

    async fn delete_job(&self, _user_id: &str, _job_id: &str) -> CronCommandResult {
        CronCommandResult {
            success: true,
            message: "Deleted cron job".into(),
        }
    }
}

// ── send_message tests ──────────────────────────────────────────

fn make_send_req() -> SendMessageRequest {
    serde_json::from_value(json!({
        "content": "Hello"
    }))
    .unwrap()
}

#[tokio::test]
async fn send_message_returns_accepted() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let msg_id = svc
        .send_message("user_1", &conv.id, make_send_req(), &task_mgr)
        .await
        .unwrap();

    assert!(!msg_id.is_empty(), "msg_id must be non-empty");
    assert_eq!(msg_id.len(), 8, "msg_id should be an 8-char short hex ID");
}

#[tokio::test]
async fn send_message_broadcasts_user_created_event() {
    let (svc, broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    // Clear events from create
    broadcaster.take_events();

    let msg_id = svc
        .send_message("user_1", &conv.id, make_send_req(), &task_mgr)
        .await
        .unwrap();

    let events = broadcaster.take_events();
    let user_created = events
        .iter()
        .find(|e| e.name == "message.userCreated")
        .expect("should broadcast message.userCreated event");

    assert_eq!(user_created.data["conversation_id"], conv.id);
    assert_eq!(user_created.data["msg_id"], msg_id);
    assert_eq!(user_created.data["content"], "Hello");
    assert_eq!(user_created.data["position"], "right");
}

#[tokio::test]
async fn send_message_persists_hidden_user_message_when_requested() {
    let (svc, _broadcaster, repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let req: SendMessageRequest = serde_json::from_value(json!({
        "content": "Hidden cron prompt",
        "hidden": true
    }))
    .unwrap();

    svc.send_message("user_1", &conv.id, req, &task_mgr).await.unwrap();

    let messages = repo.get_messages(&conv.id, 1, 20, SortOrder::Asc).await.unwrap().items;
    // The user message is the only hidden text row written by the service.
    let user_message = messages
        .iter()
        .find(|message| message.r#type == "text" && message.position.as_deref() == Some("right"))
        .expect("user message should be persisted");
    assert!(user_message.hidden);
    // msg_id is server-generated and must be non-empty for frontend routing.
    assert!(user_message.msg_id.as_deref().is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn send_message_empty_content_returns_bad_request() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let req: SendMessageRequest = serde_json::from_value(json!({
        "content": ""
    }))
    .unwrap();

    let err = svc.send_message("user_1", &conv.id, req, &task_mgr).await.unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

#[tokio::test]
async fn send_message_whitespace_content_returns_bad_request() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let req: SendMessageRequest = serde_json::from_value(json!({
        "content": "   "
    }))
    .unwrap();

    let err = svc.send_message("user_1", &conv.id, req, &task_mgr).await.unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

#[tokio::test]
async fn send_message_conversation_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let err = svc
        .send_message("user_1", "no-such-id", make_send_req(), &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn send_message_wrong_user_returns_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let err = svc
        .send_message("user_2", &conv.id, make_send_req(), &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn send_message_running_conversation_returns_conflict() {
    // Phase 2: the in-flight guard is now ConvActor's mutex, not DB.status.
    // We hold a TurnHandle for the conversation's actor and confirm that
    // send_message refuses with 409. Crucially, we leave the DB row's
    // status field untouched (and even set it to a corrupt value) to lock
    // in that the runtime-state guard does not consult the DB column.
    let (svc, _broadcaster, repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    // Corrupt the DB.status column. If the legacy guard ever sneaks back
    // in, this would either deserialize-fail or return a stale value.
    let corrupt_update = ConversationRowUpdate {
        status: Some("garbage-not-a-status".into()),
        ..Default::default()
    };
    repo.update(&conv.id, &corrupt_update).await.unwrap();

    // Hold the actor's turn slot so send_message must collide with us.
    let actor = svc.get_or_create_actor(&conv.id);
    actor.mark_idle().await;
    let _held = actor.begin_turn("msg-already-running".into()).await.unwrap();

    let err = svc
        .send_message("user_1", &conv.id, make_send_req(), &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)));
}

#[tokio::test]
async fn send_message_persists_factory_resolved_workspace() {
    // Conversation created with no workspace → create() auto-assigns one.
    // Factory resolves a *different* temp dir (simulating legacy-conv fallback).
    // After send_message, conversation.extra.workspace must match what the
    // agent reports.
    let (svc, _broadcaster, repo, _default_task_mgr) = make_service();
    let auto_workspace = "/tmp/factory-resolved";
    let task_mgr: Arc<dyn IAgentConnectorFactory> =
        MockConnectorFactory::builder().fixed_workspace(auto_workspace).build();

    // Create a conversation with an empty workspace to simulate legacy case.
    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "acp",
        "extra": {}
    }))
    .unwrap();
    let conv = svc.create("user_1", req).await.unwrap();

    // Inject an empty workspace directly into the repo to mimic legacy state.
    let empty_ws_update = ConversationRowUpdate {
        extra: Some(r#"{"workspace":""}"#.to_owned()),
        ..Default::default()
    };
    repo.update(&conv.id, &empty_ws_update).await.unwrap();

    svc.send_message("user_1", &conv.id, make_send_req(), &task_mgr)
        .await
        .unwrap();

    // Verify the workspace was written back.
    let updated = svc.get("user_1", &conv.id).await.unwrap();
    assert_eq!(updated.extra["workspace"], auto_workspace);
}

#[tokio::test]
async fn send_message_is_single_turn_with_system_responses() {
    // Phase 4: conv layer no longer chains continuations. A single
    // `send_message` dispatches exactly ONE turn. `system_responses`
    // captured by the relay are forwarded via the `TurnCompleted` event
    // for biz-layer subscribers (e.g. `CronContinuationOrchestrator`)
    // to act on — the conv layer must NOT re-enter `send_message`
    // itself.
    let (svc, broadcaster, _repo, _default_task_mgr) = make_service();
    let task_mgr = MockConnectorFactory::builder().build();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let scripted_agent = insert_scripted_connector(
        &task_mgr,
        &conv.id,
        vec![vec![
            AgentStreamEvent::Text(TextEventData {
                content: "I'll check. [CRON_LIST]".into(),
            }),
            AgentStreamEvent::Finish(FinishEventData::default()),
        ]],
    );
    svc.with_cron_service(Some(Arc::new(MockCronContinuationService)));

    // Subscribe BEFORE send so we don't miss TurnStarted/TurnCompleted.
    // Use the trait surface to mirror how biz-layer subscribers do it.
    let mut rx = IConversationService::subscribe(&svc, &conv.id);

    let task_mgr_dyn: Arc<dyn IAgentConnectorFactory> = task_mgr.clone();
    let req: SendMessageRequest = serde_json::from_value(json!({
        "content": "Create the task now"
    }))
    .unwrap();

    svc.send_message("user_1", &conv.id, req, &task_mgr_dyn).await.unwrap();

    // Wait for exactly ONE send through to the agent.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if !scripted_agent.sent_contents().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    // Drain the lifecycle event stream: TurnStarted → TurnCompleted.
    let evt1 = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(evt1, ConversationEvent::TurnStarted { .. }));

    let evt2 = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let captured_responses = match evt2 {
        ConversationEvent::TurnCompleted { system_responses, .. } => system_responses,
        other => panic!("expected TurnCompleted, got {other:?}"),
    };
    assert_eq!(
        captured_responses,
        vec!["[System: No scheduled tasks]".to_string()],
        "TurnCompleted must surface the relay-captured system_responses"
    );

    // CRUCIAL: conv layer must not chain a second turn. Give the
    // would-be loop time to fire and verify nothing else arrives.
    let next = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(next.is_err(), "conv layer must not chain a second turn (got {next:?})");
    assert_eq!(
        scripted_agent.sent_contents().len(),
        1,
        "send_message must invoke the agent exactly once per call"
    );

    let finished = svc.get("user_1", &conv.id).await.unwrap();
    // Phase 2 Task 6: response.status is derived from the runtime ConvActor.
    // After the turn task exits, the actor is `Idle`, which the legacy
    // three-state mapping flattens to `Finished` for client compat.
    assert_eq!(finished.status, ConversationStatus::Finished);

    let events = broadcaster.take_events();
    let turn_completed = events.iter().filter(|evt| evt.name == "turn.completed").count();
    assert_eq!(turn_completed, 1);
}

// ── stop_stream tests ───────────────────────────────────────────

#[tokio::test]
async fn stop_stream_with_active_agent() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    // Build agent via send_message
    svc.send_message(
        "user_1",
        &conv.id,
        make_send_req(),
        &(task_mgr.clone() as Arc<dyn IAgentConnectorFactory>),
    )
    .await
    .unwrap();

    // Stop should succeed since agent exists
    let result = svc
        .cancel("user_1", &conv.id, &(task_mgr as Arc<dyn IAgentConnectorFactory>))
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn stop_stream_conversation_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let err = svc.cancel("user_1", "no-such-id", &task_mgr).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn stop_stream_no_active_agent_is_idempotent() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let result = svc.cancel("user_1", &conv.id, &task_mgr).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn stop_stream_wrong_user_returns_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let err = svc.cancel("user_2", &conv.id, &task_mgr).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

// ── warmup tests ────────────────────────────────────────────────

#[tokio::test]
async fn warmup_creates_agent_task() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let result = svc
        .warmup(
            "user_1",
            &conv.id,
            &(task_mgr.clone() as Arc<dyn IAgentConnectorFactory>),
        )
        .await;
    assert!(result.is_ok());

    // Agent should now exist
    assert!(task_mgr.get(&conv.id).is_some());
}

#[tokio::test]
async fn warmup_conversation_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let err = svc.warmup("user_1", "no-such-id", &task_mgr).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn warmup_wrong_user_returns_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let err = svc.warmup("user_2", &conv.id, &task_mgr).await.unwrap_err();
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
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let result = svc.list_confirmations("user_1", &conv.id, &task_mgr).await.unwrap();
    assert!(result.is_empty());
}

#[tokio::test]
async fn list_confirmations_returns_items() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let _agent = insert_mock_with_confirmations(&task_mgr, &conv.id, make_test_confirmations());

    let result = svc
        .list_confirmations("user_1", &conv.id, &(task_mgr as Arc<dyn IAgentConnectorFactory>))
        .await
        .unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].call_id, "call-1");
    assert_eq!(result[1].call_id, "call-2");
}

#[tokio::test]
async fn list_confirmations_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let err = svc
        .list_confirmations("user_1", "no-such-id", &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn list_confirmations_wrong_user() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let err = svc.list_confirmations("user_2", &conv.id, &task_mgr).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn confirm_removes_confirmation_and_broadcasts() {
    let (svc, broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    broadcaster.take_events(); // clear create event

    let agent = insert_mock_with_confirmations(&task_mgr, &conv.id, make_test_confirmations());

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
        &(task_mgr.clone() as Arc<dyn IAgentConnectorFactory>),
    )
    .await
    .unwrap();

    // Confirmation should be removed from the agent
    let remaining = agent.get_confirmations();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].call_id, "call-2");

    // Should broadcast confirmation.remove event
    let events = broadcaster.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "confirmation.remove");
    assert_eq!(events[0].data["conversation_id"], conv.id);
    assert_eq!(events[0].data["id"], "c1");
}

#[tokio::test]
async fn confirm_with_always_allow_stores_approval() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let agent = insert_mock_with_confirmations(&task_mgr, &conv.id, make_test_confirmations());

    let req = aionui_api_types::ConfirmRequest {
        msg_id: "msg-1".into(),
        data: json!({ "value": "allow" }),
        always_allow: true,
    };
    let task_mgr_arc: Arc<dyn IAgentConnectorFactory> = task_mgr.clone();
    svc.confirm("user_1", &conv.id, "call-1", req, &task_mgr_arc)
        .await
        .unwrap();

    // check_approval should now return true for edit_file:bash
    assert!(agent.check_approval("edit_file", Some("bash")));
    assert!(!agent.check_approval("delete_file", None));
}

#[tokio::test]
async fn confirm_nonexistent_call_id_returns_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let _agent = insert_mock_with_confirmations(&task_mgr, &conv.id, make_test_confirmations());

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
            &(task_mgr as Arc<dyn IAgentConnectorFactory>),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn confirm_without_confirmation_state_still_calls_agent() {
    let (svc, broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    broadcaster.take_events();

    let _agent = insert_mock_with_direct_confirm(&task_mgr, &conv.id);

    let req = aionui_api_types::ConfirmRequest {
        msg_id: "msg-1".into(),
        data: json!("allow_once"),
        always_allow: false,
    };
    svc.confirm(
        "user_1",
        &conv.id,
        "call-1",
        req,
        &(task_mgr.clone() as Arc<dyn IAgentConnectorFactory>),
    )
    .await
    .unwrap();

    assert!(broadcaster.take_events().is_empty());
}

#[tokio::test]
async fn confirm_no_agent_returns_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

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
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let _agent = insert_mock_connector(&task_mgr, &conv.id);

    let result = svc
        .check_approval(
            "user_1",
            &conv.id,
            "edit_file",
            None,
            &(task_mgr as Arc<dyn IAgentConnectorFactory>),
        )
        .await
        .unwrap();
    assert!(!result.approved);
}

#[tokio::test]
async fn check_approval_returns_true_after_always_allow() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let _agent = insert_mock_with_confirmations(&task_mgr, &conv.id, make_test_confirmations());

    // Confirm with always_allow
    let req = aionui_api_types::ConfirmRequest {
        msg_id: "msg-1".into(),
        data: json!({ "value": "allow" }),
        always_allow: true,
    };
    let task_mgr_arc: Arc<dyn IAgentConnectorFactory> = task_mgr.clone();
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
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let result = svc
        .check_approval("user_1", &conv.id, "edit_file", None, &task_mgr)
        .await
        .unwrap();
    assert!(!result.approved);
}

#[tokio::test]
async fn check_approval_not_found() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();

    let err = svc
        .check_approval("user_1", "no-such-id", "edit_file", None, &task_mgr)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

// ── Skill snapshot tests ───────────────────────────────────────────

#[tokio::test]
async fn create_writes_extra_skills_from_auto_inject_and_preset() {
    let resolver = Arc::new(FixedSkillResolver {
        names: vec!["cron".into(), "todo-tracker".into()],
    });
    let (svc, _broadcaster, _repo, _task_mgr) = make_service_with_resolver(resolver);

    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "acp",
        "name": "t",
        "extra": {
            "workspace": "/project",
            "backend": "claude",
            "preset_enabled_skills": ["pdf", "cron"],
            "exclude_auto_inject_skills": ["todo-tracker"],
        },
    }))
    .unwrap();
    let resp = svc.create("user-1", req).await.unwrap();

    assert_eq!(resp.extra["skills"], json!(["cron", "pdf"]));
    assert!(resp.extra.get("preset_enabled_skills").is_none());
    assert!(resp.extra.get("exclude_auto_inject_skills").is_none());
}

#[tokio::test]
async fn create_writes_empty_skills_when_no_auto_inject_and_no_preset() {
    let resolver = Arc::new(FixedSkillResolver { names: vec![] });
    let (svc, _broadcaster, _repo, _task_mgr) = make_service_with_resolver(resolver);

    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "acp",
        "extra": { "workspace": "/project", "backend": "claude" },
    }))
    .unwrap();
    let resp = svc.create("user-1", req).await.unwrap();

    assert_eq!(resp.extra["skills"], json!([]));
}

#[tokio::test]
async fn update_rejects_extra_skills() {
    let (svc, _broadcaster, _repo, task_mgr) = make_service();

    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "acp",
        "extra": { "workspace": "/project", "backend": "claude" },
    }))
    .unwrap();
    let resp = svc.create("u", req).await.unwrap();

    let update_req: UpdateConversationRequest = serde_json::from_value(json!({
        "extra": { "skills": ["cron"] },
    }))
    .unwrap();
    let err = svc.update("u", &resp.id, update_req, &task_mgr).await.unwrap_err();

    match err {
        AppError::BadRequest(msg) => assert!(msg.contains("skills"), "msg = {msg:?}"),
        other => panic!("expected BadRequest, got {other:?}"),
    }
}

#[tokio::test]
async fn update_allows_other_extra_fields() {
    let (svc, _broadcaster, _repo, task_mgr) = make_service();

    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "acp",
        "extra": { "workspace": "/project", "backend": "claude" },
    }))
    .unwrap();
    let resp = svc.create("u", req).await.unwrap();

    let update_req: UpdateConversationRequest = serde_json::from_value(json!({
        "extra": { "current_model_id": "claude-3-5-sonnet" },
    }))
    .unwrap();
    let updated = svc.update("u", &resp.id, update_req, &task_mgr).await.unwrap();

    assert_eq!(updated.extra["current_model_id"], "claude-3-5-sonnet");
}

#[tokio::test]
async fn get_backfills_legacy_row_and_persists() {
    let resolver = Arc::new(FixedSkillResolver {
        names: vec!["cron".into(), "todo-tracker".into()],
    });
    let (svc, _broadcaster, repo, _task_mgr) = make_service_with_resolver(resolver);

    // Seed a legacy row directly via the repo — simulates a pre-migration
    // conversation that the service has never touched.
    let legacy_row = ConversationRow {
        id: "legacy-1".into(),
        user_id: "user-1".into(),
        name: "legacy".into(),
        r#type: "acp".into(),
        extra: serde_json::to_string(&json!({
            "workspace": "/tmp/x",
            "enabled_skills": ["pdf"],
            "exclude_builtin_skills": ["todo-tracker"],
            "loaded_skills": [{"name": "cron", "description": "stale"}],
        }))
        .unwrap(),
        model: None,
        status: Some("finished".into()),
        source: Some("aionui".into()),
        channel_chat_id: None,
        pinned: false,
        pinned_at: None,
        created_at: 0,
        updated_at: 0,
    };
    repo.create(&legacy_row).await.unwrap();

    let resp = svc.get("user-1", "legacy-1").await.unwrap();
    assert_eq!(resp.extra["skills"], json!(["cron", "pdf"]));
    assert!(resp.extra.get("enabled_skills").is_none());
    assert!(resp.extra.get("exclude_builtin_skills").is_none());
    assert!(resp.extra.get("loaded_skills").is_none());

    // Second read returns the same result.
    let resp2 = svc.get("user-1", "legacy-1").await.unwrap();
    assert_eq!(resp2.extra["skills"], json!(["cron", "pdf"]));

    // Verify the row on disk was persisted with the new shape.
    let persisted = repo.get("legacy-1").await.unwrap().unwrap();
    let persisted_extra: serde_json::Value = serde_json::from_str(&persisted.extra).unwrap();
    assert_eq!(persisted_extra["skills"], json!(["cron", "pdf"]));
    assert!(persisted_extra.get("enabled_skills").is_none());
    assert!(persisted_extra.get("exclude_builtin_skills").is_none());
    assert!(persisted_extra.get("loaded_skills").is_none());
}

#[tokio::test]
async fn list_backfills_mixed_rows() {
    let resolver = Arc::new(FixedSkillResolver {
        names: vec!["cron".into()],
    });
    let (svc, _broadcaster, repo, _task_mgr) = make_service_with_resolver(resolver);

    // Row 1: legacy (needs backfill).
    let legacy = ConversationRow {
        id: "a".into(),
        user_id: "u".into(),
        name: "a".into(),
        r#type: "acp".into(),
        extra: serde_json::to_string(&json!({
            "workspace": "/tmp/a",
            "enabled_skills": ["pdf"],
        }))
        .unwrap(),
        model: None,
        status: None,
        source: None,
        channel_chat_id: None,
        pinned: false,
        pinned_at: None,
        created_at: 1,
        updated_at: 1,
    };
    // Row 2: already migrated.
    let modern = ConversationRow {
        id: "b".into(),
        user_id: "u".into(),
        name: "b".into(),
        r#type: "acp".into(),
        extra: serde_json::to_string(&json!({
            "workspace": "/tmp/b",
            "skills": ["cron", "pdf"],
        }))
        .unwrap(),
        model: None,
        status: None,
        source: None,
        channel_chat_id: None,
        pinned: false,
        pinned_at: None,
        created_at: 2,
        updated_at: 2,
    };
    repo.create(&legacy).await.unwrap();
    repo.create(&modern).await.unwrap();

    let resp = svc.list("u", ListConversationsQuery::default()).await.unwrap();
    let extras: Vec<_> = resp.items.iter().map(|c| c.extra.clone()).collect();
    assert!(extras.iter().any(|e| e["skills"] == json!(["cron", "pdf"])));
}

#[tokio::test]
async fn create_honors_legacy_alias_fields_from_clone_merge() {
    let resolver = Arc::new(FixedSkillResolver {
        names: vec!["cron".into()],
    });
    let (svc, _broadcaster, _repo, _task_mgr) = make_service_with_resolver(resolver);

    // Legacy-shaped extra — what clone_create might merge in from an
    // unmigrated source conversation.
    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "acp",
        "extra": {
            "workspace": "/project",
            "backend": "claude",
            "enabled_skills": ["pdf"],
            "exclude_builtin_skills": ["cron"],
            "loaded_skills": [{"name": "cron", "description": "stale"}],
        },
    }))
    .unwrap();
    let resp = svc.create("u", req).await.unwrap();

    // Legacy enabled_skills ["pdf"] surfaces as preset; legacy exclude drops
    // cron; snapshot = {} ∪ ["pdf"] = ["pdf"].
    assert_eq!(resp.extra["skills"], json!(["pdf"]));
    assert!(resp.extra.get("enabled_skills").is_none());
    assert!(resp.extra.get("exclude_builtin_skills").is_none());
    assert!(resp.extra.get("loaded_skills").is_none());
}

// ── insert_raw_message ────────────────────────────────────────────
// Exercised by the team wake path (mirroring non-user mailbox rows into
// the target agent's conversation so the UI shows who spoke). Covers both
// the DB write and the live `message.stream` broadcast.

#[tokio::test]
async fn insert_raw_message_persists_row_and_broadcasts_stream() {
    let (svc, broadcaster, repo, _task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    // Clear the create event so our assertion sees only the insert broadcast.
    let _ = broadcaster.take_events();

    let row = MessageRow {
        id: "msg-mirror-1".into(),
        conversation_id: conv.id.clone(),
        msg_id: Some("msg-mirror-1".into()),
        r#type: "text".into(),
        content: serde_json::json!({
            "content": "from teammate",
            "teammate_message": true,
            "sender_name": "Lead",
        })
        .to_string(),
        position: Some("left".into()),
        status: Some("finish".into()),
        hidden: false,
        created_at: 1234,
    };

    svc.insert_raw_message(&row).await.unwrap();

    let stored = repo.messages.lock().unwrap().clone();
    assert_eq!(stored.len(), 1, "row must be persisted via repo.insert_message");
    assert_eq!(stored[0].id, "msg-mirror-1");
    assert_eq!(stored[0].position.as_deref(), Some("left"));

    let events = broadcaster.take_events();
    let stream_events: Vec<_> = events.iter().filter(|e| e.name == "message.stream").collect();
    assert_eq!(stream_events.len(), 1, "expected exactly one message.stream event");
    let data = &stream_events[0].data;
    assert_eq!(data["conversation_id"], conv.id);
    assert_eq!(data["msg_id"], "msg-mirror-1");
    assert_eq!(data["type"], "text");
    assert_eq!(data["position"], "left");
    assert_eq!(data["data"]["content"], "from teammate");
    assert_eq!(data["data"]["teammate_message"], true);
}

// ── IConversationService trait impl (Phase 2 Task 7) ───────────────

#[test]
fn conversation_service_implements_iconversation_service() {
    fn _assert<T: crate::conv_service_trait::IConversationService>() {}
    _assert::<ConversationService>();
}

#[tokio::test]
async fn cancel_idempotent_when_no_turn_in_flight() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    // No turn running: cancel must return Ok without hanging.
    tokio::time::timeout(
        std::time::Duration::from_millis(200),
        crate::conv_service_trait::IConversationService::cancel(&svc, "user_1", &conv.id),
    )
    .await
    .expect("cancel must not hang when no turn is in flight")
    .unwrap();
}

#[tokio::test]
async fn cancel_rejects_unknown_conversation() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let err = crate::conv_service_trait::IConversationService::cancel(&svc, "user_1", "no-such-id")
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn cancel_rejects_wrong_user() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();
    let err = crate::conv_service_trait::IConversationService::cancel(&svc, "user_2", &conv.id)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn status_reports_running_when_actor_running() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let actor = svc.get_or_create_actor(&conv.id);
    actor.mark_idle().await;
    let _h = actor.begin_turn("msg-rt".into()).await.unwrap();

    let status = crate::conv_service_trait::IConversationService::status(&svc, &conv.id);
    match status {
        crate::conv_service_trait::ConversationStatus::Running { msg_id } => assert_eq!(msg_id, "msg-rt"),
        other => panic!("expected Running, got {other:?}"),
    }
}

// ── Response.status derived from runtime ConvActor (Phase 2 Task 6) ─

#[tokio::test]
async fn response_status_reflects_runtime_not_db() {
    let (svc, _broadcaster, repo, _task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    // Spin up an actor in Idle so the response is no longer reading from
    // row.status; then corrupt DB.status to "running" and verify the
    // actor's view (Idle → Finished) wins over the corrupted column.
    let actor = svc.get_or_create_actor(&conv.id);
    actor.mark_idle().await;

    let corrupt = ConversationRowUpdate {
        status: Some("running".into()),
        ..Default::default()
    };
    repo.update(&conv.id, &corrupt).await.unwrap();

    let resp = svc.get("user_1", &conv.id).await.unwrap();
    assert_eq!(
        resp.status,
        ConversationStatus::Finished,
        "Phase 2: when an actor exists, its state wins over DB.status"
    );

    // Now begin a turn on the actor and assert the response flips to Running.
    let _held = actor.begin_turn("msg-running".into()).await.unwrap();

    let resp = svc.get("user_1", &conv.id).await.unwrap();
    assert_eq!(resp.status, ConversationStatus::Running);
}

// ── DB.status no longer written from runtime (Phase 2 Task 5) ───────

#[tokio::test]
async fn send_message_does_not_write_db_status_running() {
    // Phase 2: send_message must not write DB.status = "running". The
    // ConvActor mutex is the runtime source of truth; the column stays at
    // whatever create() set it to (currently "pending").
    let (svc, _broadcaster, repo, _task_mgr) = make_service();
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    svc.send_message("user_1", &conv.id, make_send_req(), &task_mgr)
        .await
        .unwrap();

    let row = repo.get(&conv.id).await.unwrap().unwrap();
    assert_ne!(
        row.status.as_deref(),
        Some("running"),
        "send_message must not write DB.status='running' in Phase 2"
    );
}

#[tokio::test]
async fn turn_completion_does_not_write_db_status_finished() {
    // Phase 2: complete_conversation no longer writes DB.status="finished".
    // We invoke it directly with a known-pending row and assert the column
    // is unchanged. We exercise the function via its public path so a
    // future refactor that re-introduces the write is caught here.
    let (svc, _broadcaster, repo, _task_mgr) = make_service();
    let conv = svc.create("user_1", make_create_req()).await.unwrap();

    let before = repo.get(&conv.id).await.unwrap().unwrap();
    let initial_status = before.status.clone();

    let broadcaster_dyn: Arc<dyn EventBroadcaster> = _broadcaster.clone();
    crate::stream_relay::StreamRelay::complete_conversation(svc.conversation_repo(), &broadcaster_dyn, &conv.id).await;

    let after = repo.get(&conv.id).await.unwrap().unwrap();
    assert_eq!(
        after.status, initial_status,
        "complete_conversation must not write DB.status='finished' in Phase 2"
    );
}

// ── ConvActor map (Phase 2 Task 3) ──────────────────────────────────

#[tokio::test]
async fn service_actor_is_idle_for_unknown_conversation() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    assert_eq!(
        svc.actor_status("nonexistent"),
        crate::conv_service_trait::ConversationStatus::Idle
    );
}

#[tokio::test]
async fn service_actor_is_created_on_first_access() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let actor = svc.get_or_create_actor("conv-1");
    assert_eq!(
        actor.public_status(),
        crate::conv_service_trait::ConversationStatus::Idle
    );
    let again = svc.get_or_create_actor("conv-1");
    assert!(Arc::ptr_eq(&actor, &again));
}

#[tokio::test]
async fn service_actor_is_dropped_on_delete() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    // Create a conversation through the public API so delete() finds the row.
    let resp = svc.create("user_1", make_create_req()).await.unwrap();
    let actor1 = svc.get_or_create_actor(&resp.id);

    svc.delete("user_1", &resp.id).await.unwrap();

    // After delete, asking for the actor again must produce a fresh one.
    let actor2 = svc.get_or_create_actor(&resp.id);
    assert!(
        !Arc::ptr_eq(&actor1, &actor2),
        "delete() must drop the actor entry so a subsequent lookup returns a fresh actor"
    );
}

// ── collect_idle (Phase 5 Task 2) ───────────────────────────────────

#[tokio::test]
async fn collect_idle_returns_only_actors_idle_past_threshold() {
    use aionui_common::now_ms;

    let (svc, _broadcaster, _repo, _task_mgr) = make_service();

    // stale: idle and last activity backdated past the threshold.
    let stale = svc.get_or_create_actor("stale");
    stale.mark_idle().await;
    stale.set_last_activity_ms_for_test(now_ms() - 10 * 60 * 1000);

    // fresh: idle but with a recent timestamp.
    let fresh = svc.get_or_create_actor("fresh");
    fresh.mark_idle().await;

    // busy: a turn is in flight, must never be returned regardless of age.
    let busy = svc.get_or_create_actor("busy");
    busy.mark_idle().await;
    let _h = busy.begin_turn("m".into()).await.unwrap();
    busy.set_last_activity_ms_for_test(now_ms() - 10 * 60 * 1000);

    let mut idle =
        <ConversationService as crate::conv_service_trait::IConversationService>::collect_idle(&svc, 5 * 60 * 1000);
    idle.sort();
    assert_eq!(idle, vec!["stale".to_owned()]);
}

#[tokio::test]
async fn collect_idle_empty_when_no_actors() {
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    let idle =
        <ConversationService as crate::conv_service_trait::IConversationService>::collect_idle(&svc, 5 * 60 * 1000);
    assert!(idle.is_empty());
}

#[tokio::test]
async fn cancel_idle_resolves_owner_and_is_noop_for_missing_row() {
    // Missing row -> Ok (idempotent).
    let (svc, _broadcaster, _repo, _task_mgr) = make_service();
    <ConversationService as crate::conv_service_trait::IConversationService>::cancel_idle(&svc, "missing")
        .await
        .unwrap();
}
