use crate::manager::acp::AcpAgentManager;
use crate::manager::acp::mode_normalize::agent_metadata_uses_meta_resume;
use crate::protocol::events::{
    AgentStreamEvent, AvailableCommandsEventData, FinishEventData, SessionAssignedEventData, StartEventData,
};
use crate::shared_kernel::SessionId as DomainSessionId;
use crate::types::SendMessageData;
use agent_client_protocol::schema::{ContentBlock, LoadSessionRequest, PromptRequest, SessionId};
use aionui_common::AppError;
use serde_json::Value;

use super::agent::sdk_to_snake_value;

impl AcpAgentManager {
    /// Establish a fresh ACP session (session/new) and apply desired
    /// mode/model/config via reconcile. Does NOT send a prompt and
    /// does NOT emit Start/Finish — callers wrap that around if needed.
    ///
    /// Returns the CLI-assigned session id.
    pub(super) async fn open_session_new(&self) -> Result<String, AppError> {
        let req = self.params.new_session_request();
        let session_response = self.protocol.new_session(req).await.map_err(AppError::from)?;

        let sid = session_response.session_id.to_string();

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
            // Mark that the next prompt should carry the first-prompt prelude
            // (preset_context + skill index). Consumed by SessionNewPreludeHook.
            session.mark_pending_session_new_prelude();
            self.commit_session_changes(&mut session).await;
        }
        self.emit_snapshot_events().await;

        // Notify session_sync consumer so the new id hits the DB and
        // future rebuilds can take the resume path.
        self.runtime
            .emit(AgentStreamEvent::SessionAssigned(SessionAssignedEventData {
                session_id: sid.clone(),
            }));

