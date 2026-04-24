//! Black-box integration tests for `CronService`.
//!
//! Uses real SQLite (in-memory), mock broadcaster, and stubs for
//! task manager / conversation service (since integration with AI agents
//! is out of scope for this service-layer test).
//!
//! Covers test-plan items: CJ-1..CJ-12, SK-1..SK-7, SC-1..SC-8,
//! OC-1, SR-1, ICronService trait integration.

use std::sync::{Arc, Mutex};

use aionui_ai_agent::agent_manager::AgentManagerHandle;
use aionui_ai_agent::middleware::CronCreateParams;
use aionui_ai_agent::types::BuildTaskOptions;
use aionui_api_types::{
    CreateCronJobRequest, CronScheduleDto, ListCronJobsQuery, SaveCronSkillRequest,
    UpdateCronJobRequest, WebSocketMessage,
};
use aionui_common::{PaginatedResult, TimestampMs, now_ms};
use aionui_conversation::ConversationService;
use aionui_db::{
    ConversationFilters, ConversationRowUpdate, IConversationRepository, ICronRepository,
    MessageRowUpdate, MessageSearchRow, SortOrder, SqliteCronRepository, init_database_memory,
};
use aionui_realtime::EventBroadcaster;

use aionui_cron::busy_guard::CronBusyGuard;
use aionui_cron::events::CronEventEmitter;
use aionui_cron::executor::JobExecutor;
use aionui_cron::scheduler::CronScheduler;
use aionui_cron::service::CronService;

// ── Test infrastructure ────────────────────────────────────────────

struct MockBroadcaster {
    events: Mutex<Vec<WebSocketMessage<serde_json::Value>>>,
}

impl MockBroadcaster {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    fn take_events(&self) -> Vec<WebSocketMessage<serde_json::Value>> {
        let mut guard = self.events.lock().unwrap();
        std::mem::take(&mut *guard)
    }
}

impl EventBroadcaster for MockBroadcaster {
    fn broadcast(&self, event: WebSocketMessage<serde_json::Value>) {
        self.events.lock().unwrap().push(event);
    }
}

struct StubTaskManager;

impl aionui_ai_agent::task_manager::IWorkerTaskManager for StubTaskManager {
    fn get_task(&self, _: &str) -> Option<AgentManagerHandle> {
        None
    }
    fn get_or_build_task(
        &self,
        _: &str,
        _: BuildTaskOptions,
    ) -> Result<AgentManagerHandle, aionui_common::AppError> {
        Err(aionui_common::AppError::Internal("stub".into()))
    }
    fn kill(
        &self,
        _: &str,
        _: Option<aionui_common::AgentKillReason>,
    ) -> Result<(), aionui_common::AppError> {
        Ok(())
    }
    fn clear(&self) {}
    fn active_count(&self) -> usize {
        0
    }
    fn collect_idle(&self, _: TimestampMs) -> Vec<String> {
        vec![]
    }
}

struct StubConvRepo;

