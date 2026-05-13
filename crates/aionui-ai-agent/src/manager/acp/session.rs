use std::collections::HashMap;

use agent_client_protocol::schema::{
    AgentCapabilities, AuthMethod, AvailableCommand, SessionConfigKind, SessionConfigOption, SessionModeState,
    SessionModelState, UsageUpdate,
};

use super::agent_event_tracker::AcpSessionEvent;
use super::agent_reconcile::ReconcileAction;
use crate::shared_kernel::{ConfigKey, ConfigValue, ModeId, ModelId, PersistedSessionState, SessionId};

/// What the user wants the session to be (intent).
#[derive(Debug, Clone, Default)]
struct Desired {
    mode_id: Option<ModeId>,
    model_id: Option<ModelId>,
    config_selections: HashMap<ConfigKey, ConfigValue>,
}

/// What the CLI last reported (ground truth from the backend).
#[derive(Debug, Clone, Default)]
struct Observed {
    mode_id: Option<ModeId>,
    model_id: Option<ModelId>,
    config_current: HashMap<ConfigKey, ConfigValue>,
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
    session_id: Option<SessionId>,
    opened: bool,
    /// Whether `open_session_new` has just completed and the next prompt
    /// should receive preset_context / skill-index injection.
    ///
    /// Lifecycle:
    /// - writer: `AcpAgentManager::open_session_new` after a successful
    ///   `session/new` handshake.
    /// - reader: `SessionNewPreludeHook` via `take_pending_session_new_prelude`.
    /// - invalidation: any `take_*` call drains it to `false`.
    ///
    /// Starts `false` so resume paths, warmup-only flows, and aborted
    /// session/new attempts all correctly observe "no prelude pending".
    pending_session_new_prelude: bool,
    desired: Desired,
    observed: Observed,
    advertised: Advertised,
    pending_events: Vec<AcpSessionEvent>,
    /// Model id the next prompt should announce to the CLI via an
    /// injected `<system-reminder>`. Written when the CLI bakes
    /// model identity into its cached system prompt (see
    /// `BehaviorPolicy::self_identity_sticky`) and `session/set_model`
    /// therefore does not refresh the LLM's self-description.
    /// Taken (drained) on the next prompt.
    pending_model_notice: Option<ModelId>,
}

impl AcpSession {
    pub fn new(
        initial_mode: Option<ModeId>,
        initial_model: Option<ModelId>,
        config_selections: HashMap<ConfigKey, ConfigValue>,
    ) -> Self {
        Self {
            session_id: None,
            opened: false,
            pending_session_new_prelude: false,
            desired: Desired {
                mode_id: initial_mode,
                model_id: initial_model,
                config_selections,
            },
            observed: Observed::default(),
            advertised: Advertised::default(),
            pending_events: Vec::new(),
            pending_model_notice: None,
        }
    }
}

// ─── Session Id and Session Opened ───────────────────────────────────────────────────────
impl AcpSession {
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_ref().map(SessionId::as_str)
    }

    pub fn session_id_vo(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    /// Assign (or restore) a session ID. Idempotent: re-assigning the same
    /// ID is a no-op. Assigning a *different* ID after one is already set
    /// is an invariant violation (the aggregate must be recreated).
    pub fn set_session_id(&mut self, sid: SessionId) {
        if let Some(existing) = &self.session_id {
            debug_assert_eq!(existing, &sid, "session_id reassignment attempted");
            return;
        }
        self.session_id = Some(sid.clone());
        self.pending_events
            .push(AcpSessionEvent::SessionAssigned { session_id: sid });
    }

    pub fn is_opened(&self) -> bool {
        self.opened
    }

    /// Mark the session as opened with the CLI (first turn handshake complete).
    pub fn mark_opened(&mut self) {
        if !self.opened {
            self.opened = true;
            self.pending_events.push(AcpSessionEvent::SessionOpened);
        }
    }

    /// Set the flag signalling that the next prompt carries the first
    /// post-`session/new` payload. Idempotent.
    pub fn mark_pending_session_new_prelude(&mut self) {
        self.pending_session_new_prelude = true;
    }

    /// Consume the prelude flag. Returns `true` exactly once after
    /// `mark_pending_session_new_prelude`; subsequent calls return `false`.
    pub fn take_pending_session_new_prelude(&mut self) -> bool {
        std::mem::replace(&mut self.pending_session_new_prelude, false)
    }
}

