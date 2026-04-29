mod common;

use std::sync::Arc;
use std::sync::Mutex;

use aionui_ai_agent::{AgentFactory, BuildTaskOptions, IWorkerTaskManager, WorkerTaskManagerImpl};
use aionui_api_types::{AddAgentRequest, CreateTeamRequest, TeamAgentInput, WebSocketMessage};
use aionui_common::{AgentKillReason, AppError, PaginatedResult};
use aionui_db::models::{ConversationRow, MessageRow};
use aionui_db::{
    ConversationFilters, ConversationRowUpdate, DbError, IConversationRepository, ITeamRepository,
    MessageRowUpdate, MessageSearchRow, SortOrder,
};
use aionui_realtime::EventBroadcaster;

use aionui_conversation::ConversationService;
use aionui_team::TeamSessionService;
use common::MockTeamRepo;

// ---------------------------------------------------------------------------
// Mock ConversationRepository — minimal impl for TeamSessionService tests
// ---------------------------------------------------------------------------

struct MockConversationRepo {
    conversations: std::sync::Mutex<Vec<ConversationRow>>,
}

impl MockConversationRepo {
    fn new() -> Self {
        Self {
            conversations: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl IConversationRepository for MockConversationRepo {
    async fn get(&self, id: &str) -> Result<Option<ConversationRow>, DbError> {
        let convs = self.conversations.lock().unwrap();
        Ok(convs.iter().find(|c| c.id == id).cloned())
    }
    async fn create(&self, row: &ConversationRow) -> Result<(), DbError> {
        self.conversations.lock().unwrap().push(row.clone());
        Ok(())
    }
    async fn update(&self, id: &str, updates: &ConversationRowUpdate) -> Result<(), DbError> {
        let mut convs = self.conversations.lock().unwrap();
        let conv = convs
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| DbError::NotFound(id.to_owned()))?;
        if let Some(ref extra) = updates.extra {
            conv.extra = extra.clone();
        }
        if let Some(ref name) = updates.name {
            conv.name = name.clone();
        }
        if let Some(pinned) = updates.pinned {
            conv.pinned = pinned;
        }
        if let Some(ref model) = updates.model {
            conv.model = model.clone();
        }
        if let Some(updated_at) = updates.updated_at {
            conv.updated_at = updated_at;
        }
        Ok(())
    }
    async fn delete(&self, id: &str) -> Result<(), DbError> {
        self.conversations.lock().unwrap().retain(|c| c.id != id);
        Ok(())
    }
    async fn list_paginated(
        &self,
        _user_id: &str,
        _filters: &ConversationFilters,
    ) -> Result<PaginatedResult<ConversationRow>, DbError> {
        Ok(PaginatedResult {
            items: vec![],
            total: 0,
            has_more: false,
        })
    }
    async fn find_by_source_and_chat(
        &self,
        _user_id: &str,
        _source: &str,
        _chat_id: &str,
        _agent_type: &str,
    ) -> Result<Option<ConversationRow>, DbError> {
        Ok(None)
    }
    async fn list_by_cron_job(
        &self,
        _user_id: &str,
        _cron_job_id: &str,
    ) -> Result<Vec<ConversationRow>, DbError> {
        Ok(vec![])
    }
    async fn list_associated(
        &self,
        _user_id: &str,
        _conversation_id: &str,
    ) -> Result<Vec<ConversationRow>, DbError> {
        Ok(vec![])
    }
    async fn get_messages(
        &self,
        _conv_id: &str,
        _page: u32,
        _page_size: u32,
        _order: SortOrder,
    ) -> Result<PaginatedResult<MessageRow>, DbError> {
        Ok(PaginatedResult {
            items: vec![],
            total: 0,
            has_more: false,
        })
    }
    async fn insert_message(&self, _message: &MessageRow) -> Result<(), DbError> {
        Ok(())
    }
    async fn update_message(&self, _id: &str, _updates: &MessageRowUpdate) -> Result<(), DbError> {
        Ok(())
    }
    async fn delete_messages_by_conversation(&self, _conv_id: &str) -> Result<(), DbError> {
        Ok(())
    }
    async fn get_message_by_msg_id(
        &self,
        _conv_id: &str,
        _msg_id: &str,
        _msg_type: &str,
    ) -> Result<Option<MessageRow>, DbError> {
        Ok(None)
    }
    async fn search_messages(
        &self,
        _user_id: &str,
        _keyword: &str,
        _page: u32,
        _page_size: u32,
    ) -> Result<PaginatedResult<MessageSearchRow>, DbError> {
        Ok(PaginatedResult {
            items: vec![],
            total: 0,
            has_more: false,
        })
    }
}

// ---------------------------------------------------------------------------
// NullBroadcaster — no-op event broadcaster
// ---------------------------------------------------------------------------

struct NullBroadcaster;
impl EventBroadcaster for NullBroadcaster {
    fn broadcast(&self, _msg: WebSocketMessage<serde_json::Value>) {}
}

// ---------------------------------------------------------------------------
// Full MockTeamRepo with actual team CRUD (not stubs)
// ---------------------------------------------------------------------------

struct FullMockTeamRepo {
    inner: MockTeamRepo,
    teams: std::sync::Mutex<Vec<aionui_db::models::TeamRow>>,
}

impl FullMockTeamRepo {
    fn new() -> Self {
        Self {
            inner: MockTeamRepo::new(),
            teams: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl ITeamRepository for FullMockTeamRepo {
    async fn create_team(&self, row: &aionui_db::models::TeamRow) -> Result<(), DbError> {
        self.teams.lock().unwrap().push(row.clone());
        Ok(())
    }
    async fn list_teams(&self) -> Result<Vec<aionui_db::models::TeamRow>, DbError> {
        Ok(self.teams.lock().unwrap().clone())
    }
    async fn get_team(&self, id: &str) -> Result<Option<aionui_db::models::TeamRow>, DbError> {
        Ok(self
            .teams
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.id == id)
            .cloned())
    }
    async fn update_team(
        &self,
        id: &str,
        params: &aionui_db::UpdateTeamParams,
    ) -> Result<(), DbError> {
        let mut teams = self.teams.lock().unwrap();
        let team = teams
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| DbError::NotFound(id.to_owned()))?;
        if let Some(ref name) = params.name {
            team.name = name.clone();
        }
        if let Some(ref agents) = params.agents {
            team.agents = agents.clone();
        }
        if let Some(ref lead_id) = params.lead_agent_id {
            team.lead_agent_id = Some(lead_id.clone());
        }
        team.updated_at = aionui_common::now_ms();
        Ok(())
    }
    async fn delete_team(&self, id: &str) -> Result<(), DbError> {
        self.teams.lock().unwrap().retain(|t| t.id != id);
        Ok(())
    }

    async fn write_message(
        &self,
        row: &aionui_db::models::MailboxMessageRow,
    ) -> Result<(), DbError> {
        self.inner.write_message(row).await
    }
    async fn read_unread_and_mark(
        &self,
        team_id: &str,
        to_agent_id: &str,
    ) -> Result<Vec<aionui_db::models::MailboxMessageRow>, DbError> {
        self.inner.read_unread_and_mark(team_id, to_agent_id).await
    }
    async fn get_history(
        &self,
        team_id: &str,
        to_agent_id: &str,
        limit: Option<i64>,
    ) -> Result<Vec<aionui_db::models::MailboxMessageRow>, DbError> {
        self.inner.get_history(team_id, to_agent_id, limit).await
    }
    async fn delete_mailbox_by_team(&self, team_id: &str) -> Result<(), DbError> {
        self.inner.delete_mailbox_by_team(team_id).await
    }

    async fn create_task(&self, row: &aionui_db::models::TeamTaskRow) -> Result<(), DbError> {
        self.inner.create_task(row).await
    }
    async fn find_task_by_id(
        &self,
        team_id: &str,
        task_id: &str,
    ) -> Result<Option<aionui_db::models::TeamTaskRow>, DbError> {
        self.inner.find_task_by_id(team_id, task_id).await
    }
    async fn update_task(
        &self,
        task_id: &str,
        params: &aionui_db::UpdateTaskParams,
    ) -> Result<(), DbError> {
        self.inner.update_task(task_id, params).await
    }
    async fn list_tasks(
        &self,
        team_id: &str,
    ) -> Result<Vec<aionui_db::models::TeamTaskRow>, DbError> {
        self.inner.list_tasks(team_id).await
    }
    async fn append_to_blocks(&self, task_id: &str, blocked_task_id: &str) -> Result<(), DbError> {
        self.inner.append_to_blocks(task_id, blocked_task_id).await
    }
    async fn remove_from_blocked_by(
        &self,
        task_id: &str,
        unblocked_task_id: &str,
    ) -> Result<(), DbError> {
        self.inner
            .remove_from_blocked_by(task_id, unblocked_task_id)
            .await
    }
    async fn delete_tasks_by_team(&self, team_id: &str) -> Result<(), DbError> {
        self.inner.delete_tasks_by_team(team_id).await
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct StubSkillResolver;
#[async_trait::async_trait]
impl aionui_conversation::skill_resolver::SkillResolver for StubSkillResolver {
    async fn auto_inject_names(&self) -> Vec<String> {
        Vec::new()
    }
    async fn resolve_skills(
        &self,
        _names: &[String],
    ) -> Vec<aionui_conversation::skill_resolver::ResolvedAgentSkill> {
        Vec::new()
    }
    async fn link_workspace_skills(
        &self,
        _workspace: &std::path::Path,
        _rel_dirs: &[&str],
        _skills: &[aionui_conversation::skill_resolver::ResolvedAgentSkill],
    ) -> usize {
        0
    }
}

// ---------------------------------------------------------------------------
// Counting task manager — wraps WorkerTaskManagerImpl so tests can assert
// kill / get_or_build_task call counts by conversation id.
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct TaskManagerCalls {
    kill: Vec<(String, Option<AgentKillReason>)>,
    build: Vec<String>,
}

struct CountingTaskManager {
    inner: WorkerTaskManagerImpl,
    calls: Mutex<TaskManagerCalls>,
}

impl CountingTaskManager {
    fn new(factory: AgentFactory) -> Self {
        Self {
            inner: WorkerTaskManagerImpl::new(factory),
            calls: Mutex::new(TaskManagerCalls::default()),
        }
    }

    fn snapshot(&self) -> TaskManagerCalls {
        self.calls.lock().unwrap().clone()
    }
}

impl IWorkerTaskManager for CountingTaskManager {
    fn get_task(&self, conversation_id: &str) -> Option<aionui_ai_agent::AgentManagerHandle> {
        self.inner.get_task(conversation_id)
    }
    fn get_or_build_task(
        &self,
        conversation_id: &str,
        options: BuildTaskOptions,
    ) -> Result<aionui_ai_agent::AgentManagerHandle, AppError> {
        self.calls
            .lock()
            .unwrap()
            .build
            .push(conversation_id.to_owned());
        self.inner.get_or_build_task(conversation_id, options)
    }
    fn kill(&self, conversation_id: &str, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        self.calls
            .lock()
            .unwrap()
            .kill
            .push((conversation_id.to_owned(), reason));
        self.inner.kill(conversation_id, reason)
    }
    fn clear(&self) {
        self.inner.clear()
    }
    fn active_count(&self) -> usize {
        self.inner.active_count()
    }
    fn collect_idle(&self, idle_threshold_ms: aionui_common::TimestampMs) -> Vec<String> {
        self.inner.collect_idle(idle_threshold_ms)
    }
}

// Minimal stub agent returned by the test factory: ensure_session only
// asks the task manager to kill + rebuild; the returned handle never has
// `send_message` called on it.
mod mock_agent {
    use aionui_ai_agent::agent_manager::IAgentManager;
    use aionui_ai_agent::stream_event::AgentStreamEvent;
    use aionui_ai_agent::types::SendMessageData;
    use aionui_common::{
        AgentKillReason, AgentType, AppError, Confirmation, ConversationStatus, TimestampMs,
    };
    use tokio::sync::broadcast;

    pub struct MockAgent {
        pub conversation_id: String,
        pub workspace: String,
        pub event_tx: broadcast::Sender<AgentStreamEvent>,
    }

    impl MockAgent {
        pub fn new(conversation_id: String, workspace: String) -> Self {
            let (event_tx, _) = broadcast::channel(16);
            Self {
                conversation_id,
                workspace,
                event_tx,
            }
        }
    }

    #[async_trait::async_trait]
    impl IAgentManager for MockAgent {
        fn agent_type(&self) -> AgentType {
            AgentType::Acp
        }
        fn status(&self) -> Option<ConversationStatus> {
            None
        }
        fn workspace(&self) -> &str {
            &self.workspace
        }
        fn conversation_id(&self) -> &str {
            &self.conversation_id
        }
        fn last_activity_at(&self) -> TimestampMs {
            0
        }
        fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
            self.event_tx.subscribe()
        }
        async fn send_message(&self, _data: SendMessageData) -> Result<(), AppError> {
            Ok(())
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
            vec![]
        }
        fn check_approval(&self, _action: &str, _command_type: Option<&str>) -> bool {
            false
        }
        fn kill(&self, _reason: Option<AgentKillReason>) -> Result<(), AppError> {
            Ok(())
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
}

fn success_factory() -> AgentFactory {
    Arc::new(|opts: BuildTaskOptions| {
        Ok(Arc::new(mock_agent::MockAgent::new(
            opts.conversation_id,
            opts.workspace,
        )) as aionui_ai_agent::AgentManagerHandle)
    })
}

fn setup_with_factory(factory: AgentFactory) -> (TeamSessionService, Arc<CountingTaskManager>) {
    let team_repo: Arc<dyn ITeamRepository> = Arc::new(FullMockTeamRepo::new());
    let conv_repo: Arc<dyn IConversationRepository> = Arc::new(MockConversationRepo::new());
    let broadcaster: Arc<dyn EventBroadcaster> = Arc::new(NullBroadcaster);
    let conv_service = ConversationService::new_with_workspace_root(
        conv_repo,
        broadcaster.clone(),
        std::env::temp_dir(),
        Arc::new(StubSkillResolver),
    );
    let backend_binary_path = Arc::new(std::path::PathBuf::from("/tmp/aionui-backend-test"));
    let task_manager = Arc::new(CountingTaskManager::new(factory));
    let task_manager_dyn: Arc<dyn IWorkerTaskManager> = task_manager.clone();
    let svc = TeamSessionService::new(
        team_repo,
        conv_service,
        broadcaster,
        task_manager_dyn,
        backend_binary_path,
    );
    (svc, task_manager)
}

fn setup() -> TeamSessionService {
    setup_with_factory(success_factory()).0
}

fn two_agent_input() -> Vec<TeamAgentInput> {
    vec![
        TeamAgentInput {
            name: "Lead".into(),
            role: "lead".into(),
            backend: "acp".into(),
            model: "claude".into(),
            custom_agent_id: None,
        },
        TeamAgentInput {
            name: "Worker".into(),
            role: "teammate".into(),
            backend: "acp".into(),
            model: "claude".into(),
            custom_agent_id: None,
        },
    ]
}

// ===========================================================================
// Test: Team CRUD (TC-*, TL-*, TG-*, TD-*, TR-*)
// ===========================================================================

#[tokio::test]
async fn tc1_create_team_with_multiple_agents() {
    let svc = setup();
    let resp = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "Alpha".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    assert_eq!(resp.name, "Alpha");
    assert_eq!(resp.agents.len(), 2);
    assert_eq!(resp.agents[0].role, "lead");
    assert_eq!(resp.agents[1].role, "teammate");
    assert!(resp.lead_agent_id.is_some());
    assert_eq!(resp.lead_agent_id, Some(resp.agents[0].slot_id.clone()));
}

#[tokio::test]
async fn tc2_create_single_agent_team() {
    let svc = setup();
    let resp = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "Solo".into(),
                agents: vec![TeamAgentInput {
                    name: "Lead".into(),
                    role: "lead".into(),
                    backend: "acp".into(),
                    model: "claude".into(),
                    custom_agent_id: None,
                }],
            },
        )
        .await
        .unwrap();

    assert_eq!(resp.agents.len(), 1);
    assert_eq!(resp.agents[0].role, "lead");
}

#[tokio::test]
async fn tc4_first_agent_is_lead() {
    let svc = setup();
    let resp = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: vec![
                    TeamAgentInput {
                        name: "A".into(),
                        role: "teammate".into(),
                        backend: "acp".into(),
                        model: "claude".into(),
                        custom_agent_id: None,
                    },
                    TeamAgentInput {
                        name: "B".into(),
                        role: "teammate".into(),
                        backend: "acp".into(),
                        model: "claude".into(),
                        custom_agent_id: None,
                    },
                ],
            },
        )
        .await
        .unwrap();

    assert_eq!(resp.agents[0].role, "lead");
    assert_eq!(resp.lead_agent_id, Some(resp.agents[0].slot_id.clone()));
}

#[tokio::test]
async fn tc5_empty_agents_returns_error() {
    let svc = setup();
    let result = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "Empty".into(),
                agents: vec![],
            },
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn tc3_each_agent_has_conversation_id() {
    let svc = setup();
    let resp = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    for agent in &resp.agents {
        assert!(!agent.conversation_id.is_empty());
    }
    assert_ne!(
        resp.agents[0].conversation_id,
        resp.agents[1].conversation_id
    );
}

// -- List teams ---------------------------------------------------------------

#[tokio::test]
async fn tl1_empty_list() {
    let svc = setup();
    let list = svc.list_teams().await.unwrap();
    assert!(list.is_empty());
}

#[tokio::test]
async fn tl2_list_multiple_teams() {
    let svc = setup();
    svc.create_team(
        "user1",
        CreateTeamRequest {
            name: "A".into(),
            agents: two_agent_input(),
        },
    )
    .await
    .unwrap();
    svc.create_team(
        "user1",
        CreateTeamRequest {
            name: "B".into(),
            agents: two_agent_input(),
        },
    )
    .await
    .unwrap();

    let list = svc.list_teams().await.unwrap();
    assert_eq!(list.len(), 2);
}

// -- Get team -----------------------------------------------------------------

#[tokio::test]
async fn tg1_get_existing_team() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "Alpha".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    let got = svc.get_team(&created.id).await.unwrap();
    assert_eq!(got.id, created.id);
    assert_eq!(got.name, "Alpha");
    assert_eq!(got.agents.len(), 2);
}

#[tokio::test]
async fn tg2_get_nonexistent_returns_error() {
    let svc = setup();
    let result = svc.get_team("nonexistent").await;
    assert!(result.is_err());
}

// -- Delete team --------------------------------------------------------------

#[tokio::test]
async fn td1_delete_existing_team() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.remove_team("user1", &created.id).await.unwrap();
    let list = svc.list_teams().await.unwrap();
    assert!(list.is_empty());
}