#[async_trait::async_trait]
impl IConversationRepository for StubConvRepo {
    async fn get(
        &self,
        _id: &str,
    ) -> Result<Option<aionui_db::models::ConversationRow>, aionui_db::DbError> {
        Ok(Some(aionui_db::models::ConversationRow {
            id: "conv_stub".into(),
            user_id: "u1".into(),
            name: "stub".into(),
            r#type: "default".into(),
            model: None,
            status: Some("active".into()),
            source: None,
            channel_chat_id: None,
            extra: "{}".into(),
            pinned: false,
            pinned_at: None,
            created_at: 1000,
            updated_at: 1000,
        }))
    }
    async fn create(
        &self,
        _row: &aionui_db::models::ConversationRow,
    ) -> Result<(), aionui_db::DbError> {
        Ok(())
    }
    async fn update(
        &self,
        _id: &str,
        _updates: &ConversationRowUpdate,
    ) -> Result<(), aionui_db::DbError> {
        Ok(())
    }
    async fn delete(&self, _id: &str) -> Result<(), aionui_db::DbError> {
        Ok(())
    }
    async fn list_paginated(
        &self,
        _user_id: &str,
        _filters: &ConversationFilters,
    ) -> Result<PaginatedResult<aionui_db::models::ConversationRow>, aionui_db::DbError> {
        Ok(PaginatedResult {
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
    ) -> Result<Option<aionui_db::models::ConversationRow>, aionui_db::DbError> {
        Ok(None)
    }
    async fn list_by_cron_job(
        &self,
        _user_id: &str,
        _cron_job_id: &str,
    ) -> Result<Vec<aionui_db::models::ConversationRow>, aionui_db::DbError> {
        Ok(vec![])
    }
    async fn list_associated(
        &self,
        _user_id: &str,
        _conversation_id: &str,
    ) -> Result<Vec<aionui_db::models::ConversationRow>, aionui_db::DbError> {
        Ok(vec![])
    }
    async fn get_messages(
        &self,
        _conv_id: &str,
        _page: u32,
        _page_size: u32,
        _order: SortOrder,
    ) -> Result<PaginatedResult<aionui_db::models::MessageRow>, aionui_db::DbError> {
        Ok(PaginatedResult {
            items: vec![],
            total: 0,
            has_more: false,
        })
    }
    async fn insert_message(
        &self,
        _message: &aionui_db::models::MessageRow,
    ) -> Result<(), aionui_db::DbError> {
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
    ) -> Result<Option<aionui_db::models::MessageRow>, aionui_db::DbError> {
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

async fn setup() -> (CronService, Arc<dyn ICronRepository>, Arc<MockBroadcaster>) {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool().clone();
    let cron_repo: Arc<dyn ICronRepository> = Arc::new(SqliteCronRepository::new(pool));
    let bc = Arc::new(MockBroadcaster::new());

    let stub_conv_repo: Arc<dyn IConversationRepository> = Arc::new(StubConvRepo);
    let conv_service = Arc::new(ConversationService::new(
        Arc::clone(&stub_conv_repo),
        bc.clone() as Arc<dyn EventBroadcaster>,
    ));
    let busy_guard = Arc::new(CronBusyGuard::new());
    let executor = Arc::new(JobExecutor::new(
        Arc::new(StubTaskManager),
        stub_conv_repo,
        conv_service,
        busy_guard,
    ));

    let scheduler = Arc::new(CronScheduler::new(Arc::new(|_| {})));

    let emitter = CronEventEmitter::new(bc.clone() as Arc<dyn EventBroadcaster>);
    let svc = CronService::new(cron_repo.clone(), scheduler, executor, emitter);

    std::mem::forget(db);
    (svc, cron_repo, bc)
}

fn make_create_req(name: &str, schedule: CronScheduleDto) -> CreateCronJobRequest {
    CreateCronJobRequest {
        name: name.into(),
        schedule,
        prompt: None,
        message: Some("test message".into()),
        conversation_id: "conv_1".into(),
        conversation_title: Some("Test Conv".into()),
        agent_type: "acp".into(),
        created_by: "user".into(),
        execution_mode: None,
        agent_config: None,
    }
}

fn every_60s() -> CronScheduleDto {
    CronScheduleDto::Every {
        every_ms: 60000,
        description: Some("every minute".into()),
    }
}

fn at_future(offset_ms: i64) -> CronScheduleDto {
    CronScheduleDto::At {
        at_ms: now_ms() + offset_ms,
        description: Some("once".into()),
    }
}

fn cron_every_5min() -> CronScheduleDto {
    CronScheduleDto::Cron {
        expr: "0 */5 * * * *".into(),
        tz: None,
        description: Some("every 5 min".into()),
    }
}

// ── CJ-1: Create cron job ──────────────────────────────────────────

#[tokio::test]
async fn cj1_create_cron_job() {
    let (svc, _, bc) = setup().await;
    let req = make_create_req("Daily Report", every_60s());

    let job = svc.add_job(req).await.unwrap();

    assert!(job.id.starts_with("cron_"));
    assert_eq!(job.name, "Daily Report");
    assert!(job.enabled);
    assert!(job.next_run_at.is_some());
    assert_eq!(job.run_count, 0);

    let events = bc.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "cron.jobCreated");
}

// ── CJ-2: Create three schedule types ──────────────────────────────

#[tokio::test]
async fn cj2_create_three_schedule_types() {
    let (svc, _, _) = setup().await;
    let now = now_ms();

    let at_job = svc
        .add_job(make_create_req("At Job", at_future(3600000)))
        .await
        .unwrap();
    assert!(at_job.next_run_at.unwrap() > now);

    let every_job = svc
        .add_job(make_create_req("Every Job", every_60s()))
        .await
        .unwrap();
    let next = every_job.next_run_at.unwrap();
    assert!((next - now - 60000).abs() < 2000);

    let cron_job = svc
        .add_job(make_create_req("Cron Job", cron_every_5min()))
        .await
        .unwrap();
    assert!(cron_job.next_run_at.unwrap() > now);
}

// ── CJ-4: Get single job ──────────────────────────────────────────

#[tokio::test]
async fn cj4_get_single_job() {
    let (svc, _, _) = setup().await;
    let created = svc
        .add_job(make_create_req("Get Test", every_60s()))
        .await
        .unwrap();

    let fetched = svc.get_job(&created.id).await.unwrap();
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.name, "Get Test");
}

// ── CJ-5: Get nonexistent job ─────────────────────────────────────

#[tokio::test]
async fn cj5_get_nonexistent_job() {
    let (svc, _, _) = setup().await;
    let err = svc.get_job("cron_nonexistent").await.unwrap_err();
    assert!(matches!(err, aionui_cron::error::CronError::JobNotFound(_)));
}

// ── CJ-6: List all jobs ───────────────────────────────────────────

#[tokio::test]
async fn cj6_list_all_jobs() {
    let (svc, _, _) = setup().await;
    for i in 0..3 {
        svc.add_job(make_create_req(&format!("Job {i}"), every_60s()))
            .await
            .unwrap();
    }

    let jobs = svc.list_jobs(&ListCronJobsQuery::default()).await.unwrap();
    assert!(jobs.len() >= 3);
}

// ── CJ-7: List by conversation ────────────────────────────────────

#[tokio::test]
async fn cj7_list_by_conversation() {
    let (svc, _, _) = setup().await;

    let mut req1 = make_create_req("Job A", every_60s());
    req1.conversation_id = "conv_target".into();
    svc.add_job(req1).await.unwrap();

    let mut req2 = make_create_req("Job B", every_60s());
    req2.conversation_id = "conv_target".into();
    svc.add_job(req2).await.unwrap();

    let mut req3 = make_create_req("Job C", every_60s());
    req3.conversation_id = "conv_other".into();
    svc.add_job(req3).await.unwrap();

    let query = ListCronJobsQuery {
        conversation_id: Some("conv_target".into()),
    };
    let jobs = svc.list_jobs(&query).await.unwrap();
    assert_eq!(jobs.len(), 2);
}

// ── CJ-8: Update job ──────────────────────────────────────────────

#[tokio::test]
async fn cj8_update_job() {
    let (svc, _, bc) = setup().await;
    let created = svc
        .add_job(make_create_req("Original", every_60s()))
        .await
        .unwrap();
    bc.take_events();

    let req = UpdateCronJobRequest {
        name: Some("Updated Name".into()),
        enabled: Some(false),
        schedule: None,
        message: None,
        execution_mode: None,
        agent_config: None,
        conversation_title: None,
        max_retries: None,
    };

    let updated = svc.update_job(&created.id, req).await.unwrap();
    assert_eq!(updated.name, "Updated Name");
    assert!(!updated.enabled);
    assert!(updated.updated_at >= created.created_at);

    let events = bc.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "cron.jobUpdated");
}

// ── CJ-9: Update schedule type ────────────────────────────────────

#[tokio::test]
async fn cj9_update_schedule_type() {
    let (svc, _, _) = setup().await;
    let created = svc
        .add_job(make_create_req("Schedule Change", every_60s()))
        .await
        .unwrap();

    let req = UpdateCronJobRequest {
        name: None,
        enabled: None,
        schedule: Some(cron_every_5min()),
        message: None,
        execution_mode: None,
        agent_config: None,
        conversation_title: None,
        max_retries: None,
    };

    let updated = svc.update_job(&created.id, req).await.unwrap();
    assert!(matches!(
        updated.schedule,
        aionui_cron::types::CronSchedule::Cron { .. }
    ));
    assert!(updated.next_run_at.is_some());
}

// ── CJ-10: Update nonexistent job ─────────────────────────────────

#[tokio::test]
async fn cj10_update_nonexistent() {
    let (svc, _, _) = setup().await;
    let req = UpdateCronJobRequest {
        name: Some("x".into()),
        enabled: None,
        schedule: None,
        message: None,
        execution_mode: None,
        agent_config: None,
        conversation_title: None,
        max_retries: None,
    };
    let err = svc.update_job("cron_nonexistent", req).await.unwrap_err();
    assert!(matches!(err, aionui_cron::error::CronError::JobNotFound(_)));
}

// ── CJ-11: Delete job ─────────────────────────────────────────────

#[tokio::test]
async fn cj11_delete_job() {
    let (svc, _, bc) = setup().await;
    let created = svc
        .add_job(make_create_req("To Delete", every_60s()))
        .await
        .unwrap();
    bc.take_events();

    svc.remove_job(&created.id).await.unwrap();

    let err = svc.get_job(&created.id).await.unwrap_err();
    assert!(matches!(err, aionui_cron::error::CronError::JobNotFound(_)));

    let events = bc.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "cron.jobRemoved");
}

