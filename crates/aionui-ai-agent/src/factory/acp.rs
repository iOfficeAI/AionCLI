use std::sync::Arc;

use crate::agent_task::AgentInstance;
use crate::factory::AgentFactoryDeps;
use crate::factory::acp_assembler::{WorkspaceInfo, assemble_acp_params};
use crate::factory::context::FactoryContext;
use crate::manager::acp::{AcpAgentManager, CatalogForwarder};
use crate::types::BuildTaskOptions;
use aionui_api_types::AcpBuildExtra;
use aionui_common::{AppError, CommandSpec};
use tracing::{debug, info};

pub(super) async fn build(
    deps: Arc<AgentFactoryDeps>,
    options: BuildTaskOptions,
    ctx: FactoryContext,
) -> Result<AgentInstance, AppError> {
    let mut config: AcpBuildExtra = serde_json::from_value(options.extra)
        .map_err(|e| AppError::BadRequest(format!("Invalid ACP build options: {e}")))?;

    // Resolve the catalog row — prefer explicit agent_id, fall
    // back to a vendor-label match for legacy payloads.
    let meta = if let Some(ref agent_id) = config.agent_id {
        deps.agent_registry.get(agent_id).await
    } else if let Some(ref vendor) = config.backend {
        deps.agent_registry.find_builtin_by_backend(vendor).await
    } else {
        None
    }
    .ok_or_else(|| AppError::BadRequest("ACP agent requires either agent_id or backend in extra".into()))?;

    if config.backend.is_none() {
        config.backend.clone_from(&meta.backend);
    }

    // Inject Guide MCP config for solo (non-team) sessions.
    // Team sessions already carry `team_mcp_stdio_config`; the
    // two are mutually exclusive per the build_new_session_request guard.
    if config.team_mcp_stdio_config.is_some() {
        debug!(ctx.conversation_id, "guide_mcp: skipped: has team_mcp");
    } else if config.guide_mcp_config.is_some() {
        debug!(
            ctx.conversation_id,
            "guide_mcp: skipped: caller already set guide_mcp_config"
        );
    } else if deps.guide_mcp_config.is_none() {
        debug!(ctx.conversation_id, "guide_mcp: skipped: guide server not running");
    } else {
        config.guide_mcp_config.clone_from(&deps.guide_mcp_config);
        info!(
            ctx.conversation_id,
            guide_mcp_port = deps.guide_mcp_config.as_ref().map(|c| c.port),
            "guide_mcp: injected into solo session"
        );
    }

    // Registry resolved the spawn command via `which()` at
    // hydrate time. A missing `resolved_command` means either the
    // CLI was uninstalled between hydrate and now, or the row
    // never had a command (e.g. remote-only). Either way the
    // caller needs to see a BadRequest, not a confusing
    // spawn-time error.
    let (command, args, env, cwd) = (
        meta.resolved_command
            .clone()
            .ok_or_else(|| AppError::BadRequest(format!("Agent '{}' CLI not found in PATH", meta.name)))?,
        meta.args.clone(),
        meta.env
            .iter()
            .map(|e| aionui_common::EnvVar {
                name: e.name.clone(),
                value: e.value.clone(),
            })
            .collect(),
        Some(ctx.workspace.clone()),
    );
    let command_spec = CommandSpec {
        command,
        args,
        env,
        cwd,
    };
    let session_snapshot = deps.acp_agent_service.load_snapshot_state(&ctx.conversation_id).await;

    let params = Arc::new(
        assemble_acp_params(
            ctx.conversation_id.clone(),
            WorkspaceInfo {
                path: ctx.workspace,
                is_custom: ctx.is_custom_workspace,
            },
            meta,
            command_spec,
            config,
            session_snapshot,
        )
        .await,
    );

    let skill_mgr = deps.skill_manager.clone();
    let catalog_tx = deps.agent_registry.catalog_sender();

    let (agent, domain_rx, notification_rx) = AcpAgentManager::build(params, skill_mgr, &catalog_tx).await?;

    let arc = Arc::new(agent);
    arc.start_permission_handler();
    arc.start_session_event_tracker(notification_rx);
    CatalogForwarder::spawn(
        arc.agent_metadata_id().to_owned(),
        crate::IAgentTask::subscribe(arc.as_ref()),
        catalog_tx,
    );

    // Desired (mode/model/config) are seeded from `params.session_snapshot`
    // inside `AcpAgentManager::new`. The CLI-assigned session id is still
    // loaded here so the first turn after a task rebuild takes the resume
    // path.
    if let Some(sid) = deps.acp_agent_service.load_session_id(&ctx.conversation_id).await {
        arc.restore_session_id(sid).await;
    }

    let instance = AgentInstance::Acp(Arc::clone(&arc));

    // Hand the service the domain event receiver so it can
    // persist user intent changes without reverse-engineering
    // them from CLI observations.
    deps.acp_agent_service.attach(ctx.conversation_id, domain_rx).await;

    Ok(instance)
}
