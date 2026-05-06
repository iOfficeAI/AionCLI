use std::collections::HashMap;

use agent_client_protocol::schema::{
    AgentCapabilities, AuthMethod, AvailableCommand, SessionConfigKind, SessionConfigOption, SessionModeState,
    SessionModelState, UsageUpdate,
};

use super::events::AcpSessionEvent;
use super::reconcile::ReconcileAction;
use crate::shared_kernel::{ConfigKey, ConfigValue, ModeId, ModelId, SessionId};

/// Decoded per-session runtime state loaded from `acp_session.session_config.runtime`.
///
/// Only carries the user's last *choices* — the enumerations of what
/// the agent supports (mode list, model list, config schema) come from
/// the CLI's session response after initialization.
#[derive(Debug, Clone, Default)]
pub struct PersistedSessionState {
    pub current_mode_id: Option<String>,
    pub current_model_id: Option<String>,
    pub config_selections: HashMap<String, String>,
    pub context_usage: Option<UsageUpdate>,
}

/// What the user wants the session to be (intent).
#[derive(Debug, Clone, Default)]
struct Desired {
    mode_id: Option<String>,
    config_selections: HashMap<String, String>,
}

/// What the CLI last reported (ground truth from the backend).
#[derive(Debug, Clone, Default)]
struct Observed {
    mode_id: Option<String>,
    model_id: Option<String>,
    config_current: HashMap<String, String>,
}

/// What the CLI advertises as available options.
#[derive(Debug, Clone, Default)]
struct Advertised {
    modes: Option<SessionModeState>,
    models: Option<SessionModelState>,
    config_options: Option<Vec<SessionConfigOption>>,
    context_usage: Option<UsageUpdate>,
    agent_capabilities: Option<AgentCapabilities>,
    auth_methods: Option<Vec<AuthMethod>>,
    available_commands: Option<Vec<AvailableCommand>>,
}

/// Aggregate root for a single ACP session's lifecycle and state.
///
/// Encapsulates the three-layer state model (desired / observed / advertised)
/// and protects invariants:
/// - `session_id` is assigned at most once per lifecycle
/// - `desired.mode_id` must be in `advertised.modes` (when modes are known)
/// - `plan_reconcile` is a pure function: no side effects, fully testable
///
/// All mutations happen through aggregate methods which may emit domain
/// events (collected in `pending_events` and drained by the driver).
#[derive(Debug, Clone)]
pub struct AcpSession {
    session_id: Option<String>,
    opened: bool,
    desired: Desired,
    observed: Observed,
    advertised: Advertised,
    pending_events: Vec<AcpSessionEvent>,
}

impl AcpSession {
    pub fn new(initial_mode: Option<String>, config_selections: HashMap<String, String>) -> Self {
        Self {
            session_id: None,
            opened: false,
            desired: Desired {
                mode_id: initial_mode,
                config_selections,
            },
            observed: Observed::default(),
            advertised: Advertised::default(),
            pending_events: Vec::new(),
        }
    }

    // ─── Getters ───────────────────────────────────────────────────────

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn is_opened(&self) -> bool {
        self.opened
    }

    pub fn desired_mode(&self) -> Option<&str> {
        self.desired.mode_id.as_deref()
    }

    pub fn observed_mode(&self) -> Option<&str> {
        self.observed.mode_id.as_deref()
    }

    pub fn observed_model(&self) -> Option<&str> {
        self.observed.model_id.as_deref()
    }

    pub fn config_selections(&self) -> &HashMap<String, String> {
        &self.desired.config_selections
    }

    pub fn modes(&self) -> Option<&SessionModeState> {
        self.advertised.modes.as_ref()
    }

    pub fn model_info(&self) -> Option<&SessionModelState> {
        self.advertised.models.as_ref()
    }

    pub fn config_options(&self) -> Option<&[SessionConfigOption]> {
        self.advertised.config_options.as_deref()
    }

    pub fn context_usage(&self) -> Option<&UsageUpdate> {
        self.advertised.context_usage.as_ref()
    }

    pub fn agent_capabilities(&self) -> Option<&AgentCapabilities> {
        self.advertised.agent_capabilities.as_ref()
    }

    pub fn auth_methods(&self) -> Option<&[AuthMethod]> {
        self.advertised.auth_methods.as_deref()
    }

