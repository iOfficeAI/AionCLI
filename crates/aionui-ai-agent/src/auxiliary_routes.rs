use std::sync::Arc;

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, Path, Query, State};
use axum::routing::{get, post};
use std::path::Component;

use crate::acp_agent::AcpAgentManager;
use crate::agent_manager::AgentManagerHandle;
use crate::openclaw::OpenClawAgentManager;
use crate::task_manager::IWorkerTaskManager;
use agent_client_protocol::schema::{AgentCapabilities, SessionConfigOption, UsageUpdate};
use aionui_api_types::{
    AgentModeResponse, ApiResponse, GetModelInfoResponse, ModelInfoEntry, ModelInfoPayload, SetConfigOptionRequest,
    SetConfigOptionsRequest, SetModeRequest, SetModelRequest, SideQuestionRequest, SideQuestionResponse,
    SlashCommandItem, WorkspaceBrowseQuery, WorkspaceEntry,
};
use aionui_auth::CurrentUser;
use aionui_common::{AgentType, AppError};
use aionui_db::IConversationRepository;
use serde::Deserialize;

/// Router state for auxiliary conversation routes.
#[derive(Clone)]
pub struct AuxiliaryRouterState {
    pub worker_task_manager: Arc<dyn IWorkerTaskManager>,
    pub conversation_repo: Arc<dyn IConversationRepository>,
}

/// Build the auxiliary routes router.
pub fn auxiliary_routes(state: AuxiliaryRouterState) -> Router {
    Router::new()
        .route("/api/conversations/{id}/workspace", get(browse_workspace))
        .route("/api/conversations/{id}/reload-context", post(reload_context))
        .route("/api/conversations/{id}/side-question", post(side_question))
        .route("/api/conversations/{id}/slash-commands", get(get_slash_commands))
        .route("/api/conversations/{id}/mode", get(get_mode).put(set_mode))
        .route("/api/conversations/{id}/model", get(get_model).put(set_model))
        .route("/api/conversations/{id}/config", get(get_configs).put(set_configs))
        .route(
            "/api/conversations/{id}/config/{configId}",
            get(get_config).put(set_config),
        )
        .route("/api/conversations/{id}/usage", get(get_usage))
        .route(
            "/api/conversations/{id}/agent-capabilities",
            get(get_agent_capabilities),
        )
        .route("/api/conversations/{id}/openclaw/runtime", get(get_openclaw_runtime))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct ConfigPathParams {
    id: String,
    #[serde(rename = "configId")]
    config_id: String,
}

// ── Max depth for workspace traversal ──────────────────────────────
const MAX_DIR_DEPTH: usize = 10;

// ── Route handlers ─────────────────────────────────────────────────

async fn browse_workspace(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    Query(query): Query<WorkspaceBrowseQuery>,
) -> Result<Json<ApiResponse<Vec<WorkspaceEntry>>>, AppError> {
    if query.path.trim().is_empty() {
        return Err(AppError::BadRequest("path must not be empty".into()));
    }

    let row = state
        .conversation_repo
        .get(&id)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to load conversation: {e}")))?
        .ok_or_else(|| AppError::NotFound(format!("Conversation '{id}' not found")))?;

    let extra: serde_json::Value =
        serde_json::from_str(&row.extra).map_err(|e| AppError::Internal(format!("Invalid extra JSON: {e}")))?;
    let workspace = extra
        .get("workspace")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    if workspace.is_empty() {
        return Err(AppError::BadRequest("Conversation has no workspace assigned".into()));
    }

    let relative_path = query.path.trim_start_matches('/');
    let relative_path_obj = std::path::Path::new(relative_path);
    if relative_path_obj
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(AppError::BadRequest(
            "Path traversal outside workspace is not allowed".into(),
        ));
    }

    // Resolve the browsed path relative to the workspace root
    let base = std::path::Path::new(&workspace);
    let browse_path = if relative_path.is_empty() {
        base.to_path_buf()
    } else {
        base.join(relative_path_obj)
    };

    // Security: reject direct traversal outside the workspace root, but allow
    // symlinked directories mounted inside the workspace (e.g. native skill
    // dirs that point at the builtin skills corpus under data-dir).
    let canonical_base = base
        .canonicalize()
        .map_err(|e| AppError::Internal(format!("Failed to resolve workspace path: {e}")))?;
    let canonical_browse = browse_path
        .canonicalize()
        .map_err(|_| AppError::NotFound("Directory not found".into()))?;
    if !browse_path.starts_with(base) && !canonical_browse.starts_with(&canonical_base) {
        return Err(AppError::BadRequest(
            "Path traversal outside workspace is not allowed".into(),
        ));
    }

    // Check depth limit
    let depth = relative_path_obj.components().count();
    if depth > MAX_DIR_DEPTH {
        return Err(AppError::BadRequest(format!(
            "Directory depth exceeds maximum of {MAX_DIR_DEPTH}"
        )));
    }

    let mut entries = Vec::new();
    let mut dir_reader = tokio::fs::read_dir(&canonical_browse)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read directory: {e}")))?;

    while let Ok(Some(entry)) = dir_reader.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();

        // Apply search filter if provided
        if let Some(ref search) = query.search
            && !search.is_empty()
            && !name.to_lowercase().contains(&search.to_lowercase())
        {
            continue;
        }

        let entry_path = entry.path();
        let metadata = tokio::fs::metadata(&entry_path)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to read entry metadata: {e}")))?;

        let entry_type = if metadata.is_dir() { "directory" } else { "file" };

        entries.push(WorkspaceEntry {
            name,
            entry_type: entry_type.into(),
        });
    }

    // Sort: directories first, then alphabetically
    entries.sort_by(|a, b| {
        let type_cmp = a.entry_type.cmp(&b.entry_type);
        if type_cmp == std::cmp::Ordering::Equal {
            a.name.to_lowercase().cmp(&b.name.to_lowercase())
        } else {
            type_cmp
        }
    });

    Ok(Json(ApiResponse::ok(entries)))
}