// ─── Getters Setters desired ───────────────────────────────────────────────────────
impl AcpSession {
    pub fn desired_mode(&self) -> Option<&str> {
        self.desired.mode_id.as_ref().map(ModeId::as_str)
    }

    pub fn desired_mode_id(&self) -> Option<&ModeId> {
        self.desired.mode_id.as_ref()
    }

    pub fn desired_model(&self) -> Option<&str> {
        self.desired.model_id.as_ref().map(ModelId::as_str)
    }

    pub fn desired_model_id(&self) -> Option<&ModelId> {
        self.desired.model_id.as_ref()
    }

    pub fn desired_config_selections(&self) -> &HashMap<ConfigKey, ConfigValue> {
        &self.desired.config_selections
    }

    /// Set the user's desired mode. Emits `DesiredModeChanged` if the
    /// value actually changed. When advertised modes are known, the mode
    /// must be in the list (otherwise the call is a no-op).
    pub fn set_desired_mode(&mut self, mode: ModeId) -> bool {
        if mode.as_str().is_empty() {
            return false;
        }
        if !self.is_mode_valid(mode.as_str()) {
            return false;
        }
        if self.desired.mode_id.as_ref() == Some(&mode) {
            return false;
        }
        self.desired.mode_id = Some(mode.clone());
        self.pending_events.push(AcpSessionEvent::DesiredModeChanged { mode });
        true
    }

    /// Set the user's desired model. Emits `DesiredModelChanged` if the
    /// value actually changed. When advertised models are known, the model
    /// must be in the list (otherwise the call is a no-op).
    pub fn set_desired_model(&mut self, model: ModelId) -> bool {
        if model.as_str().is_empty() {
            return false;
        }
        if !self.is_model_valid(model.as_str()) {
            return false;
        }
        if self.desired.model_id.as_ref() == Some(&model) {
            return false;
        }
        self.desired.model_id = Some(model.clone());
        self.pending_events.push(AcpSessionEvent::DesiredModelChanged { model });
        true
    }

    /// Set a user's desired config selection.
    pub fn set_desired_config(&mut self, key: ConfigKey, value: ConfigValue) {
        let changed = self.desired.config_selections.get(&key) != Some(&value);
        self.desired.config_selections.insert(key, value);
        if changed {
            let selections = self.desired.config_selections.clone();
            self.pending_events
                .push(AcpSessionEvent::DesiredConfigChanged { selections });
        }
    }
}

// ─── Getters observed ───────────────────────────────────────────────────────
impl AcpSession {
    pub fn observed_mode(&self) -> Option<&str> {
        self.observed.mode_id.as_ref().map(ModeId::as_str)
    }

    pub fn observed_mode_id(&self) -> Option<&ModeId> {
        self.observed.mode_id.as_ref()
    }

    pub fn observed_model(&self) -> Option<&str> {
        self.observed.model_id.as_ref().map(ModelId::as_str)
    }

    pub fn observed_model_id(&self) -> Option<&ModelId> {
        self.observed.model_id.as_ref()
    }
}

// ─── Getters advertised ───────────────────────────────────────────────────────
impl AcpSession {
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

    pub fn current_model_id(&self) -> Option<String> {
        self.advertised.models.as_ref().map(|m| m.current_model_id.to_string())
    }
}

// ─── Observations (from CLI responses/notifications) ───────────────
impl AcpSession {
    /// Record the CLI's current mode. Updates both `observed.mode_id` and
    /// the `advertised.modes.current_mode_id` (available_modes preserved);
    /// emits `ObservedModeSynced` when the value actually changed.
    pub fn apply_observed_mode(&mut self, mode: ModeId) {
        let changed = self.observed.mode_id.as_ref() != Some(&mode);
        self.observed.mode_id = Some(mode.clone());
        let available = self
            .advertised
            .modes
            .as_ref()
            .map(|m| m.available_modes.clone())
            .unwrap_or_default();
        self.advertised.modes = Some(SessionModeState::new(mode.as_str().to_owned(), available));
        if changed {
            self.pending_events.push(AcpSessionEvent::ObservedModeSynced { mode });
        }
    }