    pub fn available_commands(&self) -> Option<&[AvailableCommand]> {
        self.advertised.available_commands.as_deref()
    }

    pub fn current_mode_id(&self) -> Option<String> {
        self.advertised.modes.as_ref().map(|m| m.current_mode_id.to_string())
    }

    // ─── Commands (mutate + emit events) ───────────────────────────────

    /// Assign (or restore) a session ID. Idempotent: re-assigning the same
    /// ID is a no-op. Assigning a *different* ID after one is already set
    /// is an invariant violation (the aggregate must be recreated).
    pub fn assign_session_id(&mut self, sid: String) {
        if let Some(existing) = &self.session_id {
            debug_assert_eq!(existing, &sid, "session_id reassignment attempted");
            return;
        }
        self.session_id = Some(sid.clone());
        self.pending_events.push(AcpSessionEvent::SessionAssigned {
            session_id: SessionId::new(sid),
        });
    }

    /// Mark the session as opened with the CLI (first turn handshake complete).
    pub fn mark_opened(&mut self) {
        if !self.opened {
            self.opened = true;
            self.pending_events.push(AcpSessionEvent::SessionOpened);
        }
    }

    /// Set the user's desired mode. Emits `DesiredModeChanged` if the
    /// value actually changed. When advertised modes are known, the mode
    /// must be in the list (otherwise the call is a no-op).
    pub fn set_desired_mode(&mut self, mode_id: String) -> bool {
        if mode_id.is_empty() {
            return false;
        }
        if !self.is_mode_valid(&mode_id) {
            return false;
        }
        if self.desired.mode_id.as_deref() == Some(&mode_id) {
            return false;
        }
        self.desired.mode_id = Some(mode_id.clone());
        self.pending_events.push(AcpSessionEvent::DesiredModeChanged {
            mode_id: ModeId::new(mode_id),
        });
        true
    }

    /// Set a user's desired config selection.
    pub fn set_desired_config(&mut self, config_id: String, value: String) {
        let changed = self.desired.config_selections.get(&config_id) != Some(&value);
        self.desired.config_selections.insert(config_id, value);
        if changed {
            let selections = self
                .desired
                .config_selections
                .iter()
                .map(|(k, v)| (ConfigKey::new(k), ConfigValue::new(v)))
                .collect();
            self.pending_events
                .push(AcpSessionEvent::DesiredConfigChanged { selections });
        }
    }

    // ─── Observations (from CLI responses/notifications) ───────────────

    pub fn apply_observed_mode(&mut self, mode_id: &str) {
        let changed = self.observed.mode_id.as_deref() != Some(mode_id);
        self.observed.mode_id = Some(mode_id.to_owned());
        if changed {
            self.pending_events.push(AcpSessionEvent::ObservedModeSynced {
                mode_id: ModeId::new(mode_id),
            });
        }
    }

    pub fn apply_observed_model(&mut self, model_id: &str) {
        let changed = self.observed.model_id.as_deref() != Some(model_id);
        self.observed.model_id = Some(model_id.to_owned());
        if changed {
            self.pending_events.push(AcpSessionEvent::ObservedModelSynced {
                model_id: ModelId::new(model_id),
            });
        }
    }

    pub fn apply_advertised_modes(&mut self, modes: SessionModeState) {
        self.observed.mode_id = Some(modes.current_mode_id.to_string());
        self.advertised.modes = Some(modes);
    }

    pub fn apply_advertised_models(&mut self, models: SessionModelState) {
        self.observed.model_id = Some(models.current_model_id.to_string());
        self.advertised.models = Some(models);
    }

    pub fn apply_advertised_config_options(&mut self, options: Vec<SessionConfigOption>) {
        for opt in &options {
            if let Some(current) = extract_config_current_value(&opt.kind) {
                self.observed.config_current.insert(opt.id.to_string(), current);
            }
        }
        self.advertised.config_options = Some(options);
    }

    pub fn apply_advertised_capabilities(&mut self, caps: AgentCapabilities) {
        self.advertised.agent_capabilities = Some(caps);
    }

    pub fn apply_advertised_auth_methods(&mut self, methods: Vec<AuthMethod>) {
        self.advertised.auth_methods = Some(methods);
    }

    pub fn apply_advertised_commands(&mut self, commands: Vec<AvailableCommand>) {
        self.advertised.available_commands = Some(commands);
    }