// ── CJ-12: Delete nonexistent ─────────────────────────────────────

#[tokio::test]
async fn cj12_delete_nonexistent() {
    let (svc, _, _) = setup().await;
    let err = svc.remove_job("cron_nonexistent").await.unwrap_err();
    assert!(matches!(
        err,
        aionui_cron::error::CronError::Database(aionui_db::DbError::NotFound(_))
    ));
}

// ── SK-1: Save skill ──────────────────────────────────────────────

#[tokio::test]
async fn sk1_save_skill() {
    let (svc, _, _) = setup().await;
    let job = svc
        .add_job(make_create_req("Skill Job", every_60s()))
        .await
        .unwrap();

    let req = SaveCronSkillRequest {
        content: "---\nname: test\ndescription: test skill\n---\nDo something".into(),
    };
    svc.save_skill(&job.id, req).await.unwrap();
}

// ── SK-2: Has skill (true) ────────────────────────────────────────

#[tokio::test]
async fn sk2_has_skill_true() {
    let (svc, _, _) = setup().await;
    let job = svc
        .add_job(make_create_req("Skill Check", every_60s()))
        .await
        .unwrap();

    svc.save_skill(
        &job.id,
        SaveCronSkillRequest {
            content: "---\nname: x\n---\nContent".into(),
        },
    )
    .await
    .unwrap();

    let resp = svc.has_skill(&job.id).await.unwrap();
    assert!(resp.has_skill);
}

