use crate::stream_event::AgentStreamEvent;
use agent_client_protocol::schema::{
    AgentCapabilities, AuthMethod, AvailableCommand, SessionConfigOption, SessionModeState,
    SessionModelState, UsageUpdate,
};

#[derive(Debug, Clone, Default)]
pub struct AcpRuntimeSnapshot {
    modes: Option<SessionModeState>,
    model_info: Option<SessionModelState>,
    config_options: Option<Vec<SessionConfigOption>>,
    context_usage: Option<UsageUpdate>,
    agent_capabilities: Option<AgentCapabilities>,
    auth_methods: Option<Vec<AuthMethod>>,
    available_commands: Option<Vec<AvailableCommand>>,
}

impl AcpRuntimeSnapshot {
    pub fn modes(&self) -> Option<&SessionModeState> {
        self.modes.as_ref()
    }
    pub fn model_info(&self) -> Option<&SessionModelState> {
        self.model_info.as_ref()
    }
    pub fn config_options(&self) -> Option<&[SessionConfigOption]> {
        self.config_options.as_deref()
    }
    pub fn context_usage(&self) -> Option<&UsageUpdate> {
        self.context_usage.as_ref()
    }
    pub fn agent_capabilities(&self) -> Option<&AgentCapabilities> {
        self.agent_capabilities.as_ref()
    }
    pub fn auth_methods(&self) -> Option<&[AuthMethod]> {
        self.auth_methods.as_deref()
    }
    pub fn available_commands(&self) -> Option<&[AvailableCommand]> {
        self.available_commands.as_deref()
    }

    pub fn set_modes(&mut self, modes: SessionModeState) {
        self.modes = Some(modes);
    }
    pub fn set_model_info(&mut self, model_info: SessionModelState) {
        self.model_info = Some(model_info);
    }
    pub fn set_config_options(&mut self, config_options: Vec<SessionConfigOption>) {
        self.config_options = Some(config_options);
    }
    pub fn set_context_usage(&mut self, context_usage: UsageUpdate) {
        self.context_usage = Some(context_usage);
    }
    pub fn set_agent_capabilities(&mut self, agent_capabilities: AgentCapabilities) {
        self.agent_capabilities = Some(agent_capabilities);
    }
    pub fn set_auth_methods(&mut self, auth_methods: Vec<AuthMethod>) {
        self.auth_methods = Some(auth_methods);
    }
    pub fn set_available_commands(&mut self, available_commands: Vec<AvailableCommand>) {
        self.available_commands = Some(available_commands);
    }

    pub fn apply_event(&mut self, event: &AgentStreamEvent) {
        match event {
            AgentStreamEvent::AcpModeInfo(value) => {
                if let Ok(update) = serde_json::from_value::<SessionModeState>(value.clone()) {
                    self.modes = Some(update);
                }
            }
            AgentStreamEvent::AcpModelInfo(value) => {
                if let Ok(update) = serde_json::from_value::<SessionModelState>(value.clone()) {
                    self.model_info = Some(update);
                }
            }
            AgentStreamEvent::AcpConfigOption(value) => {
                if let Ok(update) =
                    serde_json::from_value::<Vec<SessionConfigOption>>(value.clone())
                {
                    self.config_options = Some(update);
                }
            }
            AgentStreamEvent::AcpContextUsage(value) => {
                if let Ok(update) = serde_json::from_value::<UsageUpdate>(value.clone()) {
                    self.context_usage = Some(update);
                }
            }
            AgentStreamEvent::AvailableCommands(data) => {
                self.available_commands = Some(data.commands.clone());
            }
            _ => {}
        }
    }

    pub fn current_mode_id(&self) -> Option<String> {
        self.modes
            .as_ref()
            .map(|modes| modes.current_mode_id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::{
        AvailableCommand, ModelInfo, SessionConfigOption, SessionConfigSelectOption, SessionMode,
        SessionModeState, SessionModelState, UsageUpdate,
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn stores_agent_capabilities() {
        let mut snapshot = AcpRuntimeSnapshot::default();
        snapshot.set_agent_capabilities(AgentCapabilities::new().load_session(true));

        let caps = snapshot
            .agent_capabilities()
            .expect("agent capabilities should be cached");
        assert!(caps.load_session);
    }

    #[test]
    fn applies_mode_update_into_session_mode_state() {
        let mut snapshot = AcpRuntimeSnapshot::default();
        snapshot.set_modes(SessionModeState::new(
            "code",
            vec![
                SessionMode::new("code", "Code"),
                SessionMode::new("plan", "Plan"),
            ],
        ));

        snapshot.apply_event(&AgentStreamEvent::AcpModeInfo(json!({
            "currentModeId": "plan"
        })));

        let modes = snapshot.modes().expect("modes should exist");
        assert_eq!(modes.current_mode_id.to_string(), "plan");
        assert_eq!(modes.available_modes.len(), 2);
    }

    #[test]
    fn applies_config_update_into_sdk_config_options() {
        let mut snapshot = AcpRuntimeSnapshot::default();
        snapshot.apply_event(&AgentStreamEvent::AcpConfigOption(json!([
            SessionConfigOption::select(
                "mode",
                "Mode",
                "code",
                vec![SessionConfigSelectOption::new("code", "Code")],
            )
        ])));

        let config_options = snapshot
            .config_options()
            .expect("config options should be cached");
        assert_eq!(config_options.len(), 1);
        assert_eq!(config_options[0].name, "Mode");
    }

    #[test]
    fn stores_model_info_and_usage() {
        let mut snapshot = AcpRuntimeSnapshot::default();
        snapshot.set_model_info(SessionModelState::new(
            "claude-sonnet-4",
            vec![ModelInfo::new("claude-sonnet-4", "Claude Sonnet 4")],
        ));
        snapshot.set_context_usage(UsageUpdate::new(1024, 8192));

        assert_eq!(
            snapshot
                .model_info()
                .expect("model info should be cached")
                .current_model_id
                .to_string(),
            "claude-sonnet-4"
        );
        assert_eq!(
            snapshot
                .context_usage()
                .expect("usage should be cached")
                .used,
            1024
        );
    }

    #[test]
    fn applies_available_commands_update() {
        let mut snapshot = AcpRuntimeSnapshot::default();
        assert!(snapshot.available_commands().is_none());

        let cmds = vec![
            AvailableCommand::new("review", "Review current changes"),
            AvailableCommand::new("compact", "Summarize conversation"),
        ];

        snapshot.apply_event(&AgentStreamEvent::AvailableCommands(
            crate::stream_event::AvailableCommandsEventData {
                commands: cmds.clone(),
            },
        ));

        let stored = snapshot
            .available_commands()
            .expect("available commands should be cached");
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].name, "review");
        assert_eq!(stored[1].name, "compact");
    }
}