    pub fn apply_context_usage(&mut self, usage: UsageUpdate) {
        self.advertised.context_usage = Some(usage);
    }

    /// Update the model's current_model_id in place without replacing
    /// the available models list. Used after a successful `set_model` call.
    pub fn update_current_model(&mut self, model_id: &str) {
        if let Some(info) = &self.advertised.models {
            let updated = SessionModelState::new(model_id.to_owned(), info.available_models.clone());
            self.advertised.models = Some(updated);
        }
        self.observed.model_id = Some(model_id.to_owned());
    }

    /// Seed the aggregate with persisted user choices from DB.
    /// Called on resume paths before the CLI session/load response arrives.
    pub fn preload_persisted(&mut self, state: &PersistedSessionState) {
        if let Some(mode_id) = &state.current_mode_id {
            self.advertised.modes = Some(SessionModeState::new(mode_id.clone(), Vec::new()));
            self.observed.mode_id = Some(mode_id.clone());
        }
        if let Some(model_id) = &state.current_model_id {
            self.advertised.models = Some(SessionModelState::new(model_id.clone(), Vec::new()));
            self.observed.model_id = Some(model_id.clone());
        }
        if !state.config_selections.is_empty() {
            self.observed.config_current = state.config_selections.clone();
        }
        if let Some(usage) = &state.context_usage {
            self.advertised.context_usage = Some(usage.clone());
        }
    }

    /// Apply a partial mode update (only currentModeId changed, keep available_modes).
    pub fn apply_partial_mode_update(&mut self, current_mode_id: &str) {
        if let Some(existing) = &self.advertised.modes {
            let available = existing.available_modes.clone();
            self.advertised.modes = Some(SessionModeState::new(current_mode_id.to_owned(), available));
        } else {
            self.advertised.modes = Some(SessionModeState::new(current_mode_id.to_owned(), Vec::new()));
        }
        self.observed.mode_id = Some(current_mode_id.to_owned());
    }

    // ─── Reconcile ─────────────────────────────────────────────────────

    /// Produce a list of actions needed to align CLI state with user intent.
    /// Pure function — no side effects. The driver executes the actions.
    pub fn plan_reconcile(&self) -> Vec<ReconcileAction> {
        let mut actions = Vec::new();

        if let Some(desired_mode) = &self.desired.mode_id
            && self.observed.mode_id.as_deref() != Some(desired_mode)
        {
            actions.push(ReconcileAction::SetMode {
                mode_id: ModeId::new(desired_mode),
            });
        }

        for (config_id, desired_value) in &self.desired.config_selections {
            if self.observed.config_current.get(config_id) != Some(desired_value) {
                actions.push(ReconcileAction::SetConfigOption {
                    config_id: ConfigKey::new(config_id),
                    value: ConfigValue::new(desired_value),
                });
            }
        }

        actions
    }

    // ─── Event drain ───────────────────────────────────────────────────

    /// Consume and return all pending domain events.
    pub fn drain_events(&mut self) -> Vec<AcpSessionEvent> {
        std::mem::take(&mut self.pending_events)
    }

    // ─── Private helpers ───────────────────────────────────────────────

    fn is_mode_valid(&self, mode_id: &str) -> bool {
        match &self.advertised.modes {
            None => true,
            Some(modes) if modes.available_modes.is_empty() => true,
            Some(modes) => modes.available_modes.iter().any(|m| m.id.0.as_ref() == mode_id),
        }
    }
}

