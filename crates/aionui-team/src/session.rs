use std::sync::Arc;

use aionui_db::ITeamRepository;
use aionui_realtime::EventBroadcaster;
use tracing::info;

use crate::error::TeamError;
use crate::mailbox::Mailbox;
use crate::mcp::{TeamMcpServer, TeamMcpStdioConfig};
use crate::scheduler::TeammateManager;
use crate::task_board::TaskBoard;
use crate::types::{MailboxMessageType, Team, TeamAgent, TeammateStatus};

pub struct TeamSession {
    team: Team,
    scheduler: Arc<TeammateManager>,
    mailbox: Arc<Mailbox>,
    task_board: Arc<TaskBoard>,
    mcp_server: TeamMcpServer,
}

impl TeamSession {
    pub async fn start(
        team: Team,
        repo: Arc<dyn ITeamRepository>,
        broadcaster: Arc<dyn EventBroadcaster>,
    ) -> Result<Self, TeamError> {
        let mailbox = Arc::new(Mailbox::new(repo.clone()));
        let task_board = Arc::new(TaskBoard::new(repo));

        let scheduler = Arc::new(TeammateManager::new(
            team.id.clone(),
            &team.agents,
            mailbox.clone(),
            task_board.clone(),
            broadcaster,
        ));

        let auth_token = aionui_common::generate_id();
        let mcp_server = TeamMcpServer::start(auth_token, scheduler.clone()).await?;

        info!(
            team_id = %team.id,
            port = mcp_server.port(),
            "TeamSession started"
        );

        Ok(Self {
            team,
            scheduler,
            mailbox,
            task_board,
            mcp_server,
        })
    }

    pub fn team_id(&self) -> &str {
        &self.team.id
    }

    pub fn scheduler(&self) -> &Arc<TeammateManager> {
        &self.scheduler
    }

    pub fn mcp_stdio_config(&self, slot_id: &str) -> TeamMcpStdioConfig {
        TeamMcpStdioConfig {
            port: self.mcp_server.port(),
            token: self.mcp_server.auth_token().to_owned(),
            slot_id: slot_id.to_owned(),
        }
    }

    pub async fn send_message(&self, content: &str) -> Result<(), TeamError> {
        let lead_slot_id = self
            .scheduler
            .find_lead_slot_id()
            .await
            .ok_or_else(|| TeamError::AgentNotFound("no lead agent in team".into()))?;

        self.mailbox
            .write(
                &self.team.id,
                &lead_slot_id,
                "user",
                MailboxMessageType::Message,
                content,
                None,
            )
            .await?;

        self.scheduler
            .set_status(&lead_slot_id, TeammateStatus::Working)
            .await?;

        Ok(())
    }

    pub async fn send_message_to_agent(
        &self,
        slot_id: &str,
        content: &str,
    ) -> Result<(), TeamError> {
        self.scheduler.get_agent(slot_id).await?;

        self.mailbox
            .write(
                &self.team.id,
                slot_id,
                "user",
                MailboxMessageType::Message,
                content,
                None,
            )
            .await?;

        self.scheduler
            .set_status(slot_id, TeammateStatus::Working)
            .await?;

        Ok(())
    }

    pub async fn add_agent(&self, agent: &TeamAgent) {
        self.scheduler.add_agent(agent).await;
    }

    pub async fn remove_agent(&self, slot_id: &str) -> Result<(), TeamError> {
        self.scheduler.remove_agent(slot_id).await
    }

    pub async fn rename_agent(&self, slot_id: &str, new_name: &str) -> Result<(), TeamError> {
        self.scheduler.rename_agent(slot_id, new_name).await
    }

    pub fn stop(&self) {
        info!(team_id = %self.team.id, "TeamSession stopping");
        self.mcp_server.stop();
    }

    pub fn mailbox(&self) -> &Arc<Mailbox> {
        &self.mailbox
    }