// ── SK-3: Has skill (false) ───────────────────────────────────────

#[tokio::test]
async fn sk3_has_skill_false() {
    let (svc, _, _) = setup().await;
    let job = svc
        .add_job(make_create_req("No Skill", every_60s()))
        .await
        .unwrap();

    let resp = svc.has_skill(&job.id).await.unwrap();
    assert!(!resp.has_skill);
}

// ── SK-4: Save empty skill ────────────────────────────────────────

#[tokio::test]
async fn sk4_save_empty_skill() {
    let (svc, _, _) = setup().await;
    let job = svc
        .add_job(make_create_req("Empty Skill", every_60s()))
        .await
        .unwrap();

    let err = svc
        .save_skill(&job.id, SaveCronSkillRequest { content: "".into() })
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        aionui_cron::error::CronError::InvalidSkillContent(_)
    ));
}

// ── SK-5: Save placeholder skill ──────────────────────────────────

#[tokio::test]
async fn sk5_save_placeholder_skill() {
    let (svc, _, _) = setup().await;
    let job = svc
        .add_job(make_create_req("Placeholder Skill", every_60s()))
        .await
        .unwrap();

    let err = svc
        .save_skill(
            &job.id,
            SaveCronSkillRequest {
                content: "TODO: fill in later".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        aionui_cron::error::CronError::InvalidSkillContent(_)
    ));
}

