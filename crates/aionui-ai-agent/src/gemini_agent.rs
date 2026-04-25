use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use aionui_common::{
    AgentKillReason, AgentType, AppError, Confirmation, ConversationStatus, TimestampMs, now_ms,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::{debug, error, info, warn};

use crate::agent_manager::IAgentManager;
use aionui_common::CommandSpec;

use crate::cli_process::CliAgentProcess;
use crate::skill_manager::{AcpSkillManager, detect_skill_load_request};
use crate::stream_event::AgentStreamEvent;
use crate::types::{GeminiBuildExtra, SendMessageData};

/// Grace period before force-killing a Gemini process (ms).
const GEMINI_KILL_GRACE_MS: u64 = 1000;

/// Gemini CLI protocol commands.
mod protocol {
    pub const START: &str = "start";
    pub const SEND_MESSAGE: &str = "send.message";
    pub const STOP_STREAM: &str = "stop.stream";
}

/// Session mode for Gemini agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiSessionMode {
    Default,
    Yolo,
    AutoEdit,
}

impl GeminiSessionMode {
    fn from_str(s: &str) -> Self {
        match s {
            "yolo" => Self::Yolo,
            "autoEdit" => Self::AutoEdit,
            _ => Self::Default,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Yolo => "yolo",
            Self::AutoEdit => "autoEdit",
        }
    }
}

/// Internal mutable state for the Gemini agent.
struct GeminiState {
    status: Option<ConversationStatus>,
    session_mode: GeminiSessionMode,
    confirmations: Vec<Confirmation>,
    has_messages: bool,
    approval_memory: HashMap<String, bool>,
    /// Fingerprint of the last MCP config for change detection.
    mcp_fingerprint: Option<String>,
}

/// Manages a Gemini CLI agent subprocess.
///
/// Gemini agents run via the `aioncli-core` CLI binary. Key features:
/// - MCP fingerprint detection: re-bootstraps when MCP config changes
/// - Skill loading interception: detects `[LOAD_SKILL]` in output
/// - Session modes: default, yolo, autoEdit
pub struct GeminiAgentManager {
    conversation_id: String,
    workspace: String,
    config: GeminiBuildExtra,
    process: Arc<CliAgentProcess>,
    event_tx: broadcast::Sender<AgentStreamEvent>,
    state: RwLock<GeminiState>,
    last_activity: AtomicI64,
    raw_rx: Mutex<Option<broadcast::Receiver<Value>>>,
    skill_manager: Option<Arc<AcpSkillManager>>,
}

impl GeminiAgentManager {
    /// Create a new Gemini agent by spawning the CLI subprocess.
    pub async fn new(
        conversation_id: String,
        workspace: String,
        cli_path: String,
        config: GeminiBuildExtra,
        skill_manager: Option<Arc<AcpSkillManager>>,
    ) -> Result<Self, AppError> {
        let session_mode = config
            .session_mode
            .as_deref()
            .map(GeminiSessionMode::from_str)
            .unwrap_or(GeminiSessionMode::Default);

        let spawn_config = Self::build_spawn_config(&cli_path, &workspace);
        let process = CliAgentProcess::spawn(spawn_config).await?;

        let raw_rx = process
            .take_initial_receiver()
            .expect("Initial receiver should be available immediately after spawn");
        let (event_tx, _) = broadcast::channel(256);

        Ok(Self {
            conversation_id,
            workspace,
            config,
            process: Arc::new(process),
            event_tx,
            state: RwLock::new(GeminiState {
                status: None,
                session_mode,
                confirmations: Vec::new(),
                has_messages: false,
                approval_memory: HashMap::new(),
                mcp_fingerprint: None,
            }),
            last_activity: AtomicI64::new(now_ms()),
            raw_rx: Mutex::new(Some(raw_rx)),
            skill_manager,
        })
    }

    fn build_spawn_config(cli_path: &str, workspace: &str) -> CommandSpec {
        CommandSpec {
            command: cli_path.into(),
            args: vec![],
            env: vec![],
            cwd: Some(workspace.to_owned()),
        }
    }