        self.reconcile_session(&sid).await;
        Ok(sid)
    }

    /// Resume an existing ACP session and apply desired mode/model/config.
    /// Does NOT send a prompt. Returns the (possibly rewritten) session id.
    ///
    /// - Claude-meta-resume backends: `session/new` with
    ///   `_meta.claudeCode.options.resume`. The CLI may assign a new session id,
    ///   which we persist via `SessionAssigned`.
    /// - `session/load`-capable backends (e.g. Codex): `session/load`, keep id.
    /// - Backends that support neither: seed the aggregate and hope the CLI
    ///   still recognises the id (legacy behaviour — matches pre-refactor).
    pub(super) async fn open_session_resume(&self, session_id: &str) -> Result<String, AppError> {
        if agent_metadata_uses_meta_resume(&self.params.metadata) {
            let mut meta = serde_json::Map::new();
            let mut claude_code = serde_json::Map::new();
            let mut options = serde_json::Map::new();
            options.insert("resume".into(), Value::String(session_id.to_owned()));
            claude_code.insert("options".into(), Value::Object(options));
            meta.insert("claudeCode".into(), Value::Object(claude_code));

            let req = self.params.new_session_request().meta(meta);
            let new_response = self.protocol.new_session(req).await.map_err(AppError::from)?;
            let new_sid = new_response.session_id.to_string();

            {
                let mut session = self.session.write().await;
                if let Some(models) = new_response.models {
                    session.apply_advertised_models(models);
                }
                if let Some(modes) = new_response.modes {
                    session.apply_advertised_modes(modes);
                }
                if let Some(config_options) = new_response.config_options {
                    session.apply_advertised_config_options(config_options);
                }
                session.set_session_id(DomainSessionId::new(new_sid.clone()));
                self.commit_session_changes(&mut session).await;
            }
            self.emit_snapshot_events().await;

            if new_sid != session_id {
                self.runtime
                    .emit(AgentStreamEvent::SessionAssigned(SessionAssignedEventData {
                        session_id: new_sid.clone(),
                    }));
            }

            self.reconcile_session(&new_sid).await;
            return Ok(new_sid);
        }

        if self.supports_session_load() {
            let (preloaded_mode, preloaded_model) = {
                let session = self.session.read().await;
                (
                    session.modes().map(|m| m.current_mode_id.to_string()),
                    session.model_info().map(|m| m.current_model_id.to_string()),
                )
            };

            let mut load_req = LoadSessionRequest::new(SessionId::new(session_id), &self.params.workspace.path);
            if !self.params.mcp_servers.is_empty() {
                load_req = load_req.mcp_servers(self.params.mcp_servers.clone());
            }
            let load_response = self.protocol.load_session(load_req).await.map_err(AppError::from)?;

            {
                let mut session = self.session.write().await;
                if let Some(mut models) = load_response.models {
                    if let Some(db_current) = preloaded_model {
                        models.current_model_id = db_current.into();
                    }
                    session.apply_advertised_models(models);
                }
                if let Some(mut modes) = load_response.modes {
                    if let Some(db_current) = preloaded_mode {
                        modes.current_mode_id = db_current.into();
                    }
                    session.apply_advertised_modes(modes);
                }
                if let Some(config_options) = load_response.config_options {
                    session.apply_advertised_config_options(config_options);
                }
                session.set_session_id(DomainSessionId::new(session_id.to_owned()));
                self.commit_session_changes(&mut session).await;
            }
            self.emit_snapshot_events().await;

            self.reconcile_session(session_id).await;
            return Ok(session_id.to_owned());
        }

        // Legacy path: backend advertised neither claude-meta-resume nor
        // session/load. Seed the aggregate with the stored id and let the
        // caller prompt — matches pre-refactor behaviour.
        {
            let mut session = self.session.write().await;
            session.set_session_id(DomainSessionId::new(session_id.to_owned()));
            self.commit_session_changes(&mut session).await;
        }
        self.emit_snapshot_events().await;
        self.reconcile_session(session_id).await;
        Ok(session_id.to_owned())
    }

    /// Send a prompt to an already-established session.
    pub(super) async fn prompt_existing_session(
        &self,
        data: &SendMessageData,
        session_id: Option<&str>,
    ) -> Result<(), AppError> {
        let sid = session_id.ok_or_else(|| AppError::Internal("Cannot prompt: no session ID available".into()))?;

        let content = data.content.clone();

        // Emit Start event
        self.runtime.emit(AgentStreamEvent::Start(StartEventData {
            session_id: Some(sid.to_owned()),
        }));

        self.protocol
            .prompt(PromptRequest::new(
                SessionId::new(sid),
                vec![ContentBlock::from(content)],
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

    fn supports_session_load(&self) -> bool {
        self.params
            .metadata
            .handshake
            .agent_capabilities
            .as_ref()
            .and_then(|caps: &Value| caps.get("load_session"))
            .and_then(|v: &Value| v.as_bool())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    //! Contract tests for the post-`warmup_session` session invariant.
    //!
    //! The integration-test harness in `tests/acp_agent_integration.rs`
    //! cannot drive `AcpAgentManager` through a JSON-RPC mock today (all
    //! existing ACP tests there are `#[ignore]` for the same reason), so we
    //! pin the observable contract at the aggregate-root layer instead:
    //! whatever `warmup_session` does internally, the session aggregate
    //! must end up with `is_opened() == true` and a populated
    //! `session_id()` — the same terminal state the real `open_session_new`
    //! / `open_session_resume` helpers leave behind.
    use crate::manager::acp::{AcpSession, AcpSessionEvent};
    use crate::shared_kernel::SessionId as DomainSessionId;

    fn make_session() -> AcpSession {
        AcpSession::new(None, None, Default::default())
    }

    /// Simulate the aggregate-state effect of a successful warmup that
    /// took the "open new session" path: `open_session_new` calls
    /// `set_session_id`, the outer `ensure_session_opened` then calls
    /// `mark_opened`. Post-state must satisfy both invariants so the
    /// follow-up `PUT /mode` / `PUT /model` can reconcile without
    /// re-opening.
    #[test]
    fn warmup_success_marks_session_opened_with_sid() {
        let mut session = make_session();
        assert!(!session.is_opened(), "precondition: session starts unopened");
        assert!(session.session_id().is_none(), "precondition: no sid yet");

        // open_session_new assigns the CLI-issued sid
        session.set_session_id(DomainSessionId::new("sess-warm-1"));
        // ensure_session_opened marks opened after the protocol call returns
        session.mark_opened();

        assert!(session.is_opened(), "warmup must leave session opened");
        assert_eq!(
            session.session_id(),
            Some("sess-warm-1"),
            "warmup must leave session id populated"
        );

        let events = session.drain_events();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AcpSessionEvent::SessionAssigned { .. })),
            "warmup must emit SessionAssigned for the persistence consumer"
        );
        assert!(
            events.iter().any(|e| matches!(e, AcpSessionEvent::SessionOpened)),
            "warmup must emit SessionOpened exactly once"
        );
    }

    /// When warmup encounters an already-opened session (e.g. called a
    /// second time on a warm agent), it must be a no-op — no duplicate
    /// `SessionOpened` event, sid preserved.
    #[test]
    fn warmup_on_opened_session_is_idempotent() {
        let mut session = make_session();
        session.set_session_id(DomainSessionId::new("sess-warm-2"));
        session.mark_opened();
        let _ = session.drain_events();

        // Second warmup call path: ensure_session_opened sees
        // (Some(sid), true) → no open_session_* call, but still flips
        // mark_opened (idempotent on the aggregate side).
        session.mark_opened();

        assert!(session.is_opened());
        assert_eq!(session.session_id(), Some("sess-warm-2"));
        assert!(
            session.drain_events().is_empty(),
            "second warmup must not emit duplicate domain events"
        );
    }
}