// ── SK-6: Save skill for nonexistent job ──────────────────────────

#[tokio::test]
async fn sk6_save_skill_nonexistent() {
    let (svc, _, _) = setup().await;
    let err = svc
        .save_skill(
            "cron_nonexistent",
            SaveCronSkillRequest {
                content: "---\nname: x\n---\nOk".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, aionui_cron::error::CronError::JobNotFound(_)));
}

// ── SK-7: Delete skill on job removal ─────────────────────────────

#[tokio::test]
async fn sk7_delete_cleans_skill() {
    let (svc, _, _) = setup().await;
    let job = svc
        .add_job(make_create_req("Skill Cleanup", every_60s()))
        .await
        .unwrap();
    svc.save_skill(
        &job.id,
        SaveCronSkillRequest {
            content: "---\nname: x\n---\nContent".into(),
        },
    )
    .await
    .unwrap();

    svc.remove_job(&job.id).await.unwrap();

    let err = svc.has_skill(&job.id).await.unwrap_err();
    assert!(matches!(err, aionui_cron::error::CronError::JobNotFound(_)));
}

// ── SC-3: Every type next_run ─────────────────────────────────────

#[tokio::test]
async fn sc3_every_type_next_run() {
    let (svc, _, _) = setup().await;
    let now = now_ms();
    let job = svc
        .add_job(make_create_req("Every 60s", every_60s()))
        .await
        .unwrap();

    let next = job.next_run_at.unwrap();
    let diff = (next - now - 60000).abs();
    assert!(diff < 2000, "expected next_run ≈ now+60000, diff={diff}");
}

// ── SC-5: Invalid cron expression ─────────────────────────────────

#[tokio::test]
async fn sc5_invalid_cron_expression() {
    let (svc, _, _) = setup().await;
    let req = make_create_req(
        "Invalid Cron",
        CronScheduleDto::Cron {
            expr: "invalid cron".into(),
            tz: None,
            description: None,
        },
    );
    let err = svc.add_job(req).await.unwrap_err();
    assert!(matches!(
        err,
        aionui_cron::error::CronError::InvalidCronExpression(_)
    ));
}

// ── SC-6: Cron with timezone ──────────────────────────────────────

#[tokio::test]
async fn sc6_cron_with_timezone() {
    let (svc, _, _) = setup().await;
    let now = now_ms();
    let req = make_create_req(
        "Shanghai Job",
        CronScheduleDto::Cron {
            expr: "0 0 9 * * *".into(),
            tz: Some("Asia/Shanghai".into()),
            description: None,
        },
    );
    let job = svc.add_job(req).await.unwrap();
    assert!(job.next_run_at.unwrap() > now);
}

// ── SC-7: Every zero interval ─────────────────────────────────────

#[tokio::test]
async fn sc7_every_zero_interval() {
    let (svc, _, _) = setup().await;
    let req = make_create_req(
        "Zero Interval",
        CronScheduleDto::Every {
            every_ms: 0,
            description: None,
        },
    );
    let err = svc.add_job(req).await.unwrap_err();
    assert!(matches!(
        err,
        aionui_cron::error::CronError::InvalidSchedule(_)
    ));
}

// ── SC-8: Every negative interval ─────────────────────────────────

#[tokio::test]
async fn sc8_every_negative_interval() {
    let (svc, _, _) = setup().await;
    let req = make_create_req(
        "Negative Interval",
        CronScheduleDto::Every {
            every_ms: -1000,
            description: None,
        },
    );
    let err = svc.add_job(req).await.unwrap_err();
    assert!(matches!(
        err,
        aionui_cron::error::CronError::InvalidSchedule(_)
    ));
}

// ── OC-1: Init cleans orphan jobs ─────────────────────────────────

