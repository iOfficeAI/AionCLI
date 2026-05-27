//! Phase 2 — integration test for ELECTRON-1KB at the conv-layer trait
//! surface.
//!
//! The structural fix being verified here: after `cancel()` returns, the
//! per-conversation `ConvActor` is back to `Idle`, and the *very next*
//! `send_message()` MUST NOT observe a `Conflict` (the legacy DB.status
//! guard could not see in-memory turn state and produced the race).
//!
//! The plan suggested using a mock `IAgentConnector` that pauses inside
//! `run_turn`. In practice the conv-layer trait surface (`send` /
//! `cancel`) does not consume `IAgentConnector` directly — it goes
//! through `IWorkerTaskManager::get_or_build_task` and `ConvActor`. We
//! therefore exercise the same invariant by driving the actor directly
//! from the test (it is the actor's mutex that closes the race), then
//! invoking `IConversationService::cancel` and `::send` against the
//! real `ConversationService`. This keeps the test trait-surface only
//! and avoids depending on connector internals that are still being
//! reshaped by Phase 1/Phase 3.
//!
//! Asserted invariants:
//!   1. `cancel()` waits for the in-flight turn (does not return early).
//!   2. After `cancel()` returns, the actor is `Idle`.
//!   3. The follow-up `send()` does NOT return `AppError::Conflict`.
//!      It may fail for unrelated infrastructure reasons (the test
//!      uses a default `MockConnectorFactory` whose default build
//!      closure produces no skill-aware ACP runtime), but `Conflict`
//!      specifically means the cancel→send race re-emerged.

use std::sync::Arc;
use std::time::Duration;

use aionui_ai_agent::IAgentConnectorFactory;
use aionui_ai_agent::test_support::MockConnectorFactory;
use aionui_api_types::{CreateConversationRequest, SendMessageRequest, WebSocketMessage};
use aionui_common::AppError;
use aionui_conversation::ConversationService;
use aionui_conversation::conv_service_trait::{ConversationStatus, IConversationService};
use aionui_conversation::skill_resolver::SkillResolver;
use aionui_db::{SqliteConversationRepository, init_database_memory};
use aionui_realtime::EventBroadcaster;
use serde_json::json;
use std::sync::Mutex;

// ── Test infrastructure ────────────────────────────────────────────

struct NullBroadcaster {
    events: Mutex<Vec<WebSocketMessage<serde_json::Value>>>,
}

impl NullBroadcaster {
    fn new() -> Self {
        Self {
            events: Mutex::new(vec![]),
        }
    }
}

impl EventBroadcaster for NullBroadcaster {
    fn broadcast(&self, event: WebSocketMessage<serde_json::Value>) {
        self.events.lock().unwrap().push(event);
    }
}

struct EmptySkillResolver;

#[async_trait::async_trait]
impl SkillResolver for EmptySkillResolver {
    async fn auto_inject_names(&self) -> Vec<String> {
        Vec::new()
    }
    async fn resolve_skills(&self, _: &[String]) -> Vec<aionui_extension::ResolvedAgentSkill> {
        Vec::new()
    }
    async fn link_workspace_skills(
        &self,
        _: &std::path::Path,
        _: &[&str],
        _: &[aionui_extension::ResolvedAgentSkill],
    ) -> usize {
        0
    }
}

async fn build_service() -> Arc<ConversationService> {
    let db = init_database_memory().await.unwrap();
    let repo = Arc::new(SqliteConversationRepository::new(db.pool().clone()));
    let agent_metadata_repo: Arc<dyn aionui_db::IAgentMetadataRepository> =
        Arc::new(aionui_db::SqliteAgentMetadataRepository::new(db.pool().clone()));
    let acp_session_repo: Arc<dyn aionui_db::IAcpSessionRepository> =
        Arc::new(aionui_db::SqliteAcpSessionRepository::new(db.pool().clone()));
    let task_mgr: Arc<dyn IAgentConnectorFactory> = MockConnectorFactory::builder().build();
    Arc::new(ConversationService::new(
        std::env::temp_dir(),
        Arc::new(NullBroadcaster::new()),
        Arc::new(EmptySkillResolver),
        task_mgr,
        repo,
        agent_metadata_repo,
        acp_session_repo,
    ))
}