#[tokio::test]
async fn td6_delete_nonexistent_returns_error() {
    let svc = setup();
    let result = svc.remove_team("user1", "nonexistent").await;
    assert!(result.is_err());
}

// -- Rename team --------------------------------------------------------------

#[tokio::test]
async fn tr1_rename_existing_team() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "Old".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.rename_team(&created.id, "New Name").await.unwrap();
    let got = svc.get_team(&created.id).await.unwrap();
    assert_eq!(got.name, "New Name");
}

#[tokio::test]
async fn tr4_rename_nonexistent_returns_error() {
    let svc = setup();
    let result = svc.rename_team("nonexistent", "X").await;
    assert!(result.is_err());
}

// ===========================================================================
// Test: Agent Management (AA-*, AR-*, AN-*)
// ===========================================================================

#[tokio::test]
async fn aa1_add_agent_to_team() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: vec![TeamAgentInput {
                    name: "Lead".into(),
                    role: "lead".into(),
                    backend: "acp".into(),
                    model: "claude".into(),
                    custom_agent_id: None,
                }],
            },
        )
        .await
        .unwrap();

    let agent = svc
        .add_agent(
            "user1",
            &created.id,
            AddAgentRequest {
                name: "Worker".into(),
                role: "teammate".into(),
                backend: "acp".into(),
                model: "claude".into(),
                custom_agent_id: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(agent.name, "Worker");
    assert_eq!(agent.role, "teammate");
    assert!(!agent.conversation_id.is_empty());

    let got = svc.get_team(&created.id).await.unwrap();
    assert_eq!(got.agents.len(), 2);
}

#[tokio::test]
async fn aa4_add_agent_to_nonexistent_team() {
    let svc = setup();
    let result = svc
        .add_agent(
            "user1",
            "nonexistent",
            AddAgentRequest {
                name: "X".into(),
                role: "teammate".into(),
                backend: "acp".into(),
                model: "claude".into(),
                custom_agent_id: None,
            },
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn ar1_remove_agent_from_team() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    let worker_slot = created.agents[1].slot_id.clone();
    svc.remove_agent("user1", &created.id, &worker_slot)
        .await
        .unwrap();

    let got = svc.get_team(&created.id).await.unwrap();
    assert_eq!(got.agents.len(), 1);
    assert!(got.agents.iter().all(|a| a.slot_id != worker_slot));
}

#[tokio::test]
async fn ar4_remove_nonexistent_agent() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    let result = svc.remove_agent("user1", &created.id, "nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn an1_rename_agent() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    let slot_id = created.agents[1].slot_id.clone();
    svc.rename_agent(&created.id, &slot_id, "Senior Worker")
        .await
        .unwrap();

    let got = svc.get_team(&created.id).await.unwrap();
    let agent = got.agents.iter().find(|a| a.slot_id == slot_id).unwrap();
    assert_eq!(agent.name, "Senior Worker");
}

#[tokio::test]
async fn an3_rename_nonexistent_agent() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    let result = svc.rename_agent(&created.id, "nonexistent", "X").await;
    assert!(result.is_err());
}

// ===========================================================================
// Test: Session Management (ES-*, SS-*)
// ===========================================================================

#[tokio::test]
async fn es1_ensure_session_creates_session() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.ensure_session(&created.id).await.unwrap();
}

