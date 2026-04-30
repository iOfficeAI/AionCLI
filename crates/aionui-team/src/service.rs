use std::path::PathBuf;
use std::sync::{Arc, Weak};

use aionui_ai_agent::IWorkerTaskManager;
use aionui_api_types::{
    AddAgentRequest, CreateConversationRequest, CreateTeamRequest, TeamAgentResponse, TeamMcpPhase,
    TeamMcpStatusPayload, TeamResponse, WebSocketMessage,
};
use aionui_common::{AgentKillReason, AgentType, ProviderWithModel, generate_id, now_ms};
use aionui_conversation::ConversationService;
use aionui_db::models::TeamRow;
use aionui_db::{ITeamRepository, UpdateTeamParams};
use aionui_realtime::EventBroadcaster;
use dashmap::DashMap;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::error::TeamError;
use crate::session::TeamSession;
use crate::types::{Team, TeamAgent, TeammateRole};

struct SessionEntry {
    session: TeamSession,
    /// Background tasks that forward `Finish` / `Error` stream events to
    /// `session.on_agent_finish`. Aborted in `stop_session`.
    finish_subscribers: Vec<JoinHandle<()>>,
}

pub struct TeamSessionService {
    repo: Arc<dyn ITeamRepository>,
    conversation_service: ConversationService,
    broadcaster: Arc<dyn EventBroadcaster>,
    task_manager: Arc<dyn IWorkerTaskManager>,
    backend_binary_path: Arc<PathBuf>,
    sessions: Arc<DashMap<String, SessionEntry>>,
    /// Per-team mutex serializing `add_agent` so concurrent callers cannot
    /// read-modify-write the `agents` JSON with stale state (last-writer-wins
    /// would otherwise drop entries).
    add_agent_locks: Arc<DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Back-pointer used by [`TeamSession::spawn_agent`] to reach DB-facing
    /// orchestration without threading the service through every session method.
    /// Stored as `Weak` so the session map does not create a strong cycle with
    /// the service that owns it. Set once during [`TeamSessionService::new`]
    /// via [`Arc::new_cyclic`].
    self_ref: Weak<TeamSessionService>,
}

