use std::path::PathBuf;
use std::sync::Arc;

use aionui_api_types::{
    AddAgentRequest, CreateConversationRequest, CreateTeamRequest, TeamAgentResponse, TeamResponse,
};
use aionui_common::{AgentType, ProviderWithModel, generate_id, now_ms};
use aionui_conversation::ConversationService;
use aionui_db::models::TeamRow;
use aionui_db::{ITeamRepository, UpdateTeamParams};
use aionui_realtime::EventBroadcaster;
use dashmap::DashMap;
use tracing::info;

use crate::error::TeamError;
use crate::session::TeamSession;
use crate::types::{Team, TeamAgent, TeammateRole};

pub struct TeamSessionService {
    repo: Arc<dyn ITeamRepository>,
    conversation_service: ConversationService,
    broadcaster: Arc<dyn EventBroadcaster>,
    backend_binary_path: Arc<PathBuf>,
    sessions: DashMap<String, TeamSession>,
}

impl TeamSessionService {
    pub fn new(
        repo: Arc<dyn ITeamRepository>,
        conversation_service: ConversationService,
        broadcaster: Arc<dyn EventBroadcaster>,
        backend_binary_path: Arc<PathBuf>,
    ) -> Self {
        Self {
            repo,
            conversation_service,
            broadcaster,
            backend_binary_path,
            sessions: DashMap::new(),
        }
    }

    pub async fn create_team(
        &self,
        user_id: &str,
        req: CreateTeamRequest,
    ) -> Result<TeamResponse, TeamError> {
        if req.agents.is_empty() {
            return Err(TeamError::InvalidRequest(
                "at least one agent is required".into(),
            ));
        }

        let team_id = generate_id();
        let now = now_ms();
        let mut agents = Vec::with_capacity(req.agents.len());

        for (i, input) in req.agents.iter().enumerate() {
            let slot_id = generate_id();
            let role = if i == 0 {
                TeammateRole::Lead
            } else {
                TeammateRole::parse(&input.role).unwrap_or(TeammateRole::Teammate)
            };

            let agent_type = parse_agent_type(&input.backend)?;
            let conv_req = CreateConversationRequest {
                r#type: agent_type,
                name: Some(input.name.clone()),
                model: Some(ProviderWithModel {
                    provider_id: input.backend.clone(),
                    model: input.model.clone(),
                    use_model: None,
                }),
                source: None,
                channel_chat_id: None,
                extra: serde_json::json!({ "teamId": team_id }),
            };
            let conv = self
                .conversation_service
                .create(user_id, conv_req)
                .await
                .map_err(|e| {
                    TeamError::InvalidRequest(format!("failed to create conversation: {e}"))
                })?;

            agents.push(TeamAgent {
                slot_id,
                name: input.name.clone(),
                role,
                conversation_id: conv.id,
                backend: input.backend.clone(),
                model: input.model.clone(),
                custom_agent_id: input.custom_agent_id.clone(),
                status: None,
                conversation_type: None,
                cli_path: None,
            });
        }

        let lead_agent_id = agents.first().map(|a| a.slot_id.clone());
        let agents_json = serde_json::to_string(&agents)?;

        let row = TeamRow {
            id: team_id.clone(),
            user_id: user_id.to_owned(),
            name: req.name.clone(),
            workspace: String::new(),
            workspace_mode: "shared".into(),
            agents: agents_json,
            lead_agent_id: lead_agent_id.clone(),
            session_mode: None,
            created_at: now,
            updated_at: now,
        };
        self.repo.create_team(&row).await?;

        let team = Team {
            id: team_id,
            name: req.name,
            agents,
            lead_agent_id,
            created_at: now,
            updated_at: now,
        };

        info!(team_id = %team.id, "Team created");
        Ok(team.to_response())
    }

    pub async fn list_teams(&self) -> Result<Vec<TeamResponse>, TeamError> {
        let rows = self.repo.list_teams().await?;
        let mut teams = Vec::with_capacity(rows.len());
        for row in &rows {
            let team = Team::from_row(row)?;
            teams.push(team.to_response());
        }
        Ok(teams)
    }