#[tokio::test]
async fn es2_ensure_session_is_idempotent() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.ensure_session(&created.id).await.unwrap();
    svc.ensure_session(&created.id).await.unwrap();
}

#[tokio::test]
async fn es3_ensure_session_nonexistent_team() {
    let svc = setup();
    let result = svc.ensure_session("nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn ss1_stop_session() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.ensure_session(&created.id).await.unwrap();
    svc.stop_session(&created.id);
}

#[tokio::test]
async fn ss3_stop_session_without_active_is_noop() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.stop_session(&created.id);
}

// ===========================================================================
// Test: Message sending requires active session (SM-*)
// ===========================================================================

#[tokio::test]
async fn sm4_send_message_no_session_returns_error() {
    let svc = setup();
    let result = svc.send_message("nonexistent", "Hello", None).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn sm1_send_message_with_active_session() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.ensure_session(&created.id).await.unwrap();
    svc.send_message(&created.id, "Hello team", None)
        .await
        .unwrap();
}

#[tokio::test]
async fn sa_send_message_to_agent_with_active_session() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.ensure_session(&created.id).await.unwrap();
    let worker_slot = created.agents[1].slot_id.clone();
    svc.send_message_to_agent(&created.id, &worker_slot, "Do this", None)
        .await
        .unwrap();
}

