use std::path::PathBuf;
use std::sync::Arc;

use aionui_db::ITeamRepository;
use aionui_realtime::EventBroadcaster;
use tracing::info;

use crate::error::TeamError;
use crate::mailbox::Mailbox;
use crate::mcp::{TeamMcpServer, TeamMcpStdioConfig, TeamMcpStdioServerSpec};
use crate::prompts::{build_lead_prompt, build_teammate_prompt, build_wake_payload};
use crate::scheduler::TeammateManager;
use crate::task_board::TaskBoard;
use crate::types::{MailboxMessageType, Team, TeamAgent, TeammateRole, TeammateStatus};

/// Input for the wake path. Produced by [`TeamSession::compute_wake_input`],
/// consumed by D7b's `send_message` / `send_message_to_agent` (not implemented
/// in D7a). `first_message` includes the role prompt on cold starts.
#[derive(Debug, Clone)]
pub struct WakeInput {
    pub conversation_id: String,
    pub first_message: String,
    /// `false` when the mailbox is empty — caller should skip wake and
    /// leave the agent idle.
    pub should_send: bool,
}

pub struct TeamSession {
    team: Team,
    scheduler: Arc<TeammateManager>,
    mailbox: Arc<Mailbox>,
    task_board: Arc<TaskBoard>,
    mcp_server: TeamMcpServer,
    backend_binary_path: Arc<PathBuf>,
}

