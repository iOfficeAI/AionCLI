use aionui_api_types::{CreateTeamRequest, TeamAgentInput, TeamResponse};
use aionui_common::AppError;
use serde_json::{Value, json};
use tracing::info;

use crate::service::TeamSessionService;

/// User ID used when the Guide MCP spawns a team on behalf of a solo agent.
/// Backend is single-tenant today; revisit when multi-tenant lands.
const MCP_SPAWN_USER_ID: &str = "system_default_user";

/// Default role string when the Guide spawns the lead agent.
const LEAD_ROLE: &str = "lead";

/// Default model passed to the lead agent when the caller does not specify;
/// empty string defers to the backend's own default routing.
const DEFAULT_LEAD_MODEL: &str = "";

#[derive(Debug, Clone)]
pub struct CreateTeamParams {
    pub summary: String,
    pub name: String,
    pub workspace: String,
}

/// Parse `aion_create_team` tool arguments into structured params.
///
/// Defaults:
/// - `name` falls back to the first 5 whitespace-separated tokens of `summary`.
/// - `workspace` falls back to the caller's workspace, then to `"."`.
pub fn parse_create_team_args(args: &Value, caller_workspace: Option<&str>) -> Result<CreateTeamParams, String> {
    let summary = args
        .get("summary")
        .and_then(Value::as_str)
        .ok_or("missing required field: summary")?
        .to_owned();

    let name = args
        .get("name")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| summary.split_whitespace().take(5).collect::<Vec<_>>().join(" "));

    let workspace = args
        .get("workspace")
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| caller_workspace.map(String::from))
        .unwrap_or_else(|| ".".to_owned());

    Ok(CreateTeamParams {
        summary,
        name,
        workspace,
    })
}

/// Build the `CreateTeamRequest` sent to `TeamSessionService::create_team`
/// from parsed `aion_create_team` args.
///
/// The lead agent inherits the caller's backend and a default role of
/// `"lead"`; `TeamSessionService::create_team` promotes the first agent in
/// the vector to the team lead regardless of the `role` string value.
///
/// # TODO (W3-D15b)
/// Once `TeamAgentInput::conversation_id` lands on this branch (PR #71),
/// pass `Some(caller_conversation_id)` so the lead reuses the solo agent's
/// existing conversation instead of spawning a fresh one. Until then,
/// `caller_conversation_id` is accepted for signature stability and logged
/// for traceability.
pub fn build_create_team_request(params: &CreateTeamParams, backend: &str) -> CreateTeamRequest {
    let lead = TeamAgentInput {
        name: params.name.clone(),
        role: LEAD_ROLE.to_owned(),
        backend: backend.to_owned(),
        model: DEFAULT_LEAD_MODEL.to_owned(),
        custom_agent_id: None,
    };
    CreateTeamRequest {
        name: params.name.clone(),
        agents: vec![lead],
    }
}

/// Shape the MCP tool response returned to the calling agent once the team
/// has been persisted.
///
/// The `next_step` copy is lifted from the AionUi reference bridge so an
/// upstream LLM seeing the result recognises that the frontend has already
/// surfaced the new team and it should terminate its current turn rather
/// than continue operating in the solo conversation.
pub fn format_create_team_response(team: &TeamResponse) -> Value {
    let lead_agent = team.agents.iter().find(|a| a.role == LEAD_ROLE);
    json!({
        "team_id": team.id,
        "name": team.name,
        "route": format!("/team/{}", team.id),
        "lead_agent": lead_agent.map(|a| json!({
            "slot_id": a.slot_id,
            "name": a.name,
            "conversation_id": a.conversation_id,
        })),
        "status": "team_created",
        "next_step": "The team page has been opened automatically. End your turn now.",
    })
}