    pub async fn get_team(&self, team_id: &str) -> Result<TeamResponse, TeamError> {
        let row = self
            .repo
            .get_team(team_id)
            .await?
            .ok_or_else(|| TeamError::TeamNotFound(team_id.into()))?;
        let team = Team::from_row(&row)?;
        Ok(team.to_response())
    }

    pub async fn remove_team(&self, user_id: &str, team_id: &str) -> Result<(), TeamError> {
        let row = self
            .repo
            .get_team(team_id)
            .await?
            .ok_or_else(|| TeamError::TeamNotFound(team_id.into()))?;
        let team = Team::from_row(&row)?;

        self.stop_session(team_id);

        for agent in &team.agents {
            let _ = self
                .conversation_service
                .delete(user_id, &agent.conversation_id)
                .await;
        }

        self.repo.delete_mailbox_by_team(team_id).await?;
        self.repo.delete_tasks_by_team(team_id).await?;
        self.repo.delete_team(team_id).await?;

        info!(team_id = %team_id, "Team removed");
        Ok(())
    }

    pub async fn rename_team(&self, team_id: &str, name: &str) -> Result<(), TeamError> {
        self.repo
            .get_team(team_id)
            .await?
            .ok_or_else(|| TeamError::TeamNotFound(team_id.into()))?;

        self.repo
            .update_team(
                team_id,
                &UpdateTeamParams {
                    name: Some(name.to_owned()),
                    agents: None,
                    lead_agent_id: None,
                },
            )
            .await?;
        Ok(())
    }

    pub async fn add_agent(
        &self,
        user_id: &str,
        team_id: &str,
        req: AddAgentRequest,
    ) -> Result<TeamAgentResponse, TeamError> {
        let row = self
            .repo
            .get_team(team_id)
            .await?
            .ok_or_else(|| TeamError::TeamNotFound(team_id.into()))?;
        let mut team = Team::from_row(&row)?;

        let slot_id = generate_id();
        let role = TeammateRole::parse(&req.role).unwrap_or(TeammateRole::Teammate);
        let agent_type = parse_agent_type(&req.backend)?;

        let conv_req = CreateConversationRequest {
            r#type: agent_type,
            name: Some(req.name.clone()),
            model: Some(ProviderWithModel {
                provider_id: req.backend.clone(),
                model: req.model.clone(),
                use_model: None,
            }),
            source: None,
            channel_chat_id: None,
            extra: serde_json::json!({ "teamId": team_id }),
        };
        let conv = self
            .conversation_service
            .create(user_id, conv_req)
            .await
            .map_err(|e| {
                TeamError::InvalidRequest(format!("failed to create conversation: {e}"))
            })?;

        let agent = TeamAgent {
            slot_id,
            name: req.name,
            role,
            conversation_id: conv.id,
            backend: req.backend,
            model: req.model,
            custom_agent_id: req.custom_agent_id,
            status: None,
            conversation_type: None,
            cli_path: None,
        };

        team.agents.push(agent.clone());
        let agents_json = serde_json::to_string(&team.agents)?;
        self.repo
            .update_team(
                team_id,
                &UpdateTeamParams {
                    name: None,
                    agents: Some(agents_json),
                    lead_agent_id: None,
                },
            )
            .await?;

        if let Some(session) = self.sessions.get(team_id) {
            session.add_agent(&agent).await;
        }

        let response = agent.to_response();
        Ok(response)
    }

    pub async fn remove_agent(
        &self,
        user_id: &str,
        team_id: &str,
        slot_id: &str,
    ) -> Result<(), TeamError> {
        let row = self
            .repo
            .get_team(team_id)
            .await?
            .ok_or_else(|| TeamError::TeamNotFound(team_id.into()))?;
        let mut team = Team::from_row(&row)?;

        let idx = team
            .agents
            .iter()
            .position(|a| a.slot_id == slot_id)
            .ok_or_else(|| TeamError::AgentNotFound(slot_id.into()))?;

        let removed = team.agents.remove(idx);

        let _ = self
            .conversation_service
            .delete(user_id, &removed.conversation_id)
            .await;

        let agents_json = serde_json::to_string(&team.agents)?;
        self.repo
            .update_team(
                team_id,
                &UpdateTeamParams {
                    name: None,
                    agents: Some(agents_json),
                    lead_agent_id: None,
                },
            )
            .await?;

        if let Some(session) = self.sessions.get(team_id) {
            let _ = session.remove_agent(slot_id).await;
        }

        Ok(())
    }