#[tokio::test]
async fn oc1_init_cleans_orphans() {
    let (svc, _repo, _) = setup().await;

    let mut req = make_create_req("Orphan", every_60s());
    req.conversation_id = "".into();
    let orphan = svc.add_job(req).await.unwrap();

    let normal_req = make_create_req("Normal", every_60s());
    let normal = svc.add_job(normal_req).await.unwrap();

    svc.init().await;

    let err = svc.get_job(&orphan.id).await;
    assert!(err.is_err());

    let found = svc.get_job(&normal.id).await;
    assert!(found.is_ok());
}

// ── Delete skill explicitly ───────────────────────────────────────

#[tokio::test]
async fn delete_skill_clears_content() {
    let (svc, _, _) = setup().await;
    let job = svc
        .add_job(make_create_req("Del Skill", every_60s()))
        .await
        .unwrap();

    svc.save_skill(
        &job.id,
        SaveCronSkillRequest {
            content: "---\nname: x\n---\nOk".into(),
        },
    )
    .await
    .unwrap();
    assert!(svc.has_skill(&job.id).await.unwrap().has_skill);

    svc.delete_skill(&job.id).await.unwrap();
    assert!(!svc.has_skill(&job.id).await.unwrap().has_skill);
}

// ── ICronService trait: create ─────────────────────────────────────

#[tokio::test]
async fn icron_service_create_job() {
    let (svc, _, _) = setup().await;

    use aionui_ai_agent::middleware::ICronService;

    let params = CronCreateParams {
        name: "Agent Job".into(),
        schedule: "0 */10 * * * *".into(),
        schedule_description: "every 10 min".into(),
        message: "do agent work".into(),
    };

    let result = ICronService::create_job(&svc, "user_1", "conv_1", &params).await;
    assert!(result.success);
    assert!(result.message.contains("Agent Job"));
}

// ── ICronService trait: list ───────────────────────────────────────

#[tokio::test]
async fn icron_service_list_jobs() {
    let (svc, _, _) = setup().await;

    use aionui_ai_agent::middleware::ICronService;

    let result = ICronService::list_jobs(&svc, "user_1").await;
    assert!(result.success);
    assert!(result.message.contains("No cron jobs found"));

    svc.add_job(make_create_req("Listed Job", every_60s()))
        .await
        .unwrap();

    let result = ICronService::list_jobs(&svc, "user_1").await;
    assert!(result.success);
    assert!(result.message.contains("1 cron job(s)"));
    assert!(result.message.contains("Listed Job"));
}

// ── ICronService trait: delete ─────────────────────────────────────

#[tokio::test]
async fn icron_service_delete_job() {
    let (svc, _, _) = setup().await;

    use aionui_ai_agent::middleware::ICronService;

    let job = svc
        .add_job(make_create_req("Delete Via Trait", every_60s()))
        .await
        .unwrap();

    let result = ICronService::delete_job(&svc, "user_1", &job.id).await;
    assert!(result.success);

    let result = ICronService::delete_job(&svc, "user_1", "cron_nonexistent").await;
    assert!(!result.success);
}

// ── Update with max_retries ───────────────────────────────────────

#[tokio::test]
async fn update_max_retries() {
    let (svc, _, _) = setup().await;
    let job = svc
        .add_job(make_create_req("Retries", every_60s()))
        .await
        .unwrap();
    assert_eq!(job.max_retries, 3);

    let req = UpdateCronJobRequest {
        name: None,
        enabled: None,
        schedule: None,
        message: None,
        execution_mode: None,
        agent_config: None,
        conversation_title: None,
        max_retries: Some(5),
    };
    let updated = svc.update_job(&job.id, req).await.unwrap();
    assert_eq!(updated.max_retries, 5);
}

// ── SC-1: At type — future timestamp, nextRunAtMs == atMs ────────

#[tokio::test]
async fn sc1_at_type_future_timestamp() {
    let (svc, _, _) = setup().await;
    let target_ms = now_ms() + 3_600_000;
    let req = make_create_req(
        "At Future",
        CronScheduleDto::At {
            at_ms: target_ms,
            description: Some("once in 1h".into()),
        },
    );
    let job = svc.add_job(req).await.unwrap();
    assert_eq!(job.next_run_at, Some(target_ms));
}

// ── SC-2: At type — past timestamp, nextRunAtMs == atMs ──────────