impl TeamSession {
    pub async fn start(
        team: Team,
        repo: Arc<dyn ITeamRepository>,
        broadcaster: Arc<dyn EventBroadcaster>,
        backend_binary_path: Arc<PathBuf>,
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
            backend_binary_path,
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
            team_id: self.team.id.clone(),
            port: self.mcp_server.port(),
            token: self.mcp_server.auth_token().to_owned(),
            slot_id: slot_id.to_owned(),
        }
    }

    /// Returns the stdio server spec that `TeamSessionService::ensure_session`
    /// (D9) persists into each agent's `conversation.extra` and that ACP
    /// `session/new` consumes via `mcp_servers`.
    pub fn stdio_spec(&self, slot_id: &str) -> TeamMcpStdioServerSpec {
        let binary_path = self.backend_binary_path.to_string_lossy();
        TeamMcpStdioServerSpec::from_config(binary_path.as_ref(), &self.mcp_stdio_config(slot_id))
    }

    /// Assemble the payload that will drive the next wake of `slot_id`.
    ///
    /// - Reads status, unread messages and tasks.
    /// - Cold-start agents (no prior status, or last status was `Error`)
    ///   receive the full role prompt prepended to the wake payload.
    /// - When the mailbox is empty, returns `WakeInput { should_send: false, .. }`
    ///   so the caller can skip the wake and mark the agent idle.
    ///
    /// Side effect: `mailbox.read_unread` marks the messages as read.
    pub async fn compute_wake_input(&self, slot_id: &str) -> Result<Option<WakeInput>, TeamError> {
        let agent = self.scheduler.get_agent(slot_id).await?;
        let unread = self.mailbox.read_unread(&self.team.id, slot_id).await?;
        let tasks = self.scheduler.list_tasks().await?;

        let wake_body = build_wake_payload(&agent, &tasks, &unread);

        // TODO(D8): swap `needs_role_prompt` to match `{Pending, Error}` once
        // `TeammateStatus::Pending` is reintroduced (it currently serde-aliases
        // into `Idle`, so we use the `TeamAgent::status` sentinel — `None` means
        // the scheduler never transitioned the slot, i.e. cold start).
        let needs_role_prompt = matches!(agent.status, None | Some(TeammateStatus::Error));

        let first_message = if needs_role_prompt {
            let role_prompt = match agent.role {
                TeammateRole::Lead => {
                    build_lead_prompt(&self.team.name, &self.scheduler.list_agents().await)
                }
                TeammateRole::Teammate => build_teammate_prompt(&agent, &self.team.name),
            };
            format!("{role_prompt}\n\n{wake_body}")
        } else {
            wake_body
        };

        let should_send = !unread.is_empty();

        Ok(Some(WakeInput {
            conversation_id: agent.conversation_id,
            first_message,
            should_send,
        }))
    }

    /// Handle agent Finish/Error events. Delegates to the scheduler's
    /// `finalize_turn` with no parsed actions (phase1 does not parse the
    /// trailing message for scheduler directives). Returns the leader slot_id
    /// that the caller should re-wake, if any; D7b wires that return value
    /// into the wake path. `is_error` is reserved for future status handling.
    pub async fn on_agent_finish(
        &self,
        conversation_id: &str,
        is_error: bool,
    ) -> Result<Option<String>, TeamError> {
        let slot_id = {
            let agents = self.scheduler.list_agents().await;
            agents
                .into_iter()
                .find(|a| a.conversation_id == conversation_id)
                .map(|a| a.slot_id)
                .ok_or_else(|| {
                    TeamError::AgentNotFound(format!(
                        "no agent with conversation_id={conversation_id}"
                    ))
                })?
        };

        if is_error {
            self.scheduler
                .set_status(&slot_id, TeammateStatus::Error)
                .await?;
        }

        self.scheduler.finalize_turn(&slot_id, &[]).await
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

    fn backend_path() -> Arc<PathBuf> {
        Arc::new(PathBuf::from("/tmp/aionui-backend-test"))
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

    async fn start_session() -> TeamSession {
        let repo: Arc<dyn ITeamRepository> = Arc::new(MockTeamRepo::new());
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(NullBroadcaster);
        TeamSession::start(make_team(), repo, broadcaster, backend_path())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn start_and_stop() {
        let session = start_session().await;
        assert_eq!(session.team_id(), "t1");
        assert!(session.mcp_server.port() > 0);
        session.stop();
    }

    #[tokio::test]
    async fn mcp_stdio_config_for_agent() {
        let session = start_session().await;
        let config = session.mcp_stdio_config("lead-1");
        assert_eq!(config.team_id, "t1");
        assert_eq!(config.slot_id, "lead-1");
        assert_eq!(config.port, session.mcp_server.port());
        session.stop();
    }

    #[tokio::test]
    async fn stdio_spec_embeds_team_and_binary_path() {
        let session = start_session().await;
        let spec = session.stdio_spec("lead-1");
        assert_eq!(spec.name, "aionui-team-t1");
        assert_eq!(spec.command, "/tmp/aionui-backend-test");
        assert_eq!(spec.args, vec!["mcp-bridge".to_string()]);
        assert!(
            spec.env
                .iter()
                .any(|(k, v)| k == "TEAM_AGENT_SLOT_ID" && v == "lead-1")
        );
        session.stop();
    }

    #[tokio::test]
    async fn send_message_writes_to_lead_mailbox() {
        let repo = Arc::new(MockTeamRepo::new());
        let repo_dyn: Arc<dyn ITeamRepository> = repo.clone();
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(NullBroadcaster);
        let session = TeamSession::start(make_team(), repo_dyn, broadcaster, backend_path())
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
        let session = TeamSession::start(make_team(), repo_dyn, broadcaster, backend_path())
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
        let session = start_session().await;
        let result = session.send_message_to_agent("nonexistent", "Hello").await;
        assert!(result.is_err());
        session.stop();
    }

    #[tokio::test]
    async fn add_and_remove_agent() {
        let session = start_session().await;

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
        let session = start_session().await;
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
        let session = start_session().await;
        let result = session.rename_agent("nonexistent", "X").await;
        assert!(result.is_err());
        session.stop();
    }

    // -- D7a new method tests ------------------------------------------------

    #[tokio::test]
    async fn compute_wake_input_cold_start_injects_lead_role_prompt() {
        let session = start_session().await;
        // Seed one unread message. `send_message` flips status to Working —
        // that is the post-send path; here we want to exercise cold-start
        // detection, so write directly to the mailbox instead.
        session
            .mailbox
            .write(
                "t1",
                "lead-1",
                "user",
                MailboxMessageType::Message,
                "kick off",
                None,
            )
            .await
            .unwrap();

        let input = session
            .compute_wake_input("lead-1")
            .await
            .unwrap()
            .expect("WakeInput");

        assert_eq!(input.conversation_id, "c1");
        assert!(input.should_send);
        assert!(
            input.first_message.contains("Lead Agent of team"),
            "expected lead role prompt, got: {}",
            input.first_message
        );
        assert!(input.first_message.contains("kick off"));
        session.stop();
    }

    #[tokio::test]
    async fn compute_wake_input_cold_start_injects_teammate_role_prompt() {
        let session = start_session().await;
        session
            .mailbox
            .write(
                "t1",
                "worker-1",
                "user",
                MailboxMessageType::Message,
                "do X",
                None,
            )
            .await
            .unwrap();

        let input = session
            .compute_wake_input("worker-1")
            .await
            .unwrap()
            .expect("WakeInput");

        assert!(
            input.first_message.contains("Teammate Agent"),
            "expected teammate role prompt, got: {}",
            input.first_message
        );
        assert!(input.first_message.contains("do X"));
        session.stop();
    }

    #[tokio::test]
    async fn compute_wake_input_warm_agent_skips_role_prompt() {
        let session = start_session().await;
        // Exit cold-start by setting a status once; any non-Error status
        // means the scheduler has seen this agent before.
        session
            .scheduler
            .set_status("lead-1", TeammateStatus::Idle)
            .await
            .unwrap();
        session
            .mailbox
            .write(
                "t1",
                "lead-1",
                "user",
                MailboxMessageType::Message,
                "follow-up",
                None,
            )
            .await
            .unwrap();

        let input = session
            .compute_wake_input("lead-1")
            .await
            .unwrap()
            .expect("WakeInput");

        assert!(input.should_send);
        assert!(
            !input.first_message.contains("Lead Agent of team"),
            "should not re-inject role prompt, got: {}",
            input.first_message
        );
        assert!(input.first_message.contains("follow-up"));
        session.stop();
    }

    #[tokio::test]
    async fn compute_wake_input_empty_mailbox_should_not_send() {
        let session = start_session().await;

        let input = session
            .compute_wake_input("lead-1")
            .await
            .unwrap()
            .expect("WakeInput");

        assert!(!input.should_send);
        session.stop();
    }

    #[tokio::test]
    async fn on_agent_finish_marks_idle_and_returns_lead_when_all_settled() {
        let session = start_session().await;

        // Worker is Working; on finish → mark idle → since the lead is the
        // only remaining non-idle member (actually also idle), all-idle
        // check returns the lead slot_id.
        session
            .scheduler
            .set_status("worker-1", TeammateStatus::Working)
            .await
            .unwrap();

        let result = session.on_agent_finish("c2", false).await.unwrap();
        assert_eq!(result.as_deref(), Some("lead-1"));

        let status = session.scheduler.get_status("worker-1").await.unwrap();
        assert_eq!(status, TeammateStatus::Idle);
        session.stop();
    }

    #[tokio::test]
    async fn on_agent_finish_lead_returns_none() {
        let session = start_session().await;
        session
            .scheduler
            .set_status("lead-1", TeammateStatus::Working)
            .await
            .unwrap();

        let result = session.on_agent_finish("c1", false).await.unwrap();
        assert!(result.is_none());
        session.stop();
    }

    #[tokio::test]
    async fn on_agent_finish_unknown_conversation_returns_error() {
        let session = start_session().await;
        let result = session.on_agent_finish("nope", false).await;
        assert!(result.is_err());
        session.stop();
    }
}