    /// Record the CLI's current model. Updates both `observed.model_id` and
    /// the `advertised.models.current_model_id` (available_models preserved);
    /// emits `ObservedModelSynced` when the value actually changed.
    pub fn apply_observed_model(&mut self, model: ModelId) {
        let changed = self.observed.model_id.as_ref() != Some(&model);
        self.observed.model_id = Some(model.clone());
        let available = self
            .advertised
            .models
            .as_ref()
            .map(|m| m.available_models.clone())
            .unwrap_or_default();
        self.advertised.models = Some(SessionModelState::new(model.as_str().to_owned(), available));
        if changed {
            self.pending_events.push(AcpSessionEvent::ObservedModelSynced { model });
        }
    }

    /// Record the CLI's current value for a single config option. Mirrors
    /// `apply_observed_mode/model`: diff-driven, emits `ObservedConfigSynced`
    /// with the full selection map when the value actually changed. Used by
    /// the reconcile loop after a successful `set_config_option` so
    /// `plan_reconcile` treats the drift as resolved.
    pub fn apply_observed_config(&mut self, key: ConfigKey, value: ConfigValue) {
        let changed = self.observed.config_current.get(&key) != Some(&value);
        self.observed.config_current.insert(key, value);
        if changed {
            let selections = self.observed.config_current.clone();
            self.pending_events
                .push(AcpSessionEvent::ObservedConfigSynced { selections });
        }
    }

    pub fn apply_advertised_modes(&mut self, modes: SessionModeState) {
        let new_id = ModeId::new(modes.current_mode_id.to_string());
        let changed = self.observed.mode_id.as_ref() != Some(&new_id);
        self.observed.mode_id = Some(new_id.clone());
        self.advertised.modes = Some(modes);
        if changed {
            self.pending_events
                .push(AcpSessionEvent::ObservedModeSynced { mode: new_id });
        }
    }

    pub fn apply_advertised_models(&mut self, models: SessionModelState) {
        let new_id = ModelId::new(models.current_model_id.to_string());
        let changed = self.observed.model_id.as_ref() != Some(&new_id);
        self.observed.model_id = Some(new_id.clone());
        self.advertised.models = Some(models);
        if changed {
            self.pending_events
                .push(AcpSessionEvent::ObservedModelSynced { model: new_id });
        }
    }