async fn reload_context(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let _handle = get_task(&state, &id)?;

    // Context reload triggers re-discovery of skills and workspace state.
    // The specific reload behavior varies by agent type and will be
    // fully integrated in Phase 6.15. For now, acknowledge the request.
    Ok(Json(ApiResponse::success()))
}

async fn side_question(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    Json(req): Json<SideQuestionRequest>,
) -> Result<Json<ApiResponse<SideQuestionResponse>>, AppError> {
    if req.question.trim().is_empty() {
        return Err(AppError::BadRequest("question must not be empty".into()));
    }

    let handle = get_task(&state, &id)?;

    // Only ACP agents support side questions
    if handle.agent_type() != AgentType::Acp {
        return Ok(Json(ApiResponse::ok(SideQuestionResponse {
            status: "unsupported".into(),
            answer: None,
        })));
    }

    let acp = downcast_acp(&handle)?;

    // Side question is gated by the agent's behavior_policy flag.
    if !acp.supports_side_question() {
        return Ok(Json(ApiResponse::ok(SideQuestionResponse {
            status: "unsupported".into(),
            answer: None,
        })));
    }

    // Side question is implemented by sending a special message to the ACP CLI.
    // The actual implementation requires forking the ACP session, which will
    // be fully wired in Phase 6.15 App Integration.
    // For now, return a placeholder indicating the feature exists but is pending integration.
    Ok(Json(ApiResponse::ok(SideQuestionResponse {
        status: "ok".into(),
        answer: Some("Side question support will be fully wired in app integration phase.".into()),
    })))
}