#[tokio::test]
async fn sc2_at_type_past_timestamp() {
    let (svc, _, _) = setup().await;
    let target_ms = now_ms() - 3_600_000;
    let req = make_create_req(
        "At Past",
        CronScheduleDto::At {
            at_ms: target_ms,
            description: Some("once in the past".into()),
        },
    );
    let job = svc.add_job(req).await.unwrap();
    assert_eq!(job.next_run_at, Some(target_ms));
}

// ── SR-1: System resume detects missed jobs ──────────────────────

#[tokio::test]
async fn sr1_system_resume_missed_job() {
    let (svc, repo, bc) = setup().await;

    let req = make_create_req("Resume Job", every_60s());
    let job = svc.add_job(req).await.unwrap();
    bc.take_events();

    let past_ms = now_ms() - 10_000;
    let params = aionui_db::UpdateCronJobParams {
        next_run_at: Some(Some(past_ms)),
        ..Default::default()
    };
    repo.update(&job.id, &params).await.unwrap();

    svc.handle_system_resume().await;

    let updated = svc.get_job(&job.id).await.unwrap();
    assert!(
        updated.last_run_at.is_some(),
        "missed job should have been executed (last_run_at set)"
    );
    assert!(
        updated.next_run_at.is_some(),
        "job should be rescheduled after execution"
    );
    assert!(
        updated.next_run_at.unwrap() > now_ms() - 2000,
        "next_run_at should be in the future"
    );
}

// ── CD-1: Cascade delete cron jobs when conversation is deleted ──

#[tokio::test]
async fn cd1_cascade_delete_by_conversation() {
    let (svc, _repo, bc) = setup().await;

    let mut req_a = make_create_req("Cascade A", every_60s());
    req_a.conversation_id = "conv_cascade".into();
    let job_a = svc.add_job(req_a).await.unwrap();

    let mut req_b = make_create_req("Cascade B", every_60s());
    req_b.conversation_id = "conv_cascade".into();
    let job_b = svc.add_job(req_b).await.unwrap();

    let mut req_c = make_create_req("Unrelated", every_60s());
    req_c.conversation_id = "conv_other".into();
    let _job_c = svc.add_job(req_c).await.unwrap();

    bc.take_events();

    svc.delete_jobs_by_conversation("conv_cascade").await;

    assert!(svc.get_job(&job_a.id).await.is_err());
    assert!(svc.get_job(&job_b.id).await.is_err());

    let remaining = svc.list_jobs(&ListCronJobsQuery::default()).await.unwrap();
    assert_eq!(remaining.len(), 1, "only the unrelated job should remain");

    let events = bc.take_events();
    let removed_events: Vec<_> = events
        .iter()
        .filter(|e| e.name == "cron.jobRemoved")
        .collect();
    assert_eq!(removed_events.len(), 2, "should emit 2 removed events");
}

// ── CD-2: Cascade delete on empty conversation (no-op) ──────────

#[tokio::test]
async fn cd2_cascade_delete_no_matching_jobs() {
    let (svc, _repo, bc) = setup().await;

    svc.add_job(make_create_req("Existing", every_60s()))
        .await
        .unwrap();
    bc.take_events();

    svc.delete_jobs_by_conversation("conv_nonexistent").await;

    let events = bc.take_events();
    assert!(
        events.is_empty(),
        "no events should be emitted when no jobs match"
    );

    let all = svc.list_jobs(&ListCronJobsQuery::default()).await.unwrap();
    assert_eq!(all.len(), 1, "existing job should remain untouched");
}

// ── CD-3: OnConversationDelete trait dispatches cascade ──────────

#[tokio::test]
async fn cd3_on_conversation_delete_trait() {
    use aionui_conversation::OnConversationDelete;

    let (svc, _repo, bc) = setup().await;

    let mut req = make_create_req("Trait Cascade", every_60s());
    req.conversation_id = "conv_trait_del".into();
    let job = svc.add_job(req).await.unwrap();
    bc.take_events();

    svc.on_conversation_deleted("conv_trait_del").await;

    assert!(svc.get_job(&job.id).await.is_err());

    let events = bc.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "cron.jobRemoved");
}
