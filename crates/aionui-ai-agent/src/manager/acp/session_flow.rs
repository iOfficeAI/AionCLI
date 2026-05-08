use crate::capability::first_message_injector::{InjectionConfig, inject_first_message_prefix};
use crate::manager::acp::AcpAgentManager;
use crate::protocol::events::{
    AgentStreamEvent, AvailableCommandsEventData, FinishEventData, SessionAssignedEventData, StartEventData,
};
use crate::shared_kernel::SessionId as DomainSessionId;
use crate::types::SendMessageData;
use agent_client_protocol::schema::{ContentBlock, LoadSessionRequest, PromptRequest, SessionId};
use aionui_common::AppError;
use serde_json::Value;
use tracing::info;

use super::agent::sdk_to_snake_value;

impl AcpAgentManager {
    /// Create a new ACP session and send the first prompt.
    pub(super) async fn session_new_and_prompt(&self, data: &SendMessageData) -> Result<(), AppError> {
        // Emit Start event
        self.runtime
            .emit(AgentStreamEvent::Start(StartEventData { session_id: None }));

        let req = self.params.new_session_request();
        tracing::info!(
            has_team_mcp = self.params.config.team_mcp_stdio_config.is_some(),
            has_guide_mcp = self.params.config.guide_mcp_config.is_some(),
            guide_mcp_port = self.params.config.guide_mcp_config.as_ref().map(|c| c.port),
            mcp_servers_count = req.mcp_servers.len(),
            "session_new_and_prompt: sending session/new"
        );
        let session_response = self.protocol.new_session(req).await.map_err(AppError::from)?;

        let sid = session_response.session_id.to_string();

        // Populate the session aggregate from the session response
        {
            let mut session = self.session.write().await;
            if let Some(models) = session_response.models {
                session.apply_advertised_models(models);
            }
            if let Some(modes) = session_response.modes {
                session.apply_advertised_modes(modes);
            }
            if let Some(config_options) = session_response.config_options {
                session.apply_advertised_config_options(config_options);
            }
            session.set_session_id(DomainSessionId::new(sid.clone()));
            self.commit_session_changes(&mut session).await;
        }
        self.emit_snapshot_events().await;

        // Notify subscribers (e.g. session_sync consumer) so the new id is
        // persisted into `acp_session.session_id` — resume can then
        // choose `session/load` instead of a fresh `session/new`.
        self.runtime
            .emit(AgentStreamEvent::SessionAssigned(SessionAssignedEventData {
                session_id: sid.clone(),
            }));

        self.reconcile_session(&sid).await;

        let injected_content = inject_first_message_prefix(
            &data.content,
            &self.skill_manager,
            InjectionConfig {
                preset_context: self.params.preset_context.as_deref(),
                skills: &self.params.config.skills,
                native_skill_support: self.native_skill_support(),
                custom_workspace: self.params.workspace.is_custom,
            },
        )
        .await;

        // Send the prompt
        self.protocol
            .prompt(PromptRequest::new(
                SessionId::new(sid.clone()),
                vec![ContentBlock::from(injected_content)],
            ))
            .await
            .map_err(AppError::from)?;

        // Emit Finish event when prompt completes
        self.runtime
            .emit(AgentStreamEvent::Finish(FinishEventData { session_id: Some(sid) }));

        Ok(())
    }