#[tokio::test]
async fn sa3_send_message_to_nonexistent_agent() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.ensure_session(&created.id).await.unwrap();
    let result = svc
        .send_message_to_agent(&created.id, "nonexistent", "Hello", None)
        .await;
    assert!(result.is_err());
}

// ===========================================================================
// Test: dispose_all
// ===========================================================================

#[tokio::test]
async fn dispose_all_cleans_up_sessions() {
    let svc = setup();
    let t1 = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "A".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();
    let t2 = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "B".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.ensure_session(&t1.id).await.unwrap();
    svc.ensure_session(&t2.id).await.unwrap();

    svc.dispose_all();

    let result = svc.send_message(&t1.id, "Hello", None).await;
    assert!(result.is_err());
}

// ===========================================================================
// Test: Delete team stops active session (TD-2 + integration)
// ===========================================================================

#[tokio::test]
async fn td_delete_team_stops_session() {
    let svc = setup();
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.ensure_session(&created.id).await.unwrap();
    svc.remove_team("user1", &created.id).await.unwrap();

    let result = svc.send_message(&created.id, "Hello", None).await;
    assert!(result.is_err());
}

// ===========================================================================
// Test: D9 ensure_session kill + rebuild closed loop
// ===========================================================================

#[tokio::test]
async fn d9_ensure_session_kills_and_rebuilds_every_agent() {
    let (svc, tm) = setup_with_factory(success_factory());
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.ensure_session(&created.id).await.unwrap();

    // Two agents → kill called 2x and get_or_build_task called 2x, each with
    // the corresponding conversation_id. Order is agents-iteration order.
    let calls = tm.snapshot();
    assert_eq!(calls.kill.len(), 2, "expected 2 kill calls");
    assert_eq!(calls.build.len(), 2, "expected 2 build calls");
    for (i, agent) in created.agents.iter().enumerate() {
        assert_eq!(calls.kill[i].0, agent.conversation_id);
        assert_eq!(calls.kill[i].1, Some(AgentKillReason::TeamMcpRebuild));
        assert_eq!(calls.build[i], agent.conversation_id);
    }
}