async fn get_slash_commands(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<SlashCommandItem>>>, AppError> {
    let handle = get_task(&state, &id)?;

    // Only ACP agents have slash commands
    if handle.agent_type() != AgentType::Acp {
        return Ok(Json(ApiResponse::ok(Vec::new())));
    }

    let acp = downcast_acp(&handle)?;
    let commands = acp.load_slash_commands().await?;
    Ok(Json(ApiResponse::ok(commands)))
}

async fn get_mode(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<AgentModeResponse>>, AppError> {
    let handle = get_task(&state, &id)?;
    Ok(Json(ApiResponse::ok(handle.get_mode().await?)))
}

async fn set_mode(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<SetModeRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    if req.mode.trim().is_empty() {
        return Err(AppError::BadRequest("mode must not be empty".into()));
    }
    let handle = get_task(&state, &id)?;
    handle.set_mode(&req.mode).await?;
    Ok(Json(ApiResponse::success()))
}

async fn get_model(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<GetModelInfoResponse>>, AppError> {
    let handle = get_task(&state, &id)?;

    if handle.agent_type() != AgentType::Acp {
        return Err(AppError::BadRequest(
            "This endpoint is only available for ACP agents".into(),
        ));
    }

    let acp = downcast_acp(&handle)?;
    let sdk_model = acp.model_info().await;

    let model_info = sdk_model.map(|m| {
        let available: Vec<ModelInfoEntry> = m
            .available_models
            .iter()
            .map(|am| ModelInfoEntry {
                id: am.model_id.to_string(),
                label: am.name.clone(),
            })
            .collect();

        let current_id = m.current_model_id.to_string();
        let current_label = available
            .iter()
            .find(|e| e.id == current_id)
            .map(|e| e.label.clone())
            .unwrap_or_else(|| current_id.clone());

        ModelInfoPayload {
            current_model_id: Some(current_id),
            current_model_label: Some(current_label),
            available_models: available,
        }
    });

    Ok(Json(ApiResponse::ok(GetModelInfoResponse { model_info })))
}

async fn set_model(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<SetModelRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    if req.model_id.trim().is_empty() {
        return Err(AppError::BadRequest("model_id must not be empty".into()));
    }

    let handle = get_task(&state, &id)?;
    if handle.agent_type() != AgentType::Acp {
        return Err(AppError::BadRequest(
            "Model switching is not supported for this agent type".into(),
        ));
    }

    let acp = downcast_acp(&handle)?;
    acp.set_model_info(&req.model_id).await?;
    Ok(Json(ApiResponse::success()))
}

async fn get_config(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(params): Path<ConfigPathParams>,
) -> Result<Json<ApiResponse<Option<SessionConfigOption>>>, AppError> {
    let handle = get_task(&state, &params.id)?;

    if handle.agent_type() != AgentType::Acp {
        return Err(AppError::BadRequest(
            "This endpoint is only available for ACP agents".into(),
        ));
    }

    let acp = downcast_acp(&handle)?;
    let config_option = acp
        .config_options()
        .await
        .into_iter()
        .find(|opt| *opt.id.0 == *params.config_id);
    Ok(Json(ApiResponse::ok(config_option)))
}

async fn set_config(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(params): Path<ConfigPathParams>,
    body: Result<Json<SetConfigOptionRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let handle = get_task(&state, &params.id)?;

    if handle.agent_type() != AgentType::Acp {
        return Err(AppError::BadRequest(
            "Config updates are not supported for this agent type".into(),
        ));
    }

    let acp = downcast_acp(&handle)?;
    acp.set_config_option(&params.config_id, &req.value).await?;
    Ok(Json(ApiResponse::success()))
}

async fn get_configs(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<SessionConfigOption>>>, AppError> {
    let handle = get_task(&state, &id)?;

    if handle.agent_type() != AgentType::Acp {
        return Err(AppError::BadRequest(
            "This endpoint is only available for ACP agents".into(),
        ));
    }

    let acp = downcast_acp(&handle)?;
    Ok(Json(ApiResponse::ok(acp.config_options().await)))
}

async fn set_configs(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<SetConfigOptionsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let handle = get_task(&state, &id)?;

    if handle.agent_type() != AgentType::Acp {
        return Err(AppError::BadRequest(
            "Config updates are not supported for this agent type".into(),
        ));
    }

    let acp = downcast_acp(&handle)?;
    for update in req.config_options {
        if update.config_id.trim().is_empty() {
            return Err(AppError::BadRequest("config_id must not be empty".into()));
        }
        acp.set_config_option(&update.config_id, &update.value).await?;
    }

    Ok(Json(ApiResponse::success()))
}

async fn get_usage(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Option<UsageUpdate>>>, AppError> {
    let handle = get_task(&state, &id)?;

    if handle.agent_type() != AgentType::Acp {
        return Err(AppError::BadRequest(
            "This endpoint is only available for ACP agents".into(),
        ));
    }

    let acp = downcast_acp(&handle)?;
    let usage = acp.usage().await;
    Ok(Json(ApiResponse::ok(usage)))
}

async fn get_agent_capabilities(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Option<AgentCapabilities>>>, AppError> {
    let handle = get_task(&state, &id)?;

    if handle.agent_type() != AgentType::Acp {
        return Err(AppError::BadRequest(
            "This endpoint is only available for ACP agents".into(),
        ));
    }

    let acp = downcast_acp(&handle)?;
    let capabilities = acp.agent_capabilities().await;
    Ok(Json(ApiResponse::ok(capabilities)))
}

async fn get_openclaw_runtime(
    State(state): State<AuxiliaryRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let handle = get_task(&state, &id)?;

    if handle.agent_type() != AgentType::OpenclawGateway {
        return Err(AppError::BadRequest(
            "This endpoint is only available for OpenClaw agents".into(),
        ));
    }

    let openclaw = handle
        .as_any()
        .downcast_ref::<OpenClawAgentManager>()
        .ok_or_else(|| AppError::Internal("Failed to downcast to OpenClawAgentManager".into()))?;

    let diagnostics = openclaw.get_diagnostics().await;
    Ok(Json(ApiResponse::ok(diagnostics)))
}

// ── Helpers ────────────────────────────────────────────────────────

fn get_task(state: &AuxiliaryRouterState, conversation_id: &str) -> Result<AgentManagerHandle, AppError> {
    state
        .worker_task_manager
        .get_task(conversation_id)
        .ok_or_else(|| AppError::NotFound(format!("No active agent for conversation '{conversation_id}'")))
}

fn downcast_acp(handle: &AgentManagerHandle) -> Result<&AcpAgentManager, AppError> {
    handle
        .as_any()
        .downcast_ref::<AcpAgentManager>()
        .ok_or_else(|| AppError::Internal("Failed to downcast to AcpAgentManager".into()))
}