    pub fn task_board(&self) -> &Arc<TaskBoard> {
        &self.task_board
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockTeamRepo;
    use crate::types::{Team, TeamAgent, TeammateRole};
    use aionui_api_types::WebSocketMessage;
    use std::sync::Arc;

    struct NullBroadcaster;
    impl EventBroadcaster for NullBroadcaster {
        fn broadcast(&self, _msg: WebSocketMessage<serde_json::Value>) {}
    }

    fn make_team() -> Team {
        Team {
            id: "t1".into(),
            name: "Test Team".into(),
            agents: vec![
                TeamAgent {
                    slot_id: "lead-1".into(),
                    name: "Lead".into(),
                    role: TeammateRole::Lead,
                    conversation_id: "c1".into(),
                    backend: "acp".into(),
                    model: "claude".into(),
                    custom_agent_id: None,
                    status: None,
                    conversation_type: None,
                    cli_path: None,
                },
                TeamAgent {
                    slot_id: "worker-1".into(),
                    name: "Worker".into(),
                    role: TeammateRole::Teammate,
                    conversation_id: "c2".into(),
                    backend: "acp".into(),
                    model: "claude".into(),
                    custom_agent_id: None,
                    status: None,
                    conversation_type: None,
                    cli_path: None,
                },
            ],
            lead_agent_id: Some("lead-1".into()),
            created_at: 1000,
            updated_at: 1000,
        }
    }

    #[tokio::test]
    async fn start_and_stop() {
        let repo: Arc<dyn ITeamRepository> = Arc::new(MockTeamRepo::new());
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(NullBroadcaster);
        let team = make_team();

        let session = TeamSession::start(team, repo, broadcaster).await.unwrap();
        assert_eq!(session.team_id(), "t1");
        assert!(session.mcp_server.port() > 0);
        session.stop();
    }

    #[tokio::test]
    async fn mcp_stdio_config_for_agent() {
        let repo: Arc<dyn ITeamRepository> = Arc::new(MockTeamRepo::new());
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(NullBroadcaster);
        let team = make_team();

        let session = TeamSession::start(team, repo, broadcaster).await.unwrap();
        let config = session.mcp_stdio_config("lead-1");
        assert_eq!(config.slot_id, "lead-1");
        assert_eq!(config.port, session.mcp_server.port());
        session.stop();
    }

    #[tokio::test]
    async fn send_message_writes_to_lead_mailbox() {
        let repo = Arc::new(MockTeamRepo::new());
        let repo_dyn: Arc<dyn ITeamRepository> = repo.clone();
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(NullBroadcaster);
        let team = make_team();

        let session = TeamSession::start(team, repo_dyn, broadcaster)
            .await
            .unwrap();
        session.send_message("Hello team").await.unwrap();

        let state = repo.state.lock().unwrap();
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].to_agent_id, "lead-1");
        assert_eq!(state.messages[0].from_agent_id, "user");
        assert_eq!(state.messages[0].content, "Hello team");
        session.stop();
    }

    #[tokio::test]
    async fn send_message_to_agent_writes_to_mailbox() {
        let repo = Arc::new(MockTeamRepo::new());
        let repo_dyn: Arc<dyn ITeamRepository> = repo.clone();
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(NullBroadcaster);
        let team = make_team();

        let session = TeamSession::start(team, repo_dyn, broadcaster)
            .await
            .unwrap();
        session
            .send_message_to_agent("worker-1", "Do this task")
            .await
            .unwrap();

        let state = repo.state.lock().unwrap();
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].to_agent_id, "worker-1");
        assert_eq!(state.messages[0].content, "Do this task");
        session.stop();
    }

    #[tokio::test]
    async fn send_message_to_unknown_agent_returns_error() {
        let repo: Arc<dyn ITeamRepository> = Arc::new(MockTeamRepo::new());
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(NullBroadcaster);
        let team = make_team();

        let session = TeamSession::start(team, repo, broadcaster).await.unwrap();
        let result = session.send_message_to_agent("nonexistent", "Hello").await;
        assert!(result.is_err());
        session.stop();
    }

    #[tokio::test]
    async fn add_and_remove_agent() {
        let repo: Arc<dyn ITeamRepository> = Arc::new(MockTeamRepo::new());
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(NullBroadcaster);
        let team = make_team();

        let session = TeamSession::start(team, repo, broadcaster).await.unwrap();

        let new_agent = TeamAgent {
            slot_id: "new-1".into(),
            name: "NewAgent".into(),
            role: TeammateRole::Teammate,
            conversation_id: "c3".into(),
            backend: "acp".into(),
            model: "claude".into(),
            custom_agent_id: None,
            status: None,
            conversation_type: None,
            cli_path: None,
        };
        session.add_agent(&new_agent).await;

        let agents = session.scheduler.list_agents().await;
        assert_eq!(agents.len(), 3);

        session.remove_agent("new-1").await.unwrap();
        let agents = session.scheduler.list_agents().await;
        assert_eq!(agents.len(), 2);

        session.stop();
    }

    #[tokio::test]
    async fn rename_agent_in_session() {
        let repo: Arc<dyn ITeamRepository> = Arc::new(MockTeamRepo::new());
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(NullBroadcaster);
        let team = make_team();

        let session = TeamSession::start(team, repo, broadcaster).await.unwrap();
        session
            .rename_agent("worker-1", "Senior Worker")
            .await
            .unwrap();

        let agent = session.scheduler.get_agent("worker-1").await.unwrap();
        assert_eq!(agent.name, "Senior Worker");

        session.stop();
    }

    #[tokio::test]
    async fn rename_unknown_agent_returns_error() {
        let repo: Arc<dyn ITeamRepository> = Arc::new(MockTeamRepo::new());
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(NullBroadcaster);
        let team = make_team();

        let session = TeamSession::start(team, repo, broadcaster).await.unwrap();
        let result = session.rename_agent("nonexistent", "X").await;
        assert!(result.is_err());
        session.stop();
    }
}
