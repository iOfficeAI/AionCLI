mod common;

use std::sync::Arc;

use aionui_api_types::{AddAgentRequest, CreateTeamRequest, TeamAgentInput, WebSocketMessage};
use aionui_common::PaginatedResult;
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
    async fn update(&self, _id: &str, _updates: &ConversationRowUpdate) -> Result<(), DbError> {
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

fn setup() -> TeamSessionService {
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
    TeamSessionService::new(team_repo, conv_service, broadcaster, backend_binary_path)
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
    let result = svc.send_message("nonexistent", "Hello").await;
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
    svc.send_message(&created.id, "Hello team").await.unwrap();
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
    svc.send_message_to_agent(&created.id, &worker_slot, "Do this")
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
        .send_message_to_agent(&created.id, "nonexistent", "Hello")
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

    let result = svc.send_message(&t1.id, "Hello").await;
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

    let result = svc.send_message(&created.id, "Hello").await;
    assert!(result.is_err());
}