#[tokio::test]
async fn d9_ensure_session_persists_team_mcp_stdio_config() {
    // Each agent's conversation.extra must carry a `team_mcp_stdio_config`
    // object by the time the factory is called — that is what the rebuilt
    // ACP process will read to reach the MCP server.
    let (svc, _tm) = setup_with_factory(Arc::new(|opts: BuildTaskOptions| {
        let extra_has_cfg = opts
            .extra
            .get("team_mcp_stdio_config")
            .and_then(|v| v.as_object())
            .is_some_and(|o| o.contains_key("port") && o.contains_key("slot_id"));
        assert!(
            extra_has_cfg,
            "factory called without team_mcp_stdio_config in extra: {:?}",
            opts.extra
        );
        Ok(Arc::new(mock_agent::MockAgent::new(
            opts.conversation_id,
            opts.workspace,
        )) as aionui_ai_agent::AgentManagerHandle)
    }));

    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.ensure_session(&created.id).await.unwrap();
}

#[tokio::test]
async fn d9_ensure_session_is_idempotent() {
    let (svc, tm) = setup_with_factory(success_factory());
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    svc.ensure_session(&created.id).await.unwrap();
    svc.ensure_session(&created.id).await.unwrap();

    // Second call short-circuits — no additional kill/build calls.
    let calls = tm.snapshot();
    assert_eq!(
        calls.kill.len(),
        2,
        "second ensure_session must not re-kill"
    );
    assert_eq!(
        calls.build.len(),
        2,
        "second ensure_session must not re-build"
    );
}