    /// Resume an existing session and send a message.
    ///
    /// The session aggregate is seeded during `AcpAgentManager::new`
    /// from `params.session_snapshot` — on resume paths it already
    /// carries `current_mode_id` / `current_model_id` from
    /// `acp_session.session_config.runtime`. When the CLI's
    /// `session/load` response arrives, we merge it in but keep the
    /// preloaded `current_*` values because they reflect the user's
    /// last explicit choice; the CLI's own `current_*` is only used
    /// when the aggregate has nothing yet.
    pub(super) async fn session_resume_and_send(
        &self,
        data: &SendMessageData,
        session_id: Option<&str>,
    ) -> Result<(), AppError> {
        if self.uses_claude_meta_resume() {
            // Claude backend: use session/new with _meta.claudeCode.options.resume
            // instead of session/load. This matches AionUi frontend behavior and
            // ensures mcpServers are re-injected on resume.
            if let Some(sid) = session_id {
                let mut meta = serde_json::Map::new();
                let mut claude_code = serde_json::Map::new();
                let mut options = serde_json::Map::new();
                options.insert("resume".into(), Value::String(sid.to_owned()));
                claude_code.insert("options".into(), Value::Object(options));
                meta.insert("claudeCode".into(), Value::Object(claude_code));

                let req = self.params.new_session_request().meta(meta);

                info!(
                    session_id = %sid,
                    has_team_mcp = self.params.config.team_mcp_stdio_config.is_some(),
                    has_guide_mcp = self.params.config.guide_mcp_config.is_some(),
                    guide_mcp_port = self.params.config.guide_mcp_config.as_ref().map(|c| c.port),
                    mcp_servers_count = req.mcp_servers.len(),
                    "session_resume: using session/new with claudeCode.options.resume"
                );

                let session_response = self.protocol.new_session(req).await.map_err(AppError::from)?;

                let new_sid = session_response.session_id.to_string();
                {
                    let mut session = self.session.write().await;
                    if let Some(models) = session_response.models {
                        session.apply_advertised_models(models);
                    }
                    if let Some(modes) = session_response.modes {
                        session.apply_advertised_modes(modes);
                    }
                    if let Some(config_options) = session_response.config_options {
                        session.apply_advertised_config_options(config_options);
                    }
                    session.set_session_id(DomainSessionId::new(new_sid.clone()));
                    self.commit_session_changes(&mut session).await;
                }
                self.emit_snapshot_events().await;

                self.reconcile_session(&new_sid).await;

                return self.prompt_existing_session(data, Some(&new_sid)).await;
            }
        } else if self.supports_session_load()
            && let Some(sid) = session_id
        {
            // Non-Claude backends (e.g. Codex): use session/load
            let (preloaded_mode, preloaded_model) = {
                let session = self.session.read().await;
                (
                    session.modes().map(|m| m.current_mode_id.to_string()),
                    session.model_info().map(|m| m.current_model_id.to_string()),
                )
            };

            let mut load_req = LoadSessionRequest::new(SessionId::new(sid), &self.params.workspace.path);
            if !self.params.mcp_servers.is_empty() {
                load_req = load_req.mcp_servers(self.params.mcp_servers.clone());
            }
            let resp = self.protocol.load_session(load_req).await.map_err(AppError::from)?;

            let mut session = self.session.write().await;
            if let Some(mut models) = resp.models {
                if let Some(db_current) = preloaded_model {
                    models.current_model_id = db_current.into();
                }
                session.apply_advertised_models(models);
            }
            if let Some(mut modes) = resp.modes {
                if let Some(db_current) = preloaded_mode {
                    modes.current_mode_id = db_current.into();
                }
                session.apply_advertised_modes(modes);
            }
            if let Some(config_options) = resp.config_options {
                session.apply_advertised_config_options(config_options);
            }
            drop(session);
        }

        self.emit_snapshot_events().await;

        // Seed the session aggregate and reconcile.
        if let Some(sid) = session_id {
            {
                let mut session = self.session.write().await;
                session.set_session_id(DomainSessionId::new(sid));
                self.commit_session_changes(&mut session).await;
            }
            self.reconcile_session(sid).await;
        }

        self.prompt_existing_session(data, session_id).await
    }

    /// Send a prompt to an already-established session.
    pub(super) async fn prompt_existing_session(
        &self,
        data: &SendMessageData,
        session_id: Option<&str>,
    ) -> Result<(), AppError> {
        let sid = session_id.ok_or_else(|| AppError::Internal("Cannot prompt: no session ID available".into()))?;

        // Emit Start event
        self.runtime.emit(AgentStreamEvent::Start(StartEventData {
            session_id: Some(sid.to_owned()),
        }));

        self.protocol
            .prompt(PromptRequest::new(
                SessionId::new(sid),
                vec![ContentBlock::from(data.content.clone())],
            ))
            .await
            .map_err(AppError::from)?;

        // Emit Finish event
        self.runtime.emit(AgentStreamEvent::Finish(FinishEventData {
            session_id: Some(sid.to_owned()),
        }));

        Ok(())
    }

    /// Emit model/mode/config events from the session aggregate so the frontend
    /// receives the initial session state via WebSocket immediately after
    /// session creation or load.
    async fn emit_snapshot_events(&self) {
        use aionui_api_types::{ModelInfoEntry, ModelInfoPayload};

        let session = self.session.read().await;
        if let Some(models) = session.model_info() {
            let current_id = models.current_model_id.to_string();
            let available: Vec<ModelInfoEntry> = models
                .available_models
                .iter()
                .map(|am| ModelInfoEntry {
                    id: am.model_id.to_string(),
                    label: am.name.clone(),
                })
                .collect();
            let current_label = available
                .iter()
                .find(|e| e.id == current_id)
                .map(|e| e.label.clone())
                .unwrap_or_else(|| current_id.clone());
            let payload = ModelInfoPayload {
                current_model_id: Some(current_id),
                current_model_label: Some(current_label),
                available_models: available,
            };
            // ModelInfoPayload is our own struct but go through the
            // normaliser for consistency with sibling events.
            if let Some(v) = sdk_to_snake_value(&payload) {
                self.runtime.emit(AgentStreamEvent::AcpModelInfo(v));
            }
        }
        if let Some(modes) = session.modes()
            && let Some(v) = sdk_to_snake_value(&modes)
        {
            self.runtime.emit(AgentStreamEvent::AcpModeInfo(v));
        }
        if let Some(config_options) = session.config_options()
            && let Some(v) = sdk_to_snake_value(&serde_json::json!({
                "config_options": config_options,
            }))
        {
            // Wrap in `{config_options: [...]}` to match the SDK
            // `ConfigOptionUpdate` shape used by the streaming path —
            // handshake blobs and downstream consumers see a uniform
            // structure regardless of origin.
            self.runtime.emit(AgentStreamEvent::AcpConfigOption(v));
        }
        if let Some(cmds) = session.available_commands() {
            self.runtime
                .emit(AgentStreamEvent::AvailableCommands(AvailableCommandsEventData {
                    commands: cmds.to_vec(),
                }));
        }
    }
}