fn extract_config_current_value(kind: &SessionConfigKind) -> Option<String> {
    match kind {
        SessionConfigKind::Select(sel) => Some(sel.current_value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::{SessionConfigSelectOption, SessionMode};

    use super::*;

    fn make_session() -> AcpSession {
        AcpSession::new(Some("default".into()), HashMap::new())
    }

    #[test]
    fn assign_session_id_emits_event() {
        let mut session = make_session();
        session.assign_session_id("sess-1".into());
        assert_eq!(session.session_id(), Some("sess-1"));
        let events = session.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            AcpSessionEvent::SessionAssigned {
                session_id: "sess-1".into()
            }
        );
    }

    #[test]
    fn assign_session_id_is_idempotent() {
        let mut session = make_session();
        session.assign_session_id("sess-1".into());
        session.drain_events();
        session.assign_session_id("sess-1".into());
        assert!(session.drain_events().is_empty());
    }

    #[test]
    fn mark_opened_emits_once() {
        let mut session = make_session();
        session.mark_opened();
        session.mark_opened();
        let events = session.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], AcpSessionEvent::SessionOpened);
        assert!(session.is_opened());
    }

    #[test]
    fn set_desired_mode_emits_when_changed() {
        let mut session = make_session();
        assert!(session.set_desired_mode("plan".into()));
        assert_eq!(session.desired_mode(), Some("plan"));
        let events = session.drain_events();
        assert_eq!(
            events[0],
            AcpSessionEvent::DesiredModeChanged { mode_id: "plan".into() }
        );
    }

    #[test]
    fn set_desired_mode_rejects_empty() {
        let mut session = make_session();
        assert!(!session.set_desired_mode(String::new()));
        assert!(session.drain_events().is_empty());
    }

    #[test]
    fn set_desired_mode_no_op_when_unchanged() {
        let mut session = make_session();
        session.set_desired_mode("plan".into());
        session.drain_events();
        assert!(!session.set_desired_mode("plan".into()));
        assert!(session.drain_events().is_empty());
    }

    #[test]
    fn set_desired_mode_validates_against_advertised() {
        let mut session = make_session();
        session.apply_advertised_modes(SessionModeState::new(
            "code",
            vec![SessionMode::new("code", "Code"), SessionMode::new("plan", "Plan")],
        ));
        assert!(session.set_desired_mode("plan".into()));
        assert!(!session.set_desired_mode("nonexistent".into()));
    }

    #[test]
    fn set_desired_mode_allows_any_when_advertised_empty() {
        let mut session = make_session();
        assert!(session.set_desired_mode("anything".into()));
    }

    #[test]
    fn apply_observed_mode_does_not_change_desired() {
        let mut session = make_session();
        session.set_desired_mode("plan".into());
        session.drain_events();
        session.apply_observed_mode("code");
        assert_eq!(session.desired_mode(), Some("plan"));
        assert_eq!(session.observed_mode(), Some("code"));
    }

    #[test]
    fn plan_reconcile_detects_mode_drift() {
        let mut session = make_session();
        session.set_desired_mode("plan".into());
        session.apply_observed_mode("default");
        let actions = session.plan_reconcile();
        assert_eq!(actions, vec![ReconcileAction::SetMode { mode_id: "plan".into() }]);
    }

    #[test]
    fn plan_reconcile_empty_when_aligned() {
        let mut session = make_session();
        session.set_desired_mode("plan".into());
        session.apply_observed_mode("plan");
        assert!(session.plan_reconcile().is_empty());
    }

    #[test]
    fn plan_reconcile_detects_config_drift() {
        let mut session = AcpSession::new(None, HashMap::new());
        session.set_desired_config("reasoning".into(), "high".into());
        let actions = session.plan_reconcile();
        assert_eq!(
            actions,
            vec![ReconcileAction::SetConfigOption {
                config_id: "reasoning".into(),
                value: "high".into(),
            }]
        );
    }

    #[test]
    fn plan_reconcile_config_aligned_when_observed_matches() {
        let mut session = AcpSession::new(None, HashMap::new());
        session.set_desired_config("reasoning".into(), "high".into());

        session.apply_advertised_config_options(vec![SessionConfigOption::select(
            "reasoning",
            "Reasoning",
            "high",
            vec![
                SessionConfigSelectOption::new("low", "Low"),
                SessionConfigSelectOption::new("high", "High"),
            ],
        )]);
        assert!(session.plan_reconcile().is_empty());
    }

    #[test]
    fn drain_events_clears_buffer() {
        let mut session = make_session();
        session.assign_session_id("s1".into());
        session.mark_opened();
        assert_eq!(session.drain_events().len(), 2);
        assert!(session.drain_events().is_empty());
    }

    #[test]
    fn apply_advertised_modes_sets_observed() {
        let mut session = make_session();
        session.apply_advertised_modes(SessionModeState::new("code", vec![SessionMode::new("code", "Code")]));
        assert_eq!(session.observed_mode(), Some("code"));
        assert_eq!(session.current_mode_id().as_deref(), Some("code"));
    }

    #[test]
    fn apply_advertised_models_sets_observed() {
        let mut session = make_session();
        session.apply_advertised_models(SessionModelState::new("claude-4", Vec::new()));
        assert_eq!(session.observed_model(), Some("claude-4"));
    }
}
