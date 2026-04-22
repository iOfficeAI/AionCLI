use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};

use aionui_api_types::{
    ApiResponse, ConversationResponse, CreateCronJobRequest, CronJobResponse, HasSkillResponse,
    ListCronJobsQuery, RunNowResponse, SaveCronSkillRequest, UpdateCronJobRequest,
};
use aionui_auth::CurrentUser;
use aionui_common::AppError;

use crate::service::CronService;
use crate::state::CronRouterState;

pub fn cron_routes(state: CronRouterState) -> Router {
    Router::new()
        .route("/api/cron/jobs", get(list_jobs).post(create_job))
        .route(
            "/api/cron/jobs/{id}",
            get(get_job).put(update_job).delete(delete_job),
        )
        .route("/api/cron/jobs/{id}/run", post(run_now))
        .route(
            "/api/cron/jobs/{id}/conversations",
            get(list_conversations_by_cron_job),
        )
        .route("/api/cron/jobs/{id}/skill", get(has_skill).post(save_skill))
        .with_state(state)
}

async fn create_job(
    State(state): State<CronRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<CreateCronJobRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ApiResponse<CronJobResponse>>), AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let job = state.cron_service.add_job(req).await?;
    let resp = CronService::to_response(&job);
    Ok((StatusCode::CREATED, Json(ApiResponse::ok(resp))))
}

async fn list_jobs(
    State(state): State<CronRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Query(query): Query<ListCronJobsQuery>,
) -> Result<Json<ApiResponse<Vec<CronJobResponse>>>, AppError> {
    let jobs = state.cron_service.list_jobs(&query).await?;
    let items: Vec<CronJobResponse> = jobs.iter().map(CronService::to_response).collect();
    Ok(Json(ApiResponse::ok(items)))
}

async fn get_job(
    State(state): State<CronRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<CronJobResponse>>, AppError> {
    let job = state.cron_service.get_job(&id).await?;
    Ok(Json(ApiResponse::ok(CronService::to_response(&job))))
}

async fn update_job(
    State(state): State<CronRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<UpdateCronJobRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<CronJobResponse>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let job = state.cron_service.update_job(&id, req).await?;
    Ok(Json(ApiResponse::ok(CronService::to_response(&job))))
}

async fn delete_job(
    State(state): State<CronRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    state.cron_service.remove_job(&id).await?;
    Ok(Json(ApiResponse::success()))
}

async fn run_now(
    State(state): State<CronRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<RunNowResponse>>, AppError> {
    let resp = state.cron_service.run_now(&id).await?;
    Ok(Json(ApiResponse::ok(resp)))
}

async fn save_skill(
    State(state): State<CronRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<SaveCronSkillRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    state.cron_service.save_skill(&id, req).await?;
    Ok(Json(ApiResponse::success()))
}

async fn list_conversations_by_cron_job(
    State(state): State<CronRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<ConversationResponse>>>, AppError> {
    let items = state
        .conversation_service
        .list_by_cron_job(&user.id, &id)
        .await?;
    Ok(Json(ApiResponse::ok(items)))
}

async fn has_skill(
    State(state): State<CronRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<HasSkillResponse>>, AppError> {
    let resp = state.cron_service.has_skill(&id).await?;
    Ok(Json(ApiResponse::ok(resp)))
}