/// MCP handler for the `aion_create_team` tool.
///
/// Parses args, constructs the service request, persists the team, and
/// returns a structured JSON payload for the calling agent.
pub async fn handle_aion_create_team(
    service: &TeamSessionService,
    args: &Value,
    backend: &str,
    caller_conversation_id: &str,
) -> Result<Value, AppError> {
    // Solo agents today only ever invoke the Guide with their own workspace
    // in their conversation.extra; we do not yet propagate it through the
    // bridge, so caller_workspace is `None` until W5-D27 lands.
    let params = parse_create_team_args(args, None).map_err(AppError::BadRequest)?;

    info!(
        team_name = %params.name,
        caller_conversation_id = %caller_conversation_id,
        backend = %backend,
        "aion_create_team invoked"
    );

    let req = build_create_team_request(&params, backend);
    let team = service.create_team(MCP_SPAWN_USER_ID, req).await?;
    Ok(format_create_team_response(&team))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn errors_when_summary_missing() {
        let args = json!({ "name": "alpha", "workspace": "/tmp" });
        let err = parse_create_team_args(&args, None).unwrap_err();
        assert!(err.contains("summary"), "unexpected error: {err}");
    }

    #[test]
    fn errors_when_summary_not_string() {
        let args = json!({ "summary": 42 });
        let err = parse_create_team_args(&args, None).unwrap_err();
        assert!(err.contains("summary"), "unexpected error: {err}");
    }

    #[test]
    fn name_defaults_to_first_five_summary_words() {
        let args = json!({
            "summary": "implement login flow and add OAuth provider support end-to-end",
        });
        let params = parse_create_team_args(&args, None).unwrap();
        assert_eq!(params.name, "implement login flow and add");
        assert_eq!(
            params.summary,
            "implement login flow and add OAuth provider support end-to-end"
        );
    }

    #[test]
    fn name_defaults_use_all_summary_when_shorter_than_five_words() {
        let args = json!({ "summary": "hello world" });
        let params = parse_create_team_args(&args, None).unwrap();
        assert_eq!(params.name, "hello world");
    }

    #[test]
    fn workspace_inherits_from_caller_when_missing() {
        let args = json!({ "summary": "do work" });
        let params = parse_create_team_args(&args, Some("/caller/ws")).unwrap();
        assert_eq!(params.workspace, "/caller/ws");
    }

    #[test]
    fn workspace_defaults_to_dot_when_caller_absent() {
        let args = json!({ "summary": "do work" });
        let params = parse_create_team_args(&args, None).unwrap();
        assert_eq!(params.workspace, ".");
    }

    #[test]
    fn custom_fields_take_precedence_over_defaults() {
        let args = json!({
            "summary": "refactor the scheduler end-to-end",
            "name": "scheduler-refactor",
            "workspace": "/repo/path",
        });
        let params = parse_create_team_args(&args, Some("/caller/ws")).unwrap();
        assert_eq!(params.summary, "refactor the scheduler end-to-end");
        assert_eq!(params.name, "scheduler-refactor");
        assert_eq!(params.workspace, "/repo/path");
    }

    #[test]
    fn non_string_name_falls_back_to_summary_prefix() {
        let args = json!({
            "summary": "one two three four five six",
            "name": 123,
        });
        let params = parse_create_team_args(&args, None).unwrap();
        assert_eq!(params.name, "one two three four five");
    }

    #[test]
    fn non_string_workspace_falls_back_to_caller() {
        let args = json!({
            "summary": "do work",
            "workspace": 42,
        });
        let params = parse_create_team_args(&args, Some("/caller/ws")).unwrap();
        assert_eq!(params.workspace, "/caller/ws");
    }

    // -----------------------------------------------------------------------
    // build_create_team_request
    // -----------------------------------------------------------------------

    #[test]
    fn build_request_uses_parsed_name_for_team_and_lead() {
        let params = CreateTeamParams {
            summary: "draft the onboarding flow".into(),
            name: "onboarding-flow".into(),
            workspace: ".".into(),
        };
        let req = build_create_team_request(&params, "claude");
        assert_eq!(req.name, "onboarding-flow");
        assert_eq!(req.agents.len(), 1);
        assert_eq!(req.agents[0].name, "onboarding-flow");
    }

    #[test]
    fn build_request_lead_inherits_caller_backend() {
        let params = CreateTeamParams {
            summary: "x".into(),
            name: "alpha".into(),
            workspace: ".".into(),
        };
        let req = build_create_team_request(&params, "codex");
        assert_eq!(req.agents[0].backend, "codex");
        // The service promotes agents[0] to Lead regardless of the role
        // string, but we still emit "lead" here for consistency with the
        // REST contract and to make the intent obvious in persisted rows.
        assert_eq!(req.agents[0].role, "lead");
    }

    #[test]
    fn build_request_lead_has_no_custom_agent_id() {
        let params = CreateTeamParams {
            summary: "x".into(),
            name: "alpha".into(),
            workspace: ".".into(),
        };
        let req = build_create_team_request(&params, "claude");
        assert!(req.agents[0].custom_agent_id.is_none());
    }

    // -----------------------------------------------------------------------
    // format_create_team_response
    // -----------------------------------------------------------------------

    fn sample_team_response(with_lead: bool) -> TeamResponse {
        use aionui_api_types::TeamAgentResponse;
        let mut agents = vec![];
        if with_lead {
            agents.push(TeamAgentResponse {
                slot_id: "slot-lead".into(),
                name: "alpha".into(),
                role: "lead".into(),
                conversation_id: "conv-lead".into(),
                backend: "claude".into(),
                model: "".into(),
                custom_agent_id: None,
                status: None,
            });
        }
        TeamResponse {
            id: "team-123".into(),
            name: "alpha".into(),
            agents,
            lead_agent_id: if with_lead { Some("slot-lead".into()) } else { None },
            created_at: 1_700_000_000_000,
            updated_at: 1_700_000_000_000,
        }
    }

    #[test]
    fn response_carries_team_id_name_route_and_status() {
        let team = sample_team_response(true);
        let out = format_create_team_response(&team);
        assert_eq!(out["team_id"], "team-123");
        assert_eq!(out["name"], "alpha");
        assert_eq!(out["route"], "/team/team-123");
        assert_eq!(out["status"], "team_created");
    }

    #[test]
    fn response_next_step_instructs_agent_to_end_turn() {
        let team = sample_team_response(true);
        let out = format_create_team_response(&team);
        let next = out["next_step"].as_str().unwrap_or_default();
        assert!(
            next.contains("End your turn"),
            "next_step must steer the calling agent to stop; got: {next}"
        );
    }

    #[test]
    fn response_lead_agent_echoes_service_assigned_slot() {
        let team = sample_team_response(true);
        let out = format_create_team_response(&team);
        assert_eq!(out["lead_agent"]["slot_id"], "slot-lead");
        assert_eq!(out["lead_agent"]["conversation_id"], "conv-lead");
        assert_eq!(out["lead_agent"]["name"], "alpha");
    }

    #[test]
    fn response_lead_agent_is_null_when_service_returns_no_agents() {
        // Defensive: should never happen because create_team rejects empty
        // agent lists, but lock the shape so callers receive an explicit
        // null rather than a field-missing shape drift.
        let team = sample_team_response(false);
        let out = format_create_team_response(&team);
        assert!(out["lead_agent"].is_null());
    }
}