    /// Start the event relay (call after wrapping in Arc).
    pub fn start_relay(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.run_event_relay().await;
        });
    }

    async fn run_event_relay(self: Arc<Self>) {
        let mut raw_rx = {
            let mut guard = self.raw_rx.lock().await;
            match guard.take() {
                Some(rx) => rx,
                None => {
                    warn!(
                        conversation_id = %self.conversation_id,
                        "Gemini event relay already started"
                    );
                    return;
                }
            }
        };

        loop {
            match raw_rx.recv().await {
                Ok(raw_json) => {
                    self.last_activity.store(now_ms(), Ordering::Relaxed);
                    self.handle_raw_event(raw_json).await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        conversation_id = %self.conversation_id,
                        lagged = n,
                        "Gemini event relay lagged"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!(
                        conversation_id = %self.conversation_id,
                        "Gemini CLI event channel closed"
                    );
                    break;
                }
            }
        }

        let mut state = self.state.write().await;
        if state.status == Some(ConversationStatus::Running) {
            state.status = Some(ConversationStatus::Finished);
        }
    }

    async fn handle_raw_event(&self, raw: Value) {
        let stream_event = match serde_json::from_value::<AgentStreamEvent>(raw.clone()) {
            Ok(event) => event,
            Err(_) => {
                debug!(
                    conversation_id = %self.conversation_id,
                    "Unrecognized Gemini event, skipping"
                );
                return;
            }
        };

        // Intercept skill load requests from text events
        if let AgentStreamEvent::Text(ref text_data) = stream_event {
            self.handle_skill_load_interception(&text_data.content)
                .await;
        }

        self.update_state_from_event(&stream_event).await;
        let _ = self.event_tx.send(stream_event);
    }

    /// Detect `[LOAD_SKILL: name]` in text and inject skill content as system response.
    async fn handle_skill_load_interception(&self, content: &str) {
        let skill_names = detect_skill_load_request(content);
        if skill_names.is_empty() {
            return;
        }

        let Some(ref skill_mgr) = self.skill_manager else {
            return;
        };

        for name in skill_names {
            if let Some(skill) = skill_mgr.get_skill(&name).await
                && let Some(ref body) = skill.body
            {
                let response = format!(
                    "[System Response]\nSkill '{}' loaded:\n\n{}\n[/System Response]",
                    name, body
                );
                let payload = json!({
                    "type": protocol::SEND_MESSAGE,
                    "data": { "content": response, "msgId": format!("skill-{name}") }
                });
                if let Err(e) = self.process.send(&payload).await {
                    warn!(
                        conversation_id = %self.conversation_id,
                        skill = name,
                        error = %e,
                        "Failed to inject skill content"
                    );
                }
            }
        }
    }

    async fn update_state_from_event(&self, event: &AgentStreamEvent) {
        match event {
            AgentStreamEvent::Start(_) => {
                let mut state = self.state.write().await;
                state.status = Some(ConversationStatus::Running);
            }
            AgentStreamEvent::Finish(_) => {
                let mut state = self.state.write().await;
                state.status = Some(ConversationStatus::Finished);
            }
            AgentStreamEvent::Error(_) => {
                let mut state = self.state.write().await;
                state.status = Some(ConversationStatus::Finished);
            }
            AgentStreamEvent::AcpPermission(data) => {
                if let Ok(conf) = serde_json::from_value::<Confirmation>(data.clone()) {
                    self.add_confirmation(conf).await;
                }
            }
            _ => {}
        }
    }

    async fn add_confirmation(&self, confirmation: Confirmation) {
        let mut guard = self.state.write().await;
        if let Some(existing) = guard
            .confirmations
            .iter_mut()
            .find(|c| c.call_id == confirmation.call_id)
        {
            *existing = confirmation;
        } else {
            guard.confirmations.push(confirmation);
        }
    }

    /// Get the current MCP config fingerprint.
    pub async fn mcp_fingerprint(&self) -> Option<String> {
        self.state.read().await.mcp_fingerprint.clone()
    }

    /// Update the MCP config fingerprint. Returns true if changed.
    pub async fn update_mcp_fingerprint(&self, new_fingerprint: String) -> bool {
        let mut state = self.state.write().await;
        let changed = state.mcp_fingerprint.as_ref() != Some(&new_fingerprint);
        if changed {
            state.mcp_fingerprint = Some(new_fingerprint);
        }
        changed
    }

    /// Get the session mode.
    pub async fn session_mode(&self) -> GeminiSessionMode {
        self.state.read().await.session_mode
    }
}

use crate::agent_manager::approval_key;

#[async_trait::async_trait]
impl IAgentManager for GeminiAgentManager {
    fn agent_type(&self) -> AgentType {
        AgentType::Gemini
    }

    fn status(&self) -> Option<ConversationStatus> {
        self.state.try_read().ok().and_then(|g| g.status)
    }

    fn workspace(&self) -> &str {
        &self.workspace
    }

    fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    fn last_activity_at(&self) -> TimestampMs {
        self.last_activity.load(Ordering::Relaxed)
    }

    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.event_tx.subscribe()
    }

    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError> {
        self.last_activity.store(now_ms(), Ordering::Relaxed);

        let is_first = {
            let mut state = self.state.write().await;
            let first = !state.has_messages;
            state.has_messages = true;
            state.status = Some(ConversationStatus::Running);
            first
        };

        if is_first {
            // Bootstrap: send start command with config
            let start_payload = json!({
                "type": protocol::START,
                "data": {
                    "workspace": self.workspace,
                    "webSearchEngine": self.config.web_search_engine,
                    "contextFileName": self.config.context_file_name,
                    "contextContent": self.config.context_content,
                    "presetRules": self.config.preset_rules,
                    "enabledSkills": self.config.enabled_skills,
                    "sessionMode": self.state.read().await.session_mode.as_str(),
                }
            });
            self.process.send(&start_payload).await?;
        }

        let mut payload = json!({
            "type": protocol::SEND_MESSAGE,
            "data": {
                "content": data.content,
                "msgId": data.msg_id,
            }
        });

        if !data.files.is_empty() {
            payload["data"]["files"] = json!(data.files);
        }
        if !data.inject_skills.is_empty() {
            payload["data"]["injectSkills"] = json!(data.inject_skills);
        }

        self.process.send(&payload).await
    }

    async fn stop(&self) -> Result<(), AppError> {
        let payload = json!({ "type": protocol::STOP_STREAM, "data": {} });
        self.process.send(&payload).await?;

        let mut state = self.state.write().await;
        state.confirmations.clear();
        Ok(())
    }

    fn confirm(
        &self,
        _msg_id: &str,
        call_id: &str,
        data: Value,
        always_allow: bool,
    ) -> Result<(), AppError> {
        if let Ok(mut state) = self.state.try_write() {
            if always_allow
                && let Some(conf) = state.confirmations.iter().find(|c| c.call_id == call_id)
            {
                let key = approval_key(conf.action.as_deref(), conf.command_type.as_deref());
                state.approval_memory.insert(key, true);
            }
            state.confirmations.retain(|c| c.call_id != call_id);
        }

        let process = Arc::clone(&self.process);
        let call_id = call_id.to_owned();
        tokio::spawn(async move {
            let payload = json!({
                "type": call_id,
                "data": data,
            });
            if let Err(e) = process.send(&payload).await {
                error!(error = %e, "Failed to send Gemini confirmation");
            }
        });

        Ok(())
    }

    fn get_confirmations(&self) -> Vec<Confirmation> {
        self.state
            .try_read()
            .map(|g| g.confirmations.clone())
            .unwrap_or_default()
    }

    fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool {
        self.state
            .try_read()
            .map(|g| {
                let key = approval_key(Some(action), command_type);
                g.approval_memory.get(&key).copied().unwrap_or(false)
            })
            .unwrap_or(false)
    }

    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        info!(
            conversation_id = %self.conversation_id,
            ?reason,
            "Killing Gemini agent"
        );

        let process = Arc::clone(&self.process);
        let grace = Duration::from_millis(GEMINI_KILL_GRACE_MS);
        tokio::spawn(async move {
            if let Err(e) = process.kill(grace).await {
                error!(error = %e, "Failed to kill Gemini process");
            }
        });

        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_session_mode_roundtrip() {
        assert_eq!(
            GeminiSessionMode::from_str("default"),
            GeminiSessionMode::Default
        );
        assert_eq!(GeminiSessionMode::from_str("yolo"), GeminiSessionMode::Yolo);
        assert_eq!(
            GeminiSessionMode::from_str("autoEdit"),
            GeminiSessionMode::AutoEdit
        );
        assert_eq!(
            GeminiSessionMode::from_str("unknown"),
            GeminiSessionMode::Default
        );

        assert_eq!(GeminiSessionMode::Default.as_str(), "default");
        assert_eq!(GeminiSessionMode::Yolo.as_str(), "yolo");
        assert_eq!(GeminiSessionMode::AutoEdit.as_str(), "autoEdit");
    }

    #[test]
    fn approval_key_formats_correctly() {
        assert_eq!(approval_key(Some("edit"), Some("file")), "edit:file");
        assert_eq!(approval_key(Some("edit"), None), "edit");
        assert_eq!(approval_key(None, Some("file")), "");
        assert_eq!(approval_key(None, None), "");
    }

    #[test]
    fn build_spawn_config_sets_cwd() {
        let config = GeminiAgentManager::build_spawn_config("/usr/bin/gemini", "/project");
        assert_eq!(config.command.to_str().unwrap(), "/usr/bin/gemini");
        assert_eq!(config.cwd, Some("/project".into()));
        assert!(config.args.is_empty());
    }
}
