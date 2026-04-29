use std::path::PathBuf;
use std::sync::Arc;

use aionui_ai_agent::{IWorkerTaskManager, SendMessageData};
use aionui_common::generate_id;
use aionui_db::ITeamRepository;
use aionui_realtime::EventBroadcaster;
use tracing::{info, warn};

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
    task_manager: Arc<dyn IWorkerTaskManager>,
}

impl TeamSession {
    pub async fn start(
        team: Team,
        repo: Arc<dyn ITeamRepository>,
        broadcaster: Arc<dyn EventBroadcaster>,
        backend_binary_path: Arc<PathBuf>,
        task_manager: Arc<dyn IWorkerTaskManager>,
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
            task_manager,
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

    /// Write a user message to the lead's mailbox and trigger a wake.
    ///
    /// Wake failures are logged but **not** propagated (D7b log-not-throw
    /// semantics — see backend-audit §3.5 #46): the mailbox row is already
    /// persisted, so surfacing an error to the HTTP caller would invite a
    /// retry that double-writes the message.
    pub async fn send_message(
        &self,
        content: &str,
        files: Option<Vec<String>>,
    ) -> Result<(), TeamError> {
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

        self.try_wake(&lead_slot_id, files).await;
        Ok(())
    }

    /// Write a user message to the specified agent's mailbox and trigger a wake.
    ///
    /// Same log-not-throw behaviour as [`send_message`]; see that method for
    /// rationale.
    pub async fn send_message_to_agent(
        &self,
        slot_id: &str,
        content: &str,
        files: Option<Vec<String>>,
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

        self.try_wake(slot_id, files).await;
        Ok(())
    }

    /// Compute the wake payload and forward it to the task manager. All
    /// error paths downgrade to `warn!` — the mailbox write has already
    /// succeeded and is the source of truth.
    async fn try_wake(&self, slot_id: &str, files: Option<Vec<String>>) {
        let input = match self.compute_wake_input(slot_id).await {
            Ok(Some(input)) => input,
            Ok(None) => {
                warn!(
                    team_id = %self.team.id,
                    slot_id,
                    "compute_wake_input returned None; skipping wake"
                );
                return;
            }
            Err(err) => {
                warn!(
                    team_id = %self.team.id,
                    slot_id,
                    error = %err,
                    "compute_wake_input failed; skipping wake (mailbox already written)"
                );
                return;
            }
        };

        if !input.should_send {
            return;
        }

        let Some(handle) = self.task_manager.get_task(&input.conversation_id) else {
            warn!(
                team_id = %self.team.id,
                slot_id,
                conversation_id = %input.conversation_id,
                "no active agent task for conversation; skipping wake (ensure_session must run first)"
            );
            return;
        };

        let data = SendMessageData {
            content: input.first_message,
            msg_id: generate_id(),
            files: files.unwrap_or_default(),
            inject_skills: Vec::new(),
        };

        if let Err(err) = handle.send_message(data).await {
            warn!(
                team_id = %self.team.id,
                slot_id,
                conversation_id = %input.conversation_id,
                error = %err,
                "agent.send_message failed; mailbox retained, wake will be retried on next trigger"
            );
        }
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
    use aionui_ai_agent::agent_manager::{AgentManagerHandle, IAgentManager, approval_key};
    use aionui_ai_agent::stream_event::AgentStreamEvent;
    use aionui_ai_agent::types::BuildTaskOptions;
    use aionui_api_types::{AgentModeResponse, WebSocketMessage};
    use aionui_common::{
        AgentKillReason, AgentType, AppError, Confirmation, ConversationStatus, TimestampMs, now_ms,
    };
    use std::any::Any;
    use std::sync::{Arc, Mutex};
    use tokio::sync::broadcast;

    struct NullBroadcaster;
    impl EventBroadcaster for NullBroadcaster {
        fn broadcast(&self, _msg: WebSocketMessage<serde_json::Value>) {}
    }

    fn backend_path() -> Arc<PathBuf> {
        Arc::new(PathBuf::from("/tmp/aionui-backend-test"))
    }

    /// Mock agent whose `send_message` pushes the received payload into a
    /// shared log, optionally failing with a configurable error.
    struct RecordingAgent {
        conversation_id: String,
        sent: Arc<Mutex<Vec<SendMessageData>>>,
        fail_with: Option<String>,
        event_tx: broadcast::Sender<AgentStreamEvent>,
    }

    impl RecordingAgent {
        fn new(
            conversation_id: &str,
            sent: Arc<Mutex<Vec<SendMessageData>>>,
            fail_with: Option<String>,
        ) -> Self {
            let (event_tx, _) = broadcast::channel(4);
            Self {
                conversation_id: conversation_id.into(),
                sent,
                fail_with,
                event_tx,
            }
        }
    }

    #[async_trait::async_trait]
    impl IAgentManager for RecordingAgent {
        fn agent_type(&self) -> AgentType {
            AgentType::Acp
        }
        fn status(&self) -> Option<ConversationStatus> {
            None
        }
        fn workspace(&self) -> &str {
            "/tmp/ws"
        }
        fn conversation_id(&self) -> &str {
            &self.conversation_id
        }
        fn last_activity_at(&self) -> TimestampMs {
            now_ms()
        }
        fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
            self.event_tx.subscribe()
        }
        async fn send_message(&self, data: SendMessageData) -> Result<(), AppError> {
            self.sent.lock().unwrap().push(data);
            match &self.fail_with {
                Some(msg) => Err(AppError::Internal(msg.clone())),
                None => Ok(()),
            }
        }
        async fn stop(&self) -> Result<(), AppError> {
            Ok(())
        }
        fn confirm(
            &self,
            _msg_id: &str,
            _call_id: &str,
            _data: serde_json::Value,
            _always_allow: bool,
        ) -> Result<(), AppError> {
            Ok(())
        }
        fn get_confirmations(&self) -> Vec<Confirmation> {
            Vec::new()
        }
        fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool {
            let _ = approval_key(Some(action), command_type);
            false
        }
        fn kill(&self, _reason: Option<AgentKillReason>) -> Result<(), AppError> {
            Ok(())
        }
        async fn get_mode(&self) -> Result<AgentModeResponse, AppError> {
            Ok(AgentModeResponse {
                mode: "default".into(),
                initialized: false,
            })
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// In-memory stub for [`IWorkerTaskManager`]. Only `get_task` is
    /// exercised by D7b; the other methods are unreachable in these tests
    /// and panic to surface drift early.
    struct StubTaskManager {
        tasks: Mutex<std::collections::HashMap<String, AgentManagerHandle>>,
    }

    impl StubTaskManager {
        fn new() -> Self {
            Self {
                tasks: Mutex::new(std::collections::HashMap::new()),
            }
        }

        fn insert(&self, conv_id: &str, handle: AgentManagerHandle) {
            self.tasks.lock().unwrap().insert(conv_id.into(), handle);
        }
    }

    impl IWorkerTaskManager for StubTaskManager {
        fn get_task(&self, conversation_id: &str) -> Option<AgentManagerHandle> {
            self.tasks.lock().unwrap().get(conversation_id).cloned()
        }
        fn get_or_build_task(
            &self,
            _conversation_id: &str,
            _options: BuildTaskOptions,
        ) -> Result<AgentManagerHandle, AppError> {
            panic!("get_or_build_task should not be called in D7b tests")
        }
        fn kill(
            &self,
            _conversation_id: &str,
            _reason: Option<AgentKillReason>,
        ) -> Result<(), AppError> {
            Ok(())
        }
        fn clear(&self) {}
        fn active_count(&self) -> usize {
            self.tasks.lock().unwrap().len()
        }
        fn collect_idle(&self, _idle_threshold_ms: TimestampMs) -> Vec<String> {
            Vec::new()
        }
    }

    /// Build a task_manager pre-populated with a [`RecordingAgent`] per
    /// conversation in `conv_ids`. `fail_with` — when set — makes
    /// `send_message` fail for every agent so tests can exercise the
    /// log-not-throw path.
    fn task_manager_with_agents(
        conv_ids: &[&str],
        fail_with: Option<String>,
    ) -> (
        Arc<dyn IWorkerTaskManager>,
        Arc<Mutex<Vec<SendMessageData>>>,
    ) {
        let sent: Arc<Mutex<Vec<SendMessageData>>> = Arc::new(Mutex::new(Vec::new()));
        let stub = StubTaskManager::new();
        for conv_id in conv_ids {
            let agent: AgentManagerHandle = Arc::new(RecordingAgent::new(
                conv_id,
                sent.clone(),
                fail_with.clone(),
            ));
            stub.insert(conv_id, agent);
        }
        (Arc::new(stub), sent)
    }

    /// Empty task_manager — `get_task` returns `None` for every conversation.
    fn empty_task_manager() -> Arc<dyn IWorkerTaskManager> {
        Arc::new(StubTaskManager::new())
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
        TeamSession::start(
            make_team(),
            repo,
            broadcaster,
            backend_path(),
            empty_task_manager(),
        )
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
        let session = TeamSession::start(
            make_team(),
            repo_dyn,
            broadcaster,
            backend_path(),
            empty_task_manager(),
        )
        .await
        .unwrap();
        session.send_message("Hello team", None).await.unwrap();

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
        let session = TeamSession::start(
            make_team(),
            repo_dyn,
            broadcaster,
            backend_path(),
            empty_task_manager(),
        )
        .await
        .unwrap();
        session
            .send_message_to_agent("worker-1", "Do this task", None)
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
        let result = session
            .send_message_to_agent("nonexistent", "Hello", None)
            .await;
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

    // -- D7b wake-path tests -------------------------------------------------

    async fn start_session_with(
        task_manager: Arc<dyn IWorkerTaskManager>,
    ) -> (TeamSession, Arc<MockTeamRepo>) {
        let repo = Arc::new(MockTeamRepo::new());
        let repo_dyn: Arc<dyn ITeamRepository> = repo.clone();
        let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(NullBroadcaster);
        let session = TeamSession::start(
            make_team(),
            repo_dyn,
            broadcaster,
            backend_path(),
            task_manager,
        )
        .await
        .unwrap();
        (session, repo)
    }

    #[tokio::test]
    async fn send_message_forwards_files_to_task_manager() {
        let (task_manager, sent) = task_manager_with_agents(&["c1"], None);
        let (session, _repo) = start_session_with(task_manager).await;

        session
            .send_message(
                "Hello",
                Some(vec!["/tmp/a.txt".into(), "/tmp/b.txt".into()]),
            )
            .await
            .unwrap();

        let log = sent.lock().unwrap();
        assert_eq!(log.len(), 1, "expected exactly one send_message call");
        assert_eq!(log[0].files, vec!["/tmp/a.txt", "/tmp/b.txt"]);
        assert!(log[0].content.contains("Hello"));
        assert!(!log[0].msg_id.is_empty());
        session.stop();
    }

    #[tokio::test]
    async fn send_message_without_active_task_does_not_error() {
        // Empty task_manager → get_task returns None → log-not-throw: the
        // mailbox write must still succeed and the call must return Ok.
        let (session, repo) = start_session_with(empty_task_manager()).await;

        session
            .send_message("queued", None)
            .await
            .expect("send_message must return Ok even when no task is active");

        let state = repo.state.lock().unwrap();
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].content, "queued");
        session.stop();
    }

    #[tokio::test]
    async fn send_message_swallows_task_manager_send_failure() {
        // Agent present but send_message fails — D7b must log and return Ok
        // (P0#46). A propagated error would invite retries that double-write
        // the mailbox.
        let (task_manager, sent) = task_manager_with_agents(&["c1"], Some("boom".into()));
        let (session, _repo) = start_session_with(task_manager).await;

        session
            .send_message("payload", None)
            .await
            .expect("wake failure must be swallowed");

        // The attempt still reached the agent, so the sent log has one entry.
        assert_eq!(sent.lock().unwrap().len(), 1);
        session.stop();
    }

    #[tokio::test]
    async fn send_message_to_agent_targets_specific_conversation() {
        let (task_manager, sent) = task_manager_with_agents(&["c1", "c2"], None);
        let (session, _repo) = start_session_with(task_manager).await;

        session
            .send_message_to_agent("worker-1", "do X", Some(vec!["/tmp/x.md".into()]))
            .await
            .unwrap();

        let log = sent.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].files, vec!["/tmp/x.md"]);
        assert!(log[0].content.contains("do X"));
        session.stop();
    }

    #[tokio::test]
    async fn send_message_with_empty_content_still_wakes() {
        // compute_wake_input returns should_send=true whenever the mailbox has
        // unread entries, regardless of content. Ensure the wake still fires
        // when a caller passes an empty string.
        let (task_manager, sent) = task_manager_with_agents(&["c1"], None);
        let (session, _repo) = start_session_with(task_manager).await;

        session.send_message("", None).await.unwrap();

        assert_eq!(sent.lock().unwrap().len(), 1);
        session.stop();
    }
}