    pub fn apply_advertised_config_options(&mut self, options: Vec<SessionConfigOption>) {
        let mut changed = false;
        for opt in &options {
            if let Some(current) = extract_config_current_value(&opt.kind) {
                let key = ConfigKey::new(opt.id.to_string());
                let value = ConfigValue::new(current);
                if self.observed.config_current.insert(key, value.clone()).as_ref() != Some(&value) {
                    changed = true;
                }
            }
        }
        self.advertised.config_options = Some(options);
        if changed {
            let selections = self.observed.config_current.clone();
            self.pending_events
                .push(AcpSessionEvent::ObservedConfigSynced { selections });
        }
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

    /// Record the CLI's latest context usage. Diff-driven: emits
    /// `ObservedContextUsageChanged` only when the usage payload differs
    /// from what we last cached, so the persistence consumer can debounce
    /// a stream of token updates into one DB write per turn.
    pub fn apply_context_usage(&mut self, usage: UsageUpdate) {
        let changed = self.advertised.context_usage.as_ref() != Some(&usage);
        self.advertised.context_usage = Some(usage.clone());
        if changed {
            let usage_json = serde_json::to_string(&usage).unwrap_or_default();
            self.pending_events
                .push(AcpSessionEvent::ObservedContextUsageChanged { usage_json });
        }
    }
}

impl AcpSession {
    /// Seed the aggregate with persisted user choices from DB.
    /// Called on resume paths before the CLI session/load response arrives.
    pub fn preload_persisted(&mut self, state: &PersistedSessionState) {
        if let Some(mode) = &state.current_mode_id {
            self.advertised.modes = Some(SessionModeState::new(mode.as_str().to_owned(), Vec::new()));
            self.observed.mode_id = Some(mode.clone());
        }
        if let Some(model) = &state.current_model_id {
            self.advertised.models = Some(SessionModelState::new(model.as_str().to_owned(), Vec::new()));
            self.observed.model_id = Some(model.clone());
        }
        if !state.config_selections.is_empty() {
            self.observed.config_current = state.config_selections.clone();
        }
        if let Some(usage) = &state.context_usage {
            self.advertised.context_usage = Some(usage.clone());
        }
    }
}

// ─── Reconcile ─────────────────────────────────────────────────────
impl AcpSession {
    /// Produce a list of actions needed to align CLI state with user intent.
    /// Pure function — no side effects. The driver executes the actions.
    pub fn plan_reconcile(&self) -> Vec<ReconcileAction> {
        let mut actions = Vec::new();

        if let Some(desired_mode) = &self.desired.mode_id
            && self.observed.mode_id.as_ref() != Some(desired_mode)
        {
            actions.push(ReconcileAction::SetMode {
                mode: desired_mode.clone(),
            });
        }

        if let Some(desired_model) = &self.desired.model_id
            && self.observed.model_id.as_ref() != Some(desired_model)
        {
            actions.push(ReconcileAction::SetModel {
                model: desired_model.clone(),
            });
        }

        for (key, desired_value) in &self.desired.config_selections {
            if self.observed.config_current.get(key) != Some(desired_value) {
                actions.push(ReconcileAction::SetConfigOption {
                    key: key.clone(),
                    value: desired_value.clone(),
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

    /// Record the model id that the next prompt should announce to the
    /// CLI via a `<system-reminder>`. See `pending_model_notice` for the
    /// motivating invariant.
    pub fn set_pending_model_notice(&mut self, model: ModelId) {
        self.pending_model_notice = Some(model);
    }

    /// Drain the pending model notice (if any). Callers consume the
    /// value before sending the next prompt so it is not re-injected.
    pub fn take_pending_model_notice(&mut self) -> Option<ModelId> {
        self.pending_model_notice.take()
    }

    // ─── Private helpers ───────────────────────────────────────────────

    fn is_mode_valid(&self, mode_id: &str) -> bool {
        match &self.advertised.modes {
            None => true,
            Some(modes) if modes.available_modes.is_empty() => true,
            Some(modes) => modes.available_modes.iter().any(|m| m.id.0.as_ref() == mode_id),
        }
    }

    fn is_model_valid(&self, model_id: &str) -> bool {
        match &self.advertised.models {
            None => true,
            Some(models) if models.available_models.is_empty() => true,
            Some(models) => models
                .available_models
                .iter()
                .any(|m| m.model_id.0.as_ref() == model_id),
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
        AcpSession::new(Some(ModeId::new("default")), None, HashMap::new())
    }

    #[test]
    fn assign_session_id_emits_event() {
        let mut session = make_session();
        session.set_session_id(SessionId::new("sess-1"));
        assert_eq!(session.session_id(), Some("sess-1"));
        let events = session.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            AcpSessionEvent::SessionAssigned {
                session_id: SessionId::new("sess-1"),
            }
        );
    }

    #[test]
    fn assign_session_id_is_idempotent() {
        let mut session = make_session();
        session.set_session_id(SessionId::new("sess-1"));
        session.drain_events();
        session.set_session_id(SessionId::new("sess-1"));
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
        assert!(session.set_desired_mode(ModeId::new("plan")));
        assert_eq!(session.desired_mode(), Some("plan"));
        let events = session.drain_events();
        assert_eq!(
            events[0],
            AcpSessionEvent::DesiredModeChanged {
                mode: ModeId::new("plan"),
            }
        );
    }

    #[test]
    fn set_desired_mode_rejects_empty() {
        let mut session = make_session();
        assert!(!session.set_desired_mode(ModeId::new("")));
        assert!(session.drain_events().is_empty());
    }

    #[test]
    fn set_desired_mode_no_op_when_unchanged() {
        let mut session = make_session();
        session.set_desired_mode(ModeId::new("plan"));
        session.drain_events();
        assert!(!session.set_desired_mode(ModeId::new("plan")));
        assert!(session.drain_events().is_empty());
    }

    #[test]
    fn set_desired_mode_validates_against_advertised() {
        let mut session = make_session();
        session.apply_advertised_modes(SessionModeState::new(
            "code",
            vec![SessionMode::new("code", "Code"), SessionMode::new("plan", "Plan")],
        ));
        assert!(session.set_desired_mode(ModeId::new("plan")));
        assert!(!session.set_desired_mode(ModeId::new("nonexistent")));
    }

    #[test]
    fn set_desired_mode_allows_any_when_advertised_empty() {
        let mut session = make_session();
        assert!(session.set_desired_mode(ModeId::new("anything")));
    }

    #[test]
    fn apply_observed_mode_does_not_change_desired() {
        let mut session = make_session();
        session.set_desired_mode(ModeId::new("plan"));
        session.drain_events();
        session.apply_observed_mode(ModeId::new("code"));
        assert_eq!(session.desired_mode(), Some("plan"));
        assert_eq!(session.observed_mode(), Some("code"));
    }

    #[test]
    fn apply_observed_mode_syncs_advertised_current_without_losing_available() {
        use agent_client_protocol::schema::SessionMode;
        let mut session = make_session();
        session.apply_advertised_modes(SessionModeState::new(
            "default",
            vec![SessionMode::new("default", "Default"), SessionMode::new("plan", "Plan")],
        ));
        session.drain_events();

        session.apply_observed_mode(ModeId::new("plan"));

        assert_eq!(session.observed_mode(), Some("plan"));
        assert_eq!(session.current_mode_id().as_deref(), Some("plan"));
        let modes = session.modes().expect("modes present");
        assert_eq!(modes.available_modes.len(), 2, "available_modes must be preserved");
    }

    #[test]
    fn apply_observed_model_syncs_advertised_current_without_losing_available() {
        use agent_client_protocol::schema::ModelInfo;
        let mut session = make_session();
        session.apply_advertised_models(SessionModelState::new(
            "claude-sonnet-4",
            vec![
                ModelInfo::new("claude-sonnet-4", "Sonnet 4"),
                ModelInfo::new("claude-opus-4", "Opus 4"),
            ],
        ));
        session.drain_events();

        session.apply_observed_model(ModelId::new("claude-opus-4"));

        assert_eq!(session.observed_model(), Some("claude-opus-4"));
        assert_eq!(session.current_model_id().as_deref(), Some("claude-opus-4"));
        let models = session.model_info().expect("models present");
        assert_eq!(models.available_models.len(), 2, "available_models must be preserved");
    }

    #[test]
    fn apply_observed_mode_creates_advertised_when_empty() {
        let mut session = make_session();
        session.apply_observed_mode(ModeId::new("plan"));
        assert_eq!(session.current_mode_id().as_deref(), Some("plan"));
    }

    #[test]
    fn apply_observed_model_creates_advertised_when_empty() {
        let mut session = make_session();
        session.apply_observed_model(ModelId::new("claude-opus-4"));
        assert_eq!(session.current_model_id().as_deref(), Some("claude-opus-4"));
    }

    #[test]
    fn apply_observed_config_emits_on_change_and_is_idempotent() {
        let mut session = make_session();
        session.apply_observed_config(ConfigKey::new("reasoning"), ConfigValue::new("high"));
        let events = session.drain_events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            AcpSessionEvent::ObservedConfigSynced { selections } => {
                assert_eq!(
                    selections.get(&ConfigKey::new("reasoning")),
                    Some(&ConfigValue::new("high"))
                );
            }
            other => panic!("expected ObservedConfigSynced, got {other:?}"),
        }

        // Idempotent repeat: no new event.
        session.apply_observed_config(ConfigKey::new("reasoning"), ConfigValue::new("high"));
        assert!(session.drain_events().is_empty());
    }

    #[test]
    fn apply_observed_config_closes_plan_reconcile_drift() {
        let mut session = AcpSession::new(None, None, HashMap::new());
        session.set_desired_config(ConfigKey::new("reasoning"), ConfigValue::new("high"));
        assert_eq!(
            session.plan_reconcile(),
            vec![ReconcileAction::SetConfigOption {
                key: ConfigKey::new("reasoning"),
                value: ConfigValue::new("high"),
            }]
        );

        session.apply_observed_config(ConfigKey::new("reasoning"), ConfigValue::new("high"));
        assert!(
            session.plan_reconcile().is_empty(),
            "plan_reconcile must be a no-op once observed catches up to desired",
        );
    }

    #[test]
    fn plan_reconcile_detects_mode_drift() {
        let mut session = make_session();
        session.set_desired_mode(ModeId::new("plan"));
        session.apply_observed_mode(ModeId::new("default"));
        let actions = session.plan_reconcile();
        assert_eq!(
            actions,
            vec![ReconcileAction::SetMode {
                mode: ModeId::new("plan"),
            }]
        );
    }

    #[test]
    fn plan_reconcile_empty_when_aligned() {
        let mut session = make_session();
        session.set_desired_mode(ModeId::new("plan"));
        session.apply_observed_mode(ModeId::new("plan"));
        assert!(session.plan_reconcile().is_empty());
    }

    #[test]
    fn plan_reconcile_detects_config_drift() {
        let mut session = AcpSession::new(None, None, HashMap::new());
        session.set_desired_config(ConfigKey::new("reasoning"), ConfigValue::new("high"));
        let actions = session.plan_reconcile();
        assert_eq!(
            actions,
            vec![ReconcileAction::SetConfigOption {
                key: ConfigKey::new("reasoning"),
                value: ConfigValue::new("high"),
            }]
        );
    }

    #[test]
    fn plan_reconcile_config_aligned_when_observed_matches() {
        let mut session = AcpSession::new(None, None, HashMap::new());
        session.set_desired_config(ConfigKey::new("reasoning"), ConfigValue::new("high"));

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
        session.set_session_id(SessionId::new("s1"));
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

    #[test]
    fn set_desired_model_emits_when_changed() {
        let mut session = make_session();
        assert!(session.set_desired_model(ModelId::new("claude-sonnet-4")));
        assert_eq!(session.desired_model(), Some("claude-sonnet-4"));
        let events = session.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            AcpSessionEvent::DesiredModelChanged {
                model: ModelId::new("claude-sonnet-4"),
            }
        );
    }

    #[test]
    fn set_desired_model_rejects_empty() {
        let mut session = make_session();
        assert!(!session.set_desired_model(ModelId::new("")));
        assert!(session.drain_events().is_empty());
    }

    #[test]
    fn set_desired_model_no_op_when_unchanged() {
        let mut session = make_session();
        session.set_desired_model(ModelId::new("claude-sonnet-4"));
        session.drain_events();
        assert!(!session.set_desired_model(ModelId::new("claude-sonnet-4")));
        assert!(session.drain_events().is_empty());
    }

    #[test]
    fn set_desired_model_validates_against_advertised() {
        use agent_client_protocol::schema::ModelInfo;
        let mut session = make_session();
        session.apply_advertised_models(SessionModelState::new(
            "claude-sonnet-4",
            vec![
                ModelInfo::new("claude-sonnet-4", "Sonnet 4"),
                ModelInfo::new("claude-opus-4", "Opus 4"),
            ],
        ));
        assert!(session.set_desired_model(ModelId::new("claude-opus-4")));
        assert!(!session.set_desired_model(ModelId::new("nonexistent")));
    }

    #[test]
    fn set_desired_model_allows_any_when_advertised_empty() {
        let mut session = make_session();
        assert!(session.set_desired_model(ModelId::new("anything")));
    }

    #[test]
    fn apply_observed_model_does_not_change_desired_model() {
        let mut session = make_session();
        session.set_desired_model(ModelId::new("claude-opus-4"));
        session.drain_events();
        session.apply_observed_model(ModelId::new("claude-sonnet-4"));
        assert_eq!(session.desired_model(), Some("claude-opus-4"));
        assert_eq!(session.observed_model(), Some("claude-sonnet-4"));
    }

    #[test]
    fn plan_reconcile_detects_model_drift() {
        let mut session = AcpSession::new(None, None, HashMap::new());
        session.set_desired_model(ModelId::new("claude-opus-4"));
        session.apply_observed_model(ModelId::new("claude-sonnet-4"));
        let actions = session.plan_reconcile();
        assert_eq!(
            actions,
            vec![ReconcileAction::SetModel {
                model: ModelId::new("claude-opus-4"),
            }]
        );
    }

    #[test]
    fn plan_reconcile_model_aligned_when_observed_matches() {
        let mut session = AcpSession::new(None, None, HashMap::new());
        session.set_desired_model(ModelId::new("claude-opus-4"));
        session.apply_observed_model(ModelId::new("claude-opus-4"));
        assert!(session.plan_reconcile().is_empty());
    }

    #[test]
    fn new_with_initial_model_sets_desired_model() {
        let session = AcpSession::new(None, Some(ModelId::new("claude-opus-4")), HashMap::new());
        assert_eq!(session.desired_model(), Some("claude-opus-4"));
    }

    #[test]
    fn apply_advertised_config_options_emits_observed_config_synced_on_change() {
        let mut session = AcpSession::new(None, None, HashMap::new());
        session.apply_advertised_config_options(vec![SessionConfigOption::select(
            "reasoning",
            "Reasoning",
            "high",
            vec![
                SessionConfigSelectOption::new("low", "Low"),
                SessionConfigSelectOption::new("high", "High"),
            ],
        )]);
        let events = session.drain_events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            AcpSessionEvent::ObservedConfigSynced { selections } => {
                assert_eq!(
                    selections.get(&ConfigKey::new("reasoning")),
                    Some(&ConfigValue::new("high"))
                );
            }
            other => panic!("expected ObservedConfigSynced, got {other:?}"),
        }
    }

    #[test]
    fn apply_advertised_config_options_idempotent_when_unchanged() {
        let mut session = AcpSession::new(None, None, HashMap::new());
        let options = vec![SessionConfigOption::select(
            "reasoning",
            "Reasoning",
            "high",
            vec![
                SessionConfigSelectOption::new("low", "Low"),
                SessionConfigSelectOption::new("high", "High"),
            ],
        )];
        session.apply_advertised_config_options(options.clone());
        session.drain_events();

        session.apply_advertised_config_options(options);
        let events = session.drain_events();
        assert!(
            events.is_empty(),
            "no ObservedConfigSynced when observed unchanged, got {events:?}"
        );
    }

    #[test]
    fn pending_model_notice_roundtrip_and_take_once() {
        use crate::shared_kernel::ModelId;

        let mut s = AcpSession::new(None, None, HashMap::new());
        assert!(s.take_pending_model_notice().is_none(), "default is None");

        s.set_pending_model_notice(ModelId::new("opus"));
        let taken = s.take_pending_model_notice();
        assert_eq!(taken.as_ref().map(|m| m.as_str()), Some("opus"));

        // Take is destructive — the second take must see None.
        assert!(s.take_pending_model_notice().is_none());
    }

    #[test]
    fn set_desired_mode_plus_plan_reconcile_produces_set_mode_action() {
        // This test documents the Stage 4 invariant: the manager's set_mode
        // should only (a) call set_desired_mode on the aggregate and (b) delegate
        // to plan_reconcile for the SDK call. Plan_reconcile should emit
        // ReconcileAction::SetMode when desired and observed diverge.
        let mut session = AcpSession::new(None, None, Default::default());
        session.apply_advertised_modes(SessionModeState::new(
            "default".to_owned(),
            vec![SessionMode::new("default", "Default"), SessionMode::new("plan", "Plan")],
        ));
        session.apply_observed_mode(ModeId::new("default"));
        assert_eq!(session.plan_reconcile(), vec![]);

        // User chooses "plan" via set_desired_mode (what set_mode will do).
        assert!(session.set_desired_mode(ModeId::new("plan")));

        // Now reconcile should want to set CLI mode to "plan".
        let actions = session.plan_reconcile();
        assert_eq!(
            actions,
            vec![ReconcileAction::SetMode {
                mode: ModeId::new("plan")
            }]
        );
    }

    #[test]
    fn pending_session_new_prelude_defaults_to_false() {
        let mut s = make_session();
        assert!(!s.take_pending_session_new_prelude());
    }

    #[test]
    fn mark_pending_session_new_prelude_sets_true() {
        let mut s = make_session();
        s.mark_pending_session_new_prelude();
        assert!(s.take_pending_session_new_prelude());
    }

    #[test]
    fn take_pending_session_new_prelude_is_destructive() {
        let mut s = make_session();
        s.mark_pending_session_new_prelude();
        assert!(s.take_pending_session_new_prelude());
        assert!(!s.take_pending_session_new_prelude());
    }

    #[test]
    fn mark_pending_session_new_prelude_is_idempotent() {
        let mut s = make_session();
        s.mark_pending_session_new_prelude();
        s.mark_pending_session_new_prelude();
        assert!(s.take_pending_session_new_prelude());
        assert!(!s.take_pending_session_new_prelude());
    }
}
