use std::str::FromStr;
use std::sync::Arc;

use aionui_api_types::{
    CreateCronJobRequest, CronJobExecutedEvent, CronJobRemovedPayload, CronJobResponse,
    CronScheduleDto, HasSkillResponse, ListCronJobsQuery, RunNowResponse, SaveCronSkillRequest,
    UpdateCronJobRequest, WebSocketMessage,
};
use aionui_common::{generate_prefixed_id, now_ms};
use aionui_db::{ICronRepository, UpdateCronJobParams};
use aionui_realtime::EventBroadcaster;
use tracing::{error, info, warn};

use crate::error::CronError;
use crate::executor::{ExecutionResult, JobExecutor, RETRY_INTERVAL_MS};
use crate::scheduler::{compute_next_run, validate_schedule, CronScheduler};
use crate::types::{
    cron_job_from_row, cron_job_to_response, cron_job_to_row, schedule_from_dto, CreatedBy,
    CronAgentConfig, CronJob, CronSchedule, ExecutionMode,
};

const PLACEHOLDER_PATTERNS: &[&str] = &[
    "todo:",
    "todo ",
    "fill in",
    "placeholder",
    "replace this",
    "your ",
    "insert ",
    "add your",
    "write your",
    "put your",
];

pub struct CronService {
    repo: Arc<dyn ICronRepository>,
    scheduler: Arc<CronScheduler>,
    executor: Arc<JobExecutor>,
    broadcaster: Arc<dyn EventBroadcaster>,
}

impl CronService {
    pub fn new(
        repo: Arc<dyn ICronRepository>,
        scheduler: Arc<CronScheduler>,
        executor: Arc<JobExecutor>,
        broadcaster: Arc<dyn EventBroadcaster>,
    ) -> Self {
        Self {
            repo,
            scheduler,
            executor,
            broadcaster,
        }
    }

    // -----------------------------------------------------------------------
    // CRUD
    // -----------------------------------------------------------------------

    pub async fn add_job(&self, req: CreateCronJobRequest) -> Result<CronJob, CronError> {
        let schedule = schedule_from_dto(&req.schedule);
        validate_schedule(&schedule)?;

        let execution_mode = parse_execution_mode(req.execution_mode.as_deref())?;
        let created_by = CreatedBy::from_str(&req.created_by)?;
        let message = req
            .message
            .or(req.prompt)
            .unwrap_or_default();

        let agent_config = req.agent_config.map(|c| CronAgentConfig {
            backend: c.backend,
            name: c.name,
            cli_path: c.cli_path,
            is_preset: c.is_preset,
            custom_agent_id: c.custom_agent_id,
            preset_agent_type: c.preset_agent_type,
            mode: c.mode,
            model_id: c.model_id,
            config_options: c.config_options,
            workspace: c.workspace,
        });

        let now = now_ms();
        let next_run_at = compute_next_run(&schedule, now);

        let job = CronJob {
            id: generate_prefixed_id("cron"),
            name: req.name,
            enabled: true,
            schedule,
            message,
            execution_mode,
            agent_config,
            conversation_id: req.conversation_id,
            conversation_title: req.conversation_title,
            agent_type: req.agent_type,
            created_by,
            skill_content: None,
            created_at: now,
            updated_at: now,
            next_run_at,
            last_run_at: None,
            last_status: None,
            last_error: None,
            run_count: 0,
            retry_count: 0,
            max_retries: 3,
        };

        let row = cron_job_to_row(&job)?;
        self.repo.insert(&row).await?;
        self.scheduler.schedule_job(&job);
        self.broadcast_event("cron.jobCreated", &cron_job_to_response(&job));

        info!(job_id = %job.id, name = %job.name, "Cron job created");
        Ok(job)
    }

