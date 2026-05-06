use super::*;
use aionui_common::AgentType;

/// Known ACP vendor labels. Kept in lockstep with the `agent_metadata`
/// seed in `005_agent_metadata.sql` — a caller hitting an unknown
/// vendor should trigger a schema drift discussion, not silently fall
/// through.
const ACP_VENDOR_LABELS: &[&str] = &[
    "claude",
    "codex",
    "gemini",
    "qwen",
    "codebuddy",
    "droid",
    "goose",
    "auggie",
    "kimi",
    "opencode",
    "copilot",
    "qoder",
    "vibe",
    "cursor",
    "kiro",
    "hermes",
    "snow",
];

pub(super) fn parse_agent_type(backend: &str) -> Result<AgentType, TeamError> {
    // Any registered ACP vendor label collapses to `AgentType::Acp`.
    if ACP_VENDOR_LABELS.contains(&backend) {
        return Ok(AgentType::Acp);
    }
    // Otherwise interpret as a top-level `AgentType` (e.g. "acp",
    // "nanobot", "aionrs", "remote", "openclaw-gateway").
    let quoted = format!("\"{backend}\"");
    if let Ok(agent_type) = serde_json::from_str::<AgentType>(&quoted) {
        return Ok(agent_type);
    }
    Err(TeamError::InvalidRequest(format!("unsupported backend: {backend}")))
}

impl TeamSessionService {
    pub async fn spawn_agent_in_session(
        &self,
        team_id: &str,
        caller_slot_id: &str,
        req: crate::session::SpawnAgentRequest,
    ) -> Result<TeamAgent, TeamError> {
        let entry = self
            .sessions
            .get(team_id)
            .ok_or_else(|| TeamError::SessionNotFound(team_id.into()))?;
        entry.session.spawn_agent(caller_slot_id, req).await
    }

    pub fn dispose_all(&self) {
        let keys: Vec<String> = self.sessions.iter().map(|entry| entry.key().clone()).collect();
        for key in keys {
            self.stop_session(&key);
        }
        info!("All team sessions disposed");
    }

    pub(crate) fn conversation_service_ref(&self) -> &ConversationService {
        &self.conversation_service
    }

    /// Create the conversation + persist the new agent slot for a spawn.
    ///
    /// Holds the per-team `add_agent` lock for the entirety of the
    /// read-modify-write on `teams.agents`, matching [`TeamSessionService::add_agent`]
    /// (W4-D23) so concurrent spawns cannot race and drop slots.
    ///
    /// The lock is *not* held across the process warmup step — callers
    /// (`TeamSession::spawn_agent`) wire that up separately so a slow
    /// `warmup` never stalls other spawns against the same team.
    pub(crate) async fn persist_spawned_agent(
        &self,
        team_id: &str,
        user_id: &str,
        name: String,
        backend: String,
        model: String,
        custom_agent_id: Option<String>,
    ) -> Result<TeamAgent, TeamError> {
        let lock = self
            .add_agent_locks
            .entry(team_id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        let row = self
            .repo
            .get_team(team_id)
            .await?
            .ok_or_else(|| TeamError::TeamNotFound(team_id.into()))?;
        let mut team = Team::from_row(&row)?;

        let agent_type = parse_agent_type(&backend)?;
        let conv_req = CreateConversationRequest {
            r#type: agent_type,
            name: Some(name.clone()),
            model: Some(ProviderWithModel {
                provider_id: backend.clone(),
                model: model.clone(),
                use_model: None,
            }),
            source: None,
            channel_chat_id: None,
            extra: serde_json::json!({
                "teamId": team_id,
                "backend": backend,
            }),
        };
        let conv = self
            .conversation_service
            .create(user_id, conv_req)
            .await
            .map_err(|error| TeamError::InvalidRequest(format!("failed to create conversation: {error}")))?;

        let agent = TeamAgent {
            slot_id: generate_id(),
            name,
            role: TeammateRole::Teammate,
            conversation_id: conv.id,
            backend,
            model,
            custom_agent_id,
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

        Ok(agent)
    }
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