impl TeamSessionService {
    pub fn new(
        repo: Arc<dyn ITeamRepository>,
        conversation_service: ConversationService,
        broadcaster: Arc<dyn EventBroadcaster>,
        task_manager: Arc<dyn IWorkerTaskManager>,
        backend_binary_path: Arc<PathBuf>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            repo,
            conversation_service,
            broadcaster,
            task_manager,
            backend_binary_path,
            sessions: Arc::new(DashMap::new()),
            add_agent_locks: Arc::new(DashMap::new()),
            self_ref: weak.clone(),
        })
    }

    /// Restore sessions for all existing teams. Called once at app startup
    /// so that MCP servers are available before any user sends a message.
    pub async fn restore_all_sessions(&self) {
        let teams = match self.repo.list_teams().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "failed to list teams for session restore");
                return;
            }
        };
        for team in &teams {
            if let Err(e) = self.ensure_session(&team.id).await {
                tracing::warn!(team_id = %team.id, error = %e, "failed to restore session on startup");
            }
        }
        if !teams.is_empty() {
            tracing::info!(count = teams.len(), "team sessions restored on startup");
        }
    }

    pub async fn create_team(&self, user_id: &str, req: CreateTeamRequest) -> Result<TeamResponse, TeamError> {
        if req.agents.is_empty() {
            return Err(TeamError::InvalidRequest("at least one agent is required".into()));
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
                extra: serde_json::json!({
                    "teamId": team_id,
                    "backend": input.backend,
                }),
            };
            let conv = self
                .conversation_service
                .create(user_id, conv_req)
                .await
                .map_err(|e| TeamError::InvalidRequest(format!("failed to create conversation: {e}")))?;

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

        // Auto-start session so MCP is injected immediately after team creation.
        // Failure only logs — the team is persisted and frontend can retry
        // via POST /api/teams/{id}/session if needed.
        if let Err(e) = self.ensure_session(&team.id).await {
            warn!(team_id = %team.id, error = %e, "auto ensure_session after create_team failed");
        }

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

        // D11.5: tear down every agent worker before the team's conversations
        // are deleted — otherwise the spawned ACP/CLI processes become orphans.
        // Failures here (e.g. the task was never built, or already gone) must
        // not block the delete path.
        for agent in &team.agents {
            let _ = self
                .task_manager
                .kill(&agent.conversation_id, Some(AgentKillReason::TeamDeleted));
        }

        for agent in &team.agents {
            let _ = self.conversation_service.delete(user_id, &agent.conversation_id).await;
        }

        self.repo.delete_mailbox_by_team(team_id).await?;
        self.repo.delete_tasks_by_team(team_id).await?;
        self.repo.delete_team(team_id).await?;

        // Drop the per-team add_agent lock so the DashMap entry does not leak
        // across team lifecycles (W4-D23).
        self.add_agent_locks.remove(team_id);

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
            extra: serde_json::json!({
                "teamId": team_id,
                "backend": req.backend,
            }),
        };
        let conv = self
            .conversation_service
            .create(user_id, conv_req)
            .await
            .map_err(|e| TeamError::InvalidRequest(format!("failed to create conversation: {e}")))?;

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

        if let Some(entry) = self.sessions.get(team_id) {
            entry.session.add_agent(&agent).await;
        }

        let response = agent.to_response();
        Ok(response)
    }

    pub async fn remove_agent(&self, user_id: &str, team_id: &str, slot_id: &str) -> Result<(), TeamError> {
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

        if let Some(entry) = self.sessions.get(team_id) {
            let _ = entry.session.remove_agent(slot_id).await;
        }

        Ok(())
    }

    pub async fn rename_agent(&self, team_id: &str, slot_id: &str, name: &str) -> Result<(), TeamError> {
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

        if let Some(entry) = self.sessions.get(team_id) {
            let _ = entry.session.rename_agent(slot_id, name).await;
        }

        Ok(())
    }

    /// Start the team's MCP server and rebuild every agent process so it
    /// carries a fresh `team_mcp_stdio_config` pointing at the new server.
    ///
    /// Flow (mcp.md §4.3):
    /// 1. Start `TeamSession` (opens the MCP TCP server).
    /// 2. For each agent: persist `team_mcp_stdio_config` into
    ///    `conversation.extra` → `task_manager.kill(conv_id, TeamMcpRebuild)`
    ///    → `conversation_service.warmup(...)` rebuilds the ACP process with
    ///    the new extra.
    /// 3. Subscribe to each agent's stream and forward `Finish` / `Error`
    ///    events to `session.on_agent_finish`.
    /// 4. Only insert into `sessions` after every step above succeeds — on
    ///    any failure, stop the session and leave the map untouched so a
    ///    retry can start cleanly.
    pub async fn ensure_session(&self, team_id: &str) -> Result<(), TeamError> {
        if self.sessions.contains_key(team_id) {
            return Ok(());
        }

        let row = match self.repo.get_team(team_id).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                self.broadcast_mcp_phase(team_id, "", TeamMcpPhase::LoadFailed, None, |p| {
                    p.error = Some(format!("team not found: {team_id}"));
                });
                return Err(TeamError::TeamNotFound(team_id.into()));
            }
            Err(e) => {
                self.broadcast_mcp_phase(team_id, "", TeamMcpPhase::LoadFailed, None, |p| {
                    p.error = Some(e.to_string());
                });
                return Err(e.into());
            }
        };
        let user_id = row.user_id.clone();
        let team = Team::from_row(&row)?;
        let agents_snapshot: Vec<TeamAgent> = team.agents.clone();

        let session = match TeamSession::start(
            team,
            self.repo.clone(),
            self.broadcaster.clone(),
            self.backend_binary_path.clone(),
            self.task_manager.clone(),
            user_id.clone(),
            self.self_ref.clone(),
        )
        .await
        {
            Ok(session) => session,
            Err(e) => {
                self.broadcast_mcp_phase(team_id, "", TeamMcpPhase::SessionError, None, |p| {
                    p.error = Some(e.to_string());
                });
                return Err(e);
            }
        };

        self.broadcast_mcp_phase(team_id, "", TeamMcpPhase::SessionInjecting, None, |_| {});

        if let Err(e) = self
            .rebuild_agent_processes(team_id, &session, &user_id, &agents_snapshot)
            .await
        {
            session.stop();
            return Err(e);
        }

        let finish_subscribers = self.spawn_finish_subscribers(team_id, &agents_snapshot);

        let entry = SessionEntry {
            session,
            finish_subscribers,
        };
        self.sessions.insert(team_id.to_owned(), entry);

        self.broadcast_mcp_phase(team_id, "", TeamMcpPhase::SessionReady, None, |p| {
            p.server_count = Some(agents_snapshot.len());
        });

        Ok(())
    }

    fn broadcast_mcp_phase<F>(&self, team_id: &str, slot_id: &str, phase: TeamMcpPhase, port: Option<u16>, customize: F)
    where
        F: FnOnce(&mut TeamMcpStatusPayload),
    {
        let mut payload = TeamMcpStatusPayload {
            team_id: team_id.to_owned(),
            slot_id: slot_id.to_owned(),
            phase,
            port,
            server_count: None,
            error: None,
        };
        customize(&mut payload);
        let event = WebSocketMessage::new(
            "team.mcpStatus",
            serde_json::to_value(payload).expect("serialize mcp status payload"),
        );
        self.broadcaster.broadcast(event);
    }

    async fn rebuild_agent_processes(
        &self,
        team_id: &str,
        session: &TeamSession,
        user_id: &str,
        agents: &[TeamAgent],
    ) -> Result<(), TeamError> {
        for agent in agents {
            let cfg = session.mcp_stdio_config(&agent.slot_id);
            let patch = serde_json::json!({ "team_mcp_stdio_config": cfg });

            if let Err(e) = self
                .conversation_service
                .update_extra(&agent.conversation_id, patch)
                .await
            {
                let msg = format!("failed to persist team_mcp_stdio_config for {}: {e}", agent.slot_id);
                self.broadcast_mcp_phase(team_id, &agent.slot_id, TeamMcpPhase::ConfigWriteFailed, None, |p| {
                    p.error = Some(msg.clone());
                });
                return Err(TeamError::InvalidRequest(msg));
            }

            let _ = self
                .task_manager
                .kill(&agent.conversation_id, Some(AgentKillReason::TeamMcpRebuild));

            if let Err(e) = self
                .conversation_service
                .warmup(user_id, &agent.conversation_id, &self.task_manager)
                .await
            {
                warn!(
                    team_id,
                    slot_id = %agent.slot_id,
                    conversation_id = %agent.conversation_id,
                    error = %e,
                    "warmup failed during rebuild; agent will not be wakeable until next user message"
                );
            }
        }
        Ok(())
    }

    /// Spawn one background task per agent that drains the agent's stream
    /// and forwards `Finish` / `Error` events to the session. The tasks
    /// look up the live session via `team_id` each iteration, and exit
    /// naturally when the entry is removed in `stop_session` (which also
    /// aborts them as a belt-and-braces measure).
    fn spawn_finish_subscribers(&self, team_id: &str, agents: &[TeamAgent]) -> Vec<JoinHandle<()>> {
        use aionui_ai_agent::AgentStreamEvent;

        let mut handles = Vec::with_capacity(agents.len());
        for agent in agents {
            let Some(task) = self.task_manager.get_task(&agent.conversation_id) else {
                warn!(
                    conversation_id = %agent.conversation_id,
                    "no agent task found after warmup, skipping finish subscription"
                );
                continue;
            };
            let mut rx = task.subscribe();
            let conv_id = agent.conversation_id.clone();
            let team_id = team_id.to_owned();
            let sessions = self.sessions.clone();
            let handle = tokio::spawn(async move {
                while let Ok(event) = rx.recv().await {
                    let is_error = matches!(event, AgentStreamEvent::Error(_));
                    if !is_error && !matches!(event, AgentStreamEvent::Finish(_)) {
                        continue;
                    }
                    let Some(entry) = sessions.get(&team_id) else {
                        break;
                    };
                    match entry.session.on_agent_finish(&conv_id, is_error).await {
                        Ok(Some(wake_target)) => {
                            entry.session.try_wake(&wake_target, None).await;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            warn!(conversation_id = %conv_id, error = %e, "on_agent_finish failed");
                        }
                    }
                }
            });
            handles.push(handle);
        }
        handles
    }

    pub fn stop_session(&self, team_id: &str) {
        if let Some((_, entry)) = self.sessions.remove(team_id) {
            for handle in &entry.finish_subscribers {
                handle.abort();
            }
            entry.session.stop();
        }
    }

    pub async fn send_message(
        &self,
        team_id: &str,
        content: &str,
        files: Option<Vec<String>>,
    ) -> Result<(), TeamError> {
        let entry = self
            .sessions
            .get(team_id)
            .ok_or_else(|| TeamError::SessionNotFound(team_id.into()))?;
        entry.session.send_message(content, files).await
    }

    pub async fn send_message_to_agent(
        &self,
        team_id: &str,
        slot_id: &str,
        content: &str,
        files: Option<Vec<String>>,
    ) -> Result<(), TeamError> {
        let entry = self
            .sessions
            .get(team_id)
            .ok_or_else(|| TeamError::SessionNotFound(team_id.into()))?;
        entry.session.send_message_to_agent(slot_id, content, files).await
    }

    /// Wake a specific agent in a team session (trigger it to read mailbox).
    /// Called by MCP dispatch after `team_send_message` writes to mailbox.
    pub async fn wake_agent_in_session(&self, team_id: &str, slot_id: &str) -> Result<(), TeamError> {
        let entry = self
            .sessions
            .get(team_id)
            .ok_or_else(|| TeamError::SessionNotFound(team_id.into()))?;
        entry
            .session
            .scheduler()
            .set_status(slot_id, crate::types::TeammateStatus::Working)
            .await?;
        let input = entry.session.compute_wake_input(slot_id).await;

        if let Ok(Some(ref i)) = input
            && i.should_send
        {
            entry.session.mirror_unread_to_conversation(i).await;
        }

        let user_id = entry.session.user_id().to_owned();
        drop(entry);

        let conv_id = match &input {
            Ok(Some(i)) if i.should_send => i.conversation_id.clone(),
            _ => return Ok(()),
        };

        // Ensure the agent task exists (mirrors AionUi's getOrBuildTask).
        if self.task_manager.get_task(&conv_id).is_none()
            && let Err(e) = self
                .conversation_service
                .warmup(&user_id, &conv_id, &self.task_manager)
                .await
        {
            warn!(team_id, slot_id, conversation_id = %conv_id, error = %e, "warmup in wake failed");
            return Ok(());
        }

        let task_mgr = self.task_manager.clone();
        tokio::spawn(async move {
            let input = match input {
                Ok(Some(i)) if i.should_send => i,
                _ => return,
            };
            let Some(handle) = task_mgr.get_task(&input.conversation_id) else {
                return;
            };
            let data = aionui_ai_agent::SendMessageData {
                content: input.first_message,
                msg_id: aionui_common::generate_id(),
                files: Vec::new(),
                inject_skills: Vec::new(),
            };
            let _ = handle.send_message(data).await;
        });
        Ok(())
    }

    /// Route an MCP `team_spawn_agent` call into the live [`TeamSession`].
    ///
    /// Looks up the session for `team_id` (errors with [`TeamError::SessionNotFound`]
    /// when absent) and delegates to [`TeamSession::spawn_agent`]. The MCP
    /// dispatch layer holds a [`Weak<TeamSessionService>`] and calls this to
    /// avoid wiring a direct `Arc<TeamSession>` into the MCP server — the
    /// session is owned by the service's `sessions` map.
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

    /// Accessor used by [`TeamSession::spawn_agent`] to reach the conversation
    /// service without threading it through every call site.
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
            .map_err(|e| TeamError::InvalidRequest(format!("failed to create conversation: {e}")))?;

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

fn parse_agent_type(backend: &str) -> Result<AgentType, TeamError> {
    // Any registered ACP vendor label collapses to `AgentType::Acp`.
    if ACP_VENDOR_LABELS.contains(&backend) {
        return Ok(AgentType::Acp);
    }
    // Otherwise interpret as a top-level `AgentType` (e.g. "acp",
    // "nanobot", "aionrs", "remote", "openclaw-gateway").
    let quoted = format!("\"{backend}\"");
    if let Ok(t) = serde_json::from_str::<AgentType>(&quoted) {
        return Ok(t);
    }
    Err(TeamError::InvalidRequest(format!("unsupported backend: {backend}")))
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