    pub async fn update_job(
        &self,
        job_id: &str,
        req: UpdateCronJobRequest,
    ) -> Result<CronJob, CronError> {
        let existing_row = self
            .repo
            .get_by_id(job_id)
            .await?
            .ok_or_else(|| CronError::JobNotFound(job_id.to_owned()))?;
        let mut job = cron_job_from_row(existing_row)?;

        if let Some(name) = &req.name {
            job.name = name.clone();
        }
        if let Some(enabled) = req.enabled {
            job.enabled = enabled;
        }
        if let Some(schedule_dto) = &req.schedule {
            let schedule = schedule_from_dto(schedule_dto);
            validate_schedule(&schedule)?;
            job.schedule = schedule;
        }
        if let Some(message) = &req.message {
            job.message = message.clone();
        }
        if let Some(mode_str) = &req.execution_mode {
            job.execution_mode = parse_execution_mode(Some(mode_str))?;
        }
        if let Some(config_dto) = &req.agent_config {
            job.agent_config = Some(CronAgentConfig {
                backend: config_dto.backend.clone(),
                name: config_dto.name.clone(),
                cli_path: config_dto.cli_path.clone(),
                is_preset: config_dto.is_preset,
                custom_agent_id: config_dto.custom_agent_id.clone(),
                preset_agent_type: config_dto.preset_agent_type.clone(),
                mode: config_dto.mode.clone(),
                model_id: config_dto.model_id.clone(),
                config_options: config_dto.config_options.clone(),
                workspace: config_dto.workspace.clone(),
            });
        }
        if let Some(title) = &req.conversation_title {
            job.conversation_title = Some(title.clone());
        }
        if let Some(max_retries) = req.max_retries {
            job.max_retries = max_retries;
        }

        if req.schedule.is_some() || req.enabled.is_some() {
            job.next_run_at = compute_next_run(&job.schedule, now_ms());
        }

        job.updated_at = now_ms();

        let params = build_update_params(&job, &req);
        self.repo.update(job_id, &params).await?;

        self.scheduler.reschedule_job(&job);
        self.broadcast_event("cron.jobUpdated", &cron_job_to_response(&job));

        info!(job_id = %job.id, "Cron job updated");
        Ok(job)
    }

    pub async fn remove_job(&self, job_id: &str) -> Result<(), CronError> {
        self.scheduler.cancel_job(job_id);
        self.repo.delete(job_id).await?;
        self.broadcast_event(
            "cron.jobRemoved",
            &CronJobRemovedPayload {
                job_id: job_id.to_owned(),
            },
        );
        info!(job_id, "Cron job removed");
        Ok(())
    }

    pub async fn get_job(&self, job_id: &str) -> Result<CronJob, CronError> {
        let row = self
            .repo
            .get_by_id(job_id)
            .await?
            .ok_or_else(|| CronError::JobNotFound(job_id.to_owned()))?;
        cron_job_from_row(row)
    }

    pub async fn list_jobs(
        &self,
        query: &ListCronJobsQuery,
    ) -> Result<Vec<CronJob>, CronError> {
        let rows = if let Some(conv_id) = &query.conversation_id {
            self.repo.list_by_conversation(conv_id).await?
        } else {
            self.repo.list_all().await?
        };

        rows.into_iter().map(cron_job_from_row).collect()
    }

    // -----------------------------------------------------------------------
    // Init / Tick / Resume / RunNow
    // -----------------------------------------------------------------------

    pub async fn init(&self) {
        let rows = match self.repo.list_enabled().await {
            Ok(rows) => rows,
            Err(e) => {
                error!(error = %e, "Failed to load enabled cron jobs");
                return;
            }
        };

        let mut scheduled = 0u32;
        let mut orphans = 0u32;

        for row in rows {
            let job = match cron_job_from_row(row) {
                Ok(j) => j,
                Err(e) => {
                    error!(error = %e, "Failed to parse cron job row");
                    continue;
                }
            };

            if self.is_orphan(&job).await {
                if let Err(e) = self.repo.delete(&job.id).await {
                    error!(job_id = %job.id, error = %e, "Failed to delete orphan cron job");
                }
                orphans += 1;
                continue;
            }

            self.scheduler.schedule_job(&job);
            scheduled += 1;
        }

        info!(scheduled, orphans, "Cron service initialized");
    }