#[tokio::test]
async fn d9_ensure_session_rollbacks_when_build_fails() {
    // Factory always fails → ensure_session must propagate error and not
    // insert into sessions, so send_message afterwards still errors.
    let failing_factory: AgentFactory = Arc::new(|_opts: BuildTaskOptions| {
        Err(AppError::Internal("simulated build failure".into()))
    });
    let (svc, tm) = setup_with_factory(failing_factory);
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    let result = svc.ensure_session(&created.id).await;
    assert!(
        result.is_err(),
        "ensure_session should propagate build error"
    );

    // Kill ran for the first agent (before warmup failed), build ran once
    // and errored. No session inserted, so send_message errors.
    let calls = tm.snapshot();
    assert_eq!(calls.kill.len(), 1);
    assert_eq!(calls.build.len(), 1);

    let send_result = svc.send_message(&created.id, "Hello", None).await;
    assert!(
        send_result.is_err(),
        "session must not be registered after build failure"
    );
}

// ===========================================================================
// Test: D11.5 remove_team cascades kill to every agent process
// ===========================================================================

#[tokio::test]
async fn d115_remove_team_kills_every_agent_process() {
    let (svc, tm) = setup_with_factory(success_factory());
    let created = svc
        .create_team(
            "user1",
            CreateTeamRequest {
                name: "T".into(),
                agents: two_agent_input(),
            },
        )
        .await
        .unwrap();

    // Bring two agents online — after ensure_session, active_count == 2.
    svc.ensure_session(&created.id).await.unwrap();
    assert_eq!(
        tm.active_count(),
        2,
        "ensure_session must register 2 live agents"
    );

    let before_kill = tm.snapshot().kill.len();

    svc.remove_team("user1", &created.id).await.unwrap();

    // remove_team must have issued one kill per agent with reason TeamDeleted,
    // and the task manager's active_count must drop back to 0.
    let calls = tm.snapshot();
    let new_kills = &calls.kill[before_kill..];
    assert_eq!(
        new_kills.len(),
        created.agents.len(),
        "remove_team must kill every agent once"
    );
    for (i, agent) in created.agents.iter().enumerate() {
        assert_eq!(new_kills[i].0, agent.conversation_id);
        assert_eq!(new_kills[i].1, Some(AgentKillReason::TeamDeleted));
    }
    assert_eq!(
        tm.active_count(),
        0,
        "every agent worker must be torn down after remove_team"
    );
}