    pub async fn rename_agent(
        &self,
        team_id: &str,
        slot_id: &str,
        name: &str,
    ) -> Result<(), TeamError> {
        let row = self
            .repo
            .get_team(team_id)
            .await?
            .ok_or_else(|| TeamError::TeamNotFound(team_id.into()))?;
        let mut team = Team::from_row(&row)?;

        let agent = team
            .agents
            .iter_mut()
            .find(|a| a.slot_id == slot_id)
            .ok_or_else(|| TeamError::AgentNotFound(slot_id.into()))?;
        agent.name = name.to_owned();

        let agents_json = serde_json::to_string(&team.agents)?;
        self.repo
            .update_team(
                team_id,
                &UpdateTeamParams {
                    name: None,
                    agents: Some(agents_json),
                    lead_agent_id: None,
                },
            )
            .await?;

        if let Some(session) = self.sessions.get(team_id) {
            let _ = session.rename_agent(slot_id, name).await;
        }

        Ok(())
    }

    pub async fn ensure_session(&self, team_id: &str) -> Result<(), TeamError> {
        if self.sessions.contains_key(team_id) {
            return Ok(());
        }

        let row = self
            .repo
            .get_team(team_id)
            .await?
            .ok_or_else(|| TeamError::TeamNotFound(team_id.into()))?;
        let team = Team::from_row(&row)?;

        let session = TeamSession::start(
            team,
            self.repo.clone(),
            self.broadcaster.clone(),
            self.backend_binary_path.clone(),
        )
        .await?;

        self.sessions.insert(team_id.to_owned(), session);
        Ok(())
    }

    pub fn stop_session(&self, team_id: &str) {
        if let Some((_, session)) = self.sessions.remove(team_id) {
            session.stop();
        }
    }

    pub async fn send_message(&self, team_id: &str, content: &str) -> Result<(), TeamError> {
        let session = self
            .sessions
            .get(team_id)
            .ok_or_else(|| TeamError::SessionNotFound(team_id.into()))?;
        session.send_message(content).await
    }

    pub async fn send_message_to_agent(
        &self,
        team_id: &str,
        slot_id: &str,
        content: &str,
    ) -> Result<(), TeamError> {
        let session = self
            .sessions
            .get(team_id)
            .ok_or_else(|| TeamError::SessionNotFound(team_id.into()))?;
        session.send_message_to_agent(slot_id, content).await
    }

    pub fn dispose_all(&self) {
        let keys: Vec<String> = self
            .sessions
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        for key in keys {
            self.stop_session(&key);
        }
        info!("All team sessions disposed");
    }
}

fn parse_agent_type(backend: &str) -> Result<AgentType, TeamError> {
    let quoted = format!("\"{backend}\"");
    serde_json::from_str::<AgentType>(&quoted)
        .map_err(|_| TeamError::InvalidRequest(format!("unsupported backend: {backend}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agent_type_known_backends() {
        assert_eq!(parse_agent_type("acp").unwrap(), AgentType::Acp);
        assert_eq!(parse_agent_type("nanobot").unwrap(), AgentType::Nanobot);
        assert_eq!(parse_agent_type("remote").unwrap(), AgentType::Remote);
        assert_eq!(parse_agent_type("aionrs").unwrap(), AgentType::Aionrs);
    }

    #[test]
    fn parse_agent_type_unknown_backend_returns_error() {
        let err = parse_agent_type("unknown").unwrap_err();
        assert!(matches!(err, TeamError::InvalidRequest(_)));
    }

    #[test]
    fn parse_agent_type_openclaw_gateway() {
        assert_eq!(
            parse_agent_type("openclaw-gateway").unwrap(),
            AgentType::OpenclawGateway
        );
    }
}