    pub async fn tick(&self, job_id: &str) {
        let row = match self.repo.get_by_id(job_id).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                warn!(job_id, "Tick: job not found, cancelling timer");
                self.scheduler.cancel_job(job_id);
                return;
            }
            Err(e) => {
                error!(job_id, error = %e, "Tick: failed to load job");
                return;
            }
        };

        let job = match cron_job_from_row(row) {
            Ok(j) => j,
            Err(e) => {
                error!(job_id, error = %e, "Tick: failed to parse job");
                return;
            }
        };

        if !job.enabled {
            info!(job_id, "Tick: job disabled, skipping");
            return;
        }

        let result = self.executor.execute(&job).await;
        self.handle_execution_result(job, result).await;
    }

    pub async fn handle_system_resume(&self) {
        let rows = match self.repo.list_enabled().await {
            Ok(r) => r,
            Err(e) => {
                error!(error = %e, "Resume: failed to load enabled jobs");
                return;
            }
        };

        let now = now_ms();

        for row in rows {
            let job = match cron_job_from_row(row) {
                Ok(j) => j,
                Err(e) => {
                    error!(error = %e, "Resume: failed to parse job");
                    continue;
                }
            };

            if let Some(next_run) = job.next_run_at
                && next_run < now
            {
                info!(job_id = %job.id, "Resume: missed job, executing immediately");
                let result = self.executor.execute(&job).await;
                self.handle_execution_result(job, result).await;
                continue;
            }

            self.scheduler.reschedule_job(&job);
        }

        info!("System resume: all cron timers rescheduled");
    }

    pub async fn run_now(&self, job_id: &str) -> Result<RunNowResponse, CronError> {
        let row = self
            .repo
            .get_by_id(job_id)
            .await?
            .ok_or_else(|| CronError::JobNotFound(job_id.to_owned()))?;
        let job = cron_job_from_row(row)?;

        let result = self.executor.execute_run_now(&job).await;

        match result {
            ExecutionResult::Success { conversation_id } => {
                self.update_job_after_success(job_id, &conversation_id)
                    .await;
                self.broadcast_executed_event(job_id, "ok", None);
                Ok(RunNowResponse { conversation_id })
            }
            ExecutionResult::Error { message } => {
                self.update_job_after_error(job_id, &message).await;
                self.broadcast_executed_event(job_id, "error", Some(&message));
                Err(CronError::Scheduler(message))
            }
            _ => Err(CronError::Scheduler("unexpected execution result".into())),
        }
    }

    // -----------------------------------------------------------------------
    // Skill management
    // -----------------------------------------------------------------------

    pub async fn save_skill(
        &self,
        job_id: &str,
        req: SaveCronSkillRequest,
    ) -> Result<(), CronError> {
        self.repo
            .get_by_id(job_id)
            .await?
            .ok_or_else(|| CronError::JobNotFound(job_id.to_owned()))?;

        validate_skill_content(&req.content)?;

        let params = UpdateCronJobParams {
            skill_content: Some(Some(req.content)),
            ..Default::default()
        };
        self.repo.update(job_id, &params).await?;

        info!(job_id, "Skill content saved");
        Ok(())
    }

    pub async fn has_skill(&self, job_id: &str) -> Result<HasSkillResponse, CronError> {
        let row = self
            .repo
            .get_by_id(job_id)
            .await?
            .ok_or_else(|| CronError::JobNotFound(job_id.to_owned()))?;

        let has_skill = row
            .skill_content
            .as_ref()
            .is_some_and(|s| !s.is_empty());

        Ok(HasSkillResponse { has_skill })
    }

    pub async fn delete_skill(&self, job_id: &str) -> Result<(), CronError> {
        self.repo
            .get_by_id(job_id)
            .await?
            .ok_or_else(|| CronError::JobNotFound(job_id.to_owned()))?;

        let params = UpdateCronJobParams {
            skill_content: Some(None),
            ..Default::default()
        };
        self.repo.update(job_id, &params).await?;

        info!(job_id, "Skill content deleted");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    pub fn to_response(job: &CronJob) -> CronJobResponse {
        cron_job_to_response(job)
    }

    async fn is_orphan(&self, job: &CronJob) -> bool {
        match self.executor.busy_guard().is_busy(&job.conversation_id) {
            true => false,
            false => {
                // Check if conversation still exists via repo
                // The executor holds a conversation_repo reference but we
                // use the cron repo's list_by_conversation as a proxy isn't right.
                // We re-verify by attempting tick-level check — for init we
                // just check the job's conversation existence.
                // Since we don't have direct conversation_repo access here,
                // delegate orphan detection to a simple heuristic: if the
                // conversation_id is empty, it's an orphan.
                job.conversation_id.is_empty()
            }
        }
    }

    async fn handle_execution_result(&self, job: CronJob, result: ExecutionResult) {
        let job_id = &job.id;

        match result {
            ExecutionResult::Success { conversation_id } => {
                self.update_job_after_success(job_id, &conversation_id)
                    .await;
                self.reschedule_after_execution(&job).await;
                self.broadcast_executed_event(job_id, "ok", None);
            }
            ExecutionResult::Retrying { attempt } => {
                let params = UpdateCronJobParams {
                    retry_count: Some(attempt),
                    ..Default::default()
                };
                if let Err(e) = self.repo.update(job_id, &params).await {
                    error!(job_id, error = %e, "Failed to update retry count");
                }
                self.schedule_retry(job_id, attempt);
            }
            ExecutionResult::Skipped => {
                let params = UpdateCronJobParams {
                    last_status: Some(Some("skipped".into())),
                    retry_count: Some(0),
                    ..Default::default()
                };
                if let Err(e) = self.repo.update(job_id, &params).await {
                    error!(job_id, error = %e, "Failed to update skipped status");
                }
                self.reschedule_after_execution(&job).await;
                self.broadcast_executed_event(job_id, "skipped", None);
            }
            ExecutionResult::Error { message } => {
                self.update_job_after_error(job_id, &message).await;
                self.reschedule_after_execution(&job).await;
                self.broadcast_executed_event(job_id, "error", Some(&message));
            }
        }
    }

    async fn update_job_after_success(&self, job_id: &str, _conversation_id: &str) {
        let run_count = match self.repo.get_by_id(job_id).await {
            Ok(Some(r)) => r.run_count,
            Ok(None) => return,
            Err(e) => {
                error!(job_id, error = %e, "Failed to read job for run_count");
                return;
            }
        };
        let now = now_ms();
        let params = UpdateCronJobParams {
            last_run_at: Some(Some(now)),
            last_status: Some(Some("ok".into())),
            last_error: Some(None),
            retry_count: Some(0),
            run_count: Some(run_count + 1),
            ..Default::default()
        };
        if let Err(e) = self.repo.update(job_id, &params).await {
            error!(job_id, error = %e, "Failed to update job after success");
        }
    }

    async fn update_job_after_error(&self, job_id: &str, message: &str) {
        let run_count = match self.repo.get_by_id(job_id).await {
            Ok(Some(r)) => r.run_count,
            Ok(None) => return,
            Err(e) => {
                error!(job_id, error = %e, "Failed to read job for run_count");
                return;
            }
        };
        let now = now_ms();
        let params = UpdateCronJobParams {
            last_run_at: Some(Some(now)),
            last_status: Some(Some("error".into())),
            last_error: Some(Some(message.to_owned())),
            retry_count: Some(0),
            run_count: Some(run_count + 1),
            ..Default::default()
        };
        if let Err(e) = self.repo.update(job_id, &params).await {
            error!(job_id, error = %e, "Failed to update job after error");
        }
    }

    async fn reschedule_after_execution(&self, job: &CronJob) {
        let is_at = matches!(job.schedule, CronSchedule::At { .. });
        if is_at {
            let params = UpdateCronJobParams {
                enabled: Some(false),
                next_run_at: Some(None),
                ..Default::default()
            };
            if let Err(e) = self.repo.update(&job.id, &params).await {
                error!(job_id = %job.id, error = %e, "Failed to disable at-type job");
            }
            self.scheduler.cancel_job(&job.id);

            let disabled = CronJob {
                enabled: false,
                next_run_at: None,
                ..job.clone()
            };
            self.broadcast_event("cron.jobUpdated", &cron_job_to_response(&disabled));

            info!(job_id = %job.id, "At-type job executed, auto-disabled");
            return;
        }

        let now = now_ms();
        let next = compute_next_run(&job.schedule, now);
        let updated = CronJob {
            next_run_at: next,
            ..job.clone()
        };
        let params = UpdateCronJobParams {
            next_run_at: Some(next),
            ..Default::default()
        };
        if let Err(e) = self.repo.update(&job.id, &params).await {
            error!(job_id = %job.id, error = %e, "Failed to update next_run_at");
        }
        self.scheduler.reschedule_job(&updated);
    }

    fn schedule_retry(&self, job_id: &str, _attempt: i64) {
        let next_run = now_ms() + RETRY_INTERVAL_MS as i64;
        let retry_job = CronJob {
            id: job_id.to_owned(),
            name: String::new(),
            enabled: true,
            schedule: CronSchedule::At {
                at_ms: next_run,
                description: None,
            },
            message: String::new(),
            execution_mode: ExecutionMode::Existing,
            agent_config: None,
            conversation_id: String::new(),
            conversation_title: None,
            agent_type: String::new(),
            created_by: CreatedBy::User,
            skill_content: None,
            created_at: 0,
            updated_at: 0,
            next_run_at: Some(next_run),
            last_run_at: None,
            last_status: None,
            last_error: None,
            run_count: 0,
            retry_count: 0,
            max_retries: 0,
        };
        self.scheduler.schedule_job(&retry_job);
    }

    fn broadcast_event<T: serde::Serialize>(&self, event_name: &str, payload: &T) {
        let value = match serde_json::to_value(payload) {
            Ok(v) => v,
            Err(e) => {
                error!(event_name, error = %e, "Failed to serialize event payload");
                return;
            }
        };
        self.broadcaster
            .broadcast(WebSocketMessage::new(event_name, value));
    }

    fn broadcast_executed_event(
        &self,
        job_id: &str,
        status: &str,
        error: Option<&str>,
    ) {
        self.broadcast_event(
            "cron.jobExecuted",
            &CronJobExecutedEvent {
                job_id: job_id.to_owned(),
                status: status.to_owned(),
                error: error.map(|s| s.to_owned()),
            },
        );
    }
}