const USER: &str = "system_default_user";

fn create_req() -> CreateConversationRequest {
    serde_json::from_value(json!({
        "type": "acp",
        "extra": { "workspace": "/home/user/project" }
    }))
    .unwrap()
}

fn send_req(content: &str) -> SendMessageRequest {
    serde_json::from_value(json!({ "content": content })).unwrap()
}

// ── Tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn cancel_then_send_does_not_return_conflict() {
    let svc = build_service().await;
    let conv_id = svc.create(USER, create_req()).await.unwrap().id;
    let trait_svc: Arc<dyn IConversationService> = svc.clone();

    // Simulate an in-flight turn by holding a TurnHandle ourselves.
    // From the trait surface's point of view this is indistinguishable
    // from a real connector that is paused inside `run_turn` — both
    // leave the actor in `Running { msg_id }`.
    let actor = svc.get_or_create_actor(&conv_id);
    let turn_handle = actor.begin_turn("msg-fake-1".into()).await.unwrap();
    assert!(matches!(trait_svc.status(&conv_id), ConversationStatus::Running { .. }));

    // Spawn a task that drops the TurnHandle after a short delay.
    // This mimics the connector ack'ing cancel and the spawned turn
    // task exiting — the TurnHandle::Drop transitions the actor to
    // Idle and unblocks `wait_for_idle()`.
    let drop_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(turn_handle);
    });

    // Cancel must wait until the actor is idle.
    let cancel_started = std::time::Instant::now();
    trait_svc.cancel(USER, &conv_id).await.unwrap();
    let cancel_elapsed = cancel_started.elapsed();
    assert!(
        cancel_elapsed >= Duration::from_millis(15),
        "cancel returned too early ({cancel_elapsed:?}); did not wait for actor idle"
    );

    drop_task.await.unwrap();

    // After cancel, the actor MUST be Idle.
    assert_eq!(
        trait_svc.status(&conv_id),
        ConversationStatus::Idle,
        "actor still Running after cancel returned",
    );

    // The next send MUST NOT return Conflict. With the default
    // MockConnectorFactory the call may succeed (the mock connector
    // accepts send_message) — that's fine. The race-regression
    // assertion is specifically that we do not see Conflict.
    let result = trait_svc.send(USER, &conv_id, send_req("hello again")).await;
    if let Err(AppError::Conflict(msg)) = result {
        panic!("regression: send after cancel returned Conflict ({msg}); the cancel→send race is back");
    }
}

#[tokio::test]
async fn cancel_returns_immediately_when_no_turn_in_flight() {
    // Idempotency check: cancel on a never-running conversation MUST
    // return without blocking. This guards against `wait_for_idle()`
    // being mis-implemented to wait on an empty channel.
    let svc = build_service().await;
    let conv_id = svc.create(USER, create_req()).await.unwrap().id;
    let trait_svc: Arc<dyn IConversationService> = svc.clone();

    let started = std::time::Instant::now();
    trait_svc.cancel(USER, &conv_id).await.unwrap();
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(50),
        "cancel blocked for {elapsed:?} when no turn was in flight"
    );
    assert_eq!(trait_svc.status(&conv_id), ConversationStatus::Idle);
}

#[tokio::test]
async fn second_send_after_cancel_does_not_see_running_state() {
    // Variant: after a real begin_turn → drop cycle, the actor is Idle
    // and a fresh `begin_turn` succeeds. This exercises the same path
    // that production uses when `send_message` spawns its own turn.
    let svc = build_service().await;
    let conv_id = svc.create(USER, create_req()).await.unwrap().id;
    let trait_svc: Arc<dyn IConversationService> = svc.clone();

    let actor = svc.get_or_create_actor(&conv_id);

    {
        let h = actor.begin_turn("msg-A".into()).await.unwrap();
        assert!(matches!(trait_svc.status(&conv_id), ConversationStatus::Running { .. }));
        drop(h);
    }

    // After drop the actor must be Idle so the next caller is not blocked.
    assert_eq!(trait_svc.status(&conv_id), ConversationStatus::Idle);

    // A fresh begin_turn must succeed (no Conflict).
    let h2 = actor.begin_turn("msg-B".into()).await;
    assert!(h2.is_ok(), "fresh begin_turn returned: {:?}", h2.err());
}