// ---------------------------------------------------------------------------
// ICronService implementation (for middleware)
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl aionui_ai_agent::middleware::ICronService for CronService {
    async fn create_job(
        &self,
        _user_id: &str,
        conversation_id: &str,
        params: &aionui_ai_agent::middleware::CronCreateParams,
    ) -> aionui_ai_agent::middleware::CronCommandResult {
        let schedule_dto = CronScheduleDto::Cron {
            expr: params.schedule.clone(),
            tz: None,
            description: Some(params.schedule_description.clone()),
        };

        let req = CreateCronJobRequest {
            name: params.name.clone(),
            schedule: schedule_dto,
            prompt: None,
            message: Some(params.message.clone()),
            conversation_id: conversation_id.to_owned(),
            conversation_title: None,
            agent_type: "acp".to_owned(),
            created_by: "agent".to_owned(),
            execution_mode: Some("existing".to_owned()),
            agent_config: None,
        };

        match self.add_job(req).await {
            Ok(job) => aionui_ai_agent::middleware::CronCommandResult {
                success: true,
                message: format!(
                    "Created cron job '{}' ({})",
                    job.name, job.id
                ),
            },
            Err(e) => aionui_ai_agent::middleware::CronCommandResult {
                success: false,
                message: e.to_string(),
            },
        }
    }

    async fn list_jobs(
        &self,
        _user_id: &str,
    ) -> aionui_ai_agent::middleware::CronCommandResult {
        let query = ListCronJobsQuery::default();
        match self.list_jobs(&query).await {
            Ok(jobs) => {
                if jobs.is_empty() {
                    return aionui_ai_agent::middleware::CronCommandResult {
                        success: true,
                        message: "No cron jobs found.".into(),
                    };
                }

                let lines: Vec<String> = jobs
                    .iter()
                    .map(|j| {
                        let status = if j.enabled { "enabled" } else { "disabled" };
                        format!("- {} ({}) [{}]", j.name, j.id, status)
                    })
                    .collect();

                aionui_ai_agent::middleware::CronCommandResult {
                    success: true,
                    message: format!(
                        "Found {} cron job(s):\n{}",
                        jobs.len(),
                        lines.join("\n")
                    ),
                }
            }
            Err(e) => aionui_ai_agent::middleware::CronCommandResult {
                success: false,
                message: e.to_string(),
            },
        }
    }

    async fn delete_job(
        &self,
        _user_id: &str,
        job_id: &str,
    ) -> aionui_ai_agent::middleware::CronCommandResult {
        match self.remove_job(job_id).await {
            Ok(()) => aionui_ai_agent::middleware::CronCommandResult {
                success: true,
                message: format!("Deleted cron job '{job_id}'"),
            },
            Err(e) => aionui_ai_agent::middleware::CronCommandResult {
                success: false,
                message: e.to_string(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

fn parse_execution_mode(mode: Option<&str>) -> Result<ExecutionMode, CronError> {
    match mode {
        None | Some("existing") => Ok(ExecutionMode::Existing),
        Some(s) => ExecutionMode::from_str(s),
    }
}

fn validate_skill_content(content: &str) -> Result<(), CronError> {
    let trimmed = content.trim();

    if trimmed.is_empty() {
        return Err(CronError::InvalidSkillContent(
            "content must not be empty".into(),
        ));
    }

    let lower = trimmed.to_lowercase();
    for pattern in PLACEHOLDER_PATTERNS {
        if lower.starts_with(pattern) {
            return Err(CronError::InvalidSkillContent(
                "content appears to be placeholder text".into(),
            ));
        }
    }

    Ok(())
}

fn build_update_params(job: &CronJob, req: &UpdateCronJobRequest) -> UpdateCronJobParams {
    let (schedule_kind, schedule_value, schedule_tz, schedule_description) =
        if req.schedule.is_some() {
            let (k, v, tz, d) = schedule_to_row_fields(&job.schedule);
            (Some(k), Some(v), Some(tz), Some(d))
        } else {
            (None, None, None, None)
        };

    let agent_config = req.agent_config.as_ref().map(|c| {
        let config = CronAgentConfig {
            backend: c.backend.clone(),
            name: c.name.clone(),
            cli_path: c.cli_path.clone(),
            is_preset: c.is_preset,
            custom_agent_id: c.custom_agent_id.clone(),
            preset_agent_type: c.preset_agent_type.clone(),
            mode: c.mode.clone(),
            model_id: c.model_id.clone(),
            config_options: c.config_options.clone(),
            workspace: c.workspace.clone(),
        };
        Some(serde_json::to_string(&config).unwrap_or_default())
    });

    UpdateCronJobParams {
        name: req.name.clone(),
        enabled: req.enabled,
        schedule_kind,
        schedule_value,
        schedule_tz,
        schedule_description,
        payload_message: req.message.clone(),
        execution_mode: req.execution_mode.clone(),
        agent_config,
        conversation_id: None,
        conversation_title: req.conversation_title.as_ref().map(|t| Some(t.clone())),
        agent_type: None,
        skill_content: None,
        next_run_at: if req.schedule.is_some() || req.enabled.is_some() {
            Some(job.next_run_at)
        } else {
            None
        },
        last_run_at: None,
        last_status: None,
        last_error: None,
        run_count: None,
        retry_count: None,
    }
}

fn schedule_to_row_fields(
    schedule: &CronSchedule,
) -> (String, String, Option<String>, Option<String>) {
    match schedule {
        CronSchedule::At { at_ms, description } => (
            "at".to_owned(),
            at_ms.to_string(),
            None,
            description.clone(),
        ),
        CronSchedule::Every {
            every_ms,
            description,
        } => (
            "every".to_owned(),
            every_ms.to_string(),
            None,
            description.clone(),
        ),
        CronSchedule::Cron {
            expr,
            tz,
            description,
        } => (
            "cron".to_owned(),
            expr.clone(),
            tz.clone(),
            description.clone(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- validate_skill_content -----------------------------------------------

    #[test]
    fn validate_skill_empty_content() {
        let err = validate_skill_content("").unwrap_err();
        assert!(matches!(err, CronError::InvalidSkillContent(_)));
    }

    #[test]
    fn validate_skill_whitespace_only() {
        let err = validate_skill_content("   \n  ").unwrap_err();
        assert!(matches!(err, CronError::InvalidSkillContent(_)));
    }

    #[test]
    fn validate_skill_placeholder_todo() {
        let err = validate_skill_content("TODO: fill in later").unwrap_err();
        assert!(matches!(err, CronError::InvalidSkillContent(_)));
    }

    #[test]
    fn validate_skill_placeholder_fill_in() {
        let err = validate_skill_content("Fill in your instructions here").unwrap_err();
        assert!(matches!(err, CronError::InvalidSkillContent(_)));
    }

    #[test]
    fn validate_skill_placeholder_replace() {
        let err = validate_skill_content("Replace this with your skill").unwrap_err();
        assert!(matches!(err, CronError::InvalidSkillContent(_)));
    }

    #[test]
    fn validate_skill_valid_content() {
        assert!(validate_skill_content("---\nname: test\n---\nDo something useful").is_ok());
    }

    #[test]
    fn validate_skill_valid_short() {
        assert!(validate_skill_content("Run daily report").is_ok());
    }

    // -- parse_execution_mode -------------------------------------------------

    #[test]
    fn parse_mode_none_defaults_to_existing() {
        assert_eq!(
            parse_execution_mode(None).unwrap(),
            ExecutionMode::Existing
        );
    }

    #[test]
    fn parse_mode_existing() {
        assert_eq!(
            parse_execution_mode(Some("existing")).unwrap(),
            ExecutionMode::Existing
        );
    }

    #[test]
    fn parse_mode_new_conversation() {
        assert_eq!(
            parse_execution_mode(Some("new_conversation")).unwrap(),
            ExecutionMode::NewConversation
        );
    }

    #[test]
    fn parse_mode_invalid() {
        let err = parse_execution_mode(Some("parallel")).unwrap_err();
        assert!(matches!(err, CronError::InvalidExecutionMode(_)));
    }

    // -- build_update_params --------------------------------------------------

    fn sample_job() -> CronJob {
        CronJob {
            id: "cron_test".into(),
            name: "Test".into(),
            enabled: true,
            schedule: CronSchedule::Every {
                every_ms: 60000,
                description: None,
            },
            message: "do something".into(),
            execution_mode: ExecutionMode::Existing,
            agent_config: None,
            conversation_id: "conv_1".into(),
            conversation_title: None,
            agent_type: "acp".into(),
            created_by: CreatedBy::User,
            skill_content: None,
            created_at: 1000,
            updated_at: 2000,
            next_run_at: Some(61000),
            last_run_at: None,
            last_status: None,
            last_error: None,
            run_count: 0,
            retry_count: 0,
            max_retries: 3,
        }
    }

    #[test]
    fn build_update_params_name_only() {
        let job = sample_job();
        let req = UpdateCronJobRequest {
            name: Some("New Name".into()),
            enabled: None,
            schedule: None,
            message: None,
            execution_mode: None,
            agent_config: None,
            conversation_title: None,
            max_retries: None,
        };
        let params = build_update_params(&job, &req);
        assert_eq!(params.name.as_deref(), Some("New Name"));
        assert!(params.enabled.is_none());
        assert!(params.schedule_kind.is_none());
        assert!(params.next_run_at.is_none());
    }

    #[test]
    fn build_update_params_with_schedule_change() {
        let job = CronJob {
            schedule: CronSchedule::Cron {
                expr: "0 0 9 * * *".into(),
                tz: Some("UTC".into()),
                description: Some("daily".into()),
            },
            next_run_at: Some(99999),
            ..sample_job()
        };
        let req = UpdateCronJobRequest {
            name: None,
            enabled: None,
            schedule: Some(CronScheduleDto::Cron {
                expr: "0 0 9 * * *".into(),
                tz: Some("UTC".into()),
                description: Some("daily".into()),
            }),
            message: None,
            execution_mode: None,
            agent_config: None,
            conversation_title: None,
            max_retries: None,
        };
        let params = build_update_params(&job, &req);
        assert_eq!(params.schedule_kind.as_deref(), Some("cron"));
        assert_eq!(params.schedule_value.as_deref(), Some("0 0 9 * * *"));
        assert!(params.next_run_at.is_some());
    }

    #[test]
    fn build_update_params_enabled_change_triggers_next_run() {
        let job = sample_job();
        let req = UpdateCronJobRequest {
            name: None,
            enabled: Some(false),
            schedule: None,
            message: None,
            execution_mode: None,
            agent_config: None,
            conversation_title: None,
            max_retries: None,
        };
        let params = build_update_params(&job, &req);
        assert_eq!(params.enabled, Some(false));
        assert!(params.next_run_at.is_some());
    }

    // -- schedule_to_row_fields -----------------------------------------------

    #[test]
    fn row_fields_at() {
        let (kind, value, tz, desc) = schedule_to_row_fields(&CronSchedule::At {
            at_ms: 5000,
            description: Some("once".into()),
        });
        assert_eq!(kind, "at");
        assert_eq!(value, "5000");
        assert!(tz.is_none());
        assert_eq!(desc.as_deref(), Some("once"));
    }

    #[test]
    fn row_fields_every() {
        let (kind, value, tz, desc) = schedule_to_row_fields(&CronSchedule::Every {
            every_ms: 30000,
            description: None,
        });
        assert_eq!(kind, "every");
        assert_eq!(value, "30000");
        assert!(tz.is_none());
        assert!(desc.is_none());
    }

    #[test]
    fn row_fields_cron() {
        let (kind, value, tz, desc) = schedule_to_row_fields(&CronSchedule::Cron {
            expr: "0 0 * * * *".into(),
            tz: Some("UTC".into()),
            description: Some("hourly".into()),
        });
        assert_eq!(kind, "cron");
        assert_eq!(value, "0 0 * * * *");
        assert_eq!(tz.as_deref(), Some("UTC"));
        assert_eq!(desc.as_deref(), Some("hourly"));
    }
}
