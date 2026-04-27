use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};

use aion_agent::bootstrap::AgentBootstrap;
use aion_agent::engine::AgentEngine;
use aion_agent::output::OutputSink;
use aion_config::compat::ProviderCompat;
use aion_config::config::{Config, ProviderType};
use aion_mcp::manager::McpManager;
use aion_protocol::ToolApprovalManager;
use aionui_common::{
    AgentKillReason, AgentType, AppError, Confirmation, ConversationStatus, TimestampMs, now_ms,
};
use serde_json::Value;
use tokio::sync::{Mutex, broadcast};
use tracing::{debug, error, info};

use crate::agent_manager::IAgentManager;
use crate::backend_output_sink::BackendOutputSink;
use crate::stream_event::AgentStreamEvent;
use crate::types::{AionrsResolvedConfig, SendMessageData};

pub struct AionrsAgentManager {
    conversation_id: String,
    workspace: String,
    pub(crate) event_tx: broadcast::Sender<AgentStreamEvent>,
    last_activity: AtomicI64,
    engine: Mutex<AgentEngine>,
    #[allow(dead_code)] // held for Arc drop cleanup on agent destruction
    mcp_managers: Vec<Arc<McpManager>>,
    status: RwLock<Option<ConversationStatus>>,
    approval_manager: Arc<ToolApprovalManager>,
}

impl AionrsAgentManager {
    pub async fn new(
        conversation_id: String,
        workspace: String,
        config_extra: AionrsResolvedConfig,
    ) -> Result<Self, AppError> {
        let (event_tx, _) = broadcast::channel(128);
        let sink: Arc<dyn OutputSink> = Arc::new(BackendOutputSink::new(event_tx.clone()));

        let provider_type = match config_extra.provider.as_str() {
            "openai" => ProviderType::OpenAI,
            "bedrock" => ProviderType::Bedrock,
            "vertex" => ProviderType::Vertex,
            _ => ProviderType::Anthropic,
        };

        let mut compat = match provider_type {
            ProviderType::OpenAI => ProviderCompat::openai_defaults(),
            ProviderType::Bedrock => ProviderCompat::bedrock_defaults(),
            ProviderType::Anthropic | ProviderType::Vertex => ProviderCompat::anthropic_defaults(),
        };
        if let Some(field) = config_extra.compat_overrides.max_tokens_field {
            compat.max_tokens_field = Some(field);
        }
        if let Some(path) = config_extra.compat_overrides.api_path {
            compat.api_path = Some(path);
        }

        let prompt_caching = matches!(
            provider_type,
            ProviderType::Anthropic | ProviderType::Bedrock | ProviderType::Vertex
        );

        let config = Config {
            provider_label: config_extra.provider.clone(),
            provider: provider_type,
            api_key: config_extra.api_key,
            base_url: config_extra.base_url.unwrap_or_default(),
            model: config_extra.model,
            max_tokens: config_extra.max_tokens,
            max_turns: config_extra.max_turns,
            system_prompt: config_extra.system_prompt,
            thinking: None,
            prompt_caching,
            compat,
            tools: Default::default(),
            session: Default::default(),
            compact: Default::default(),
            plan: Default::default(),
            file_cache: Default::default(),
            hooks: Default::default(),
            bedrock: None,
            vertex: None,
            mcp: Default::default(),
            debug: Default::default(),
        };

        let result = AgentBootstrap::new(config, &workspace, sink)
            .build()
            .await
            .map_err(|e| AppError::Internal(format!("Agent bootstrap failed: {e}")))?;

        let approval_manager = Arc::new(ToolApprovalManager::new());

        Ok(Self {
            conversation_id,
            workspace,
            event_tx,
            last_activity: AtomicI64::new(now_ms()),
            engine: Mutex::new(result.engine),
            mcp_managers: result.mcp_managers,
            status: RwLock::new(Some(ConversationStatus::Pending)),
            approval_manager,
        })
    }
}

#[async_trait::async_trait]
impl IAgentManager for AionrsAgentManager {
    fn agent_type(&self) -> AgentType {
        AgentType::Aionrs
    }

    fn status(&self) -> Option<ConversationStatus> {
        self.status.read().ok().and_then(|s| *s)
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
        let started_at = now_ms();
        info!(
            conversation_id = %self.conversation_id,
            msg_id = %data.msg_id,
            "Aionrs send_message started"
        );
        self.last_activity.store(started_at, Ordering::Relaxed);

        if let Ok(mut s) = self.status.write() {
            *s = Some(ConversationStatus::Running);
        }

        let mut engine = self.engine.lock().await;
        let result = engine.run(&data.content, &data.msg_id).await;

        let elapsed_ms = now_ms() - started_at;

        if let Ok(mut s) = self.status.write() {
            *s = Some(ConversationStatus::Finished);
        }

        self.last_activity.store(now_ms(), Ordering::Relaxed);

        match result {
            Ok(_) => {
                info!(
                    conversation_id = %self.conversation_id,
                    elapsed_ms,
                    "Aionrs engine.run() completed, emitting Finish"
                );
                // AgentEngine.run() does not call emit_stream_end(), so we must
                // send the Finish event ourselves to unblock StreamRelay.
                let _ = self.event_tx.send(AgentStreamEvent::Finish(
                    crate::stream_event::FinishEventData { session_id: None },
                ));
                Ok(())
            }
            Err(e) => {
                let error_msg = format!("Aionrs agent error: {e}");
                error!(
                    conversation_id = %self.conversation_id,
                    elapsed_ms,
                    error = %e,
                    "Aionrs engine.run() failed, emitting Error+Finish"
                );
                let _ = self.event_tx.send(AgentStreamEvent::Error(
                    crate::stream_event::ErrorEventData {
                        message: error_msg.clone(),
                        code: None,
                    },
                ));
                let _ = self.event_tx.send(AgentStreamEvent::Finish(
                    crate::stream_event::FinishEventData { session_id: None },
                ));
                Err(AppError::Internal(error_msg))
            }
        }
    }

    async fn stop(&self) -> Result<(), AppError> {
        info!(
            conversation_id = %self.conversation_id,
            "Aionrs stop requested"
        );
        let _ = self.event_tx.send(AgentStreamEvent::Error(
            crate::stream_event::ErrorEventData {
                message: "Stopped by user".into(),
                code: None,
            },
        ));
        let _ = self.event_tx.send(AgentStreamEvent::Finish(
            crate::stream_event::FinishEventData { session_id: None },
        ));
        if let Ok(mut s) = self.status.write() {
            *s = Some(ConversationStatus::Finished);
        }
        Ok(())
    }

    fn confirm(
        &self,
        _msg_id: &str,
        call_id: &str,
        _data: Value,
        always_allow: bool,
    ) -> Result<(), AppError> {
        let scope = if always_allow {
            aion_protocol::commands::ApprovalScope::Always
        } else {
            aion_protocol::commands::ApprovalScope::Once
        };
        debug!(
            conversation_id = %self.conversation_id,
            call_id,
            always_allow,
            "Aionrs confirm"
        );
        self.approval_manager.approve(call_id, scope);
        Ok(())
    }

    fn get_confirmations(&self) -> Vec<Confirmation> {
        Vec::new()
    }

    fn check_approval(&self, action: &str, _command_type: Option<&str>) -> bool {
        self.approval_manager.is_auto_approved(action)
    }

    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError> {
        info!(
            conversation_id = %self.conversation_id,
            ?reason,
            "Killing Aionrs agent"
        );
        if let Ok(mut s) = self.status.write() {
            *s = None;
        }
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_config() -> AionrsResolvedConfig {
        AionrsResolvedConfig {
            provider: "anthropic".into(),
            api_key: "sk-test-key".into(),
            model: "claude-sonnet-4-20250514".into(),
            base_url: None,
            system_prompt: None,
            max_tokens: 4096,
            max_turns: None,
            compat_overrides: Default::default(),
        }
    }

    #[tokio::test]
    async fn aionrs_agent_returns_correct_type() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config())
            .await
            .unwrap();
        assert_eq!(agent.agent_type(), AgentType::Aionrs);
        assert_eq!(agent.workspace(), "/project");
        assert_eq!(agent.conversation_id(), "conv-1");
    }

    #[tokio::test]
    async fn aionrs_agent_initial_status_is_pending() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config())
            .await
            .unwrap();
        assert_eq!(agent.status(), Some(ConversationStatus::Pending));
    }

    #[tokio::test]
    async fn aionrs_agent_subscribe_returns_receiver() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config())
            .await
            .unwrap();
        let _rx = agent.subscribe();
    }

    #[tokio::test]
    async fn aionrs_agent_kill_succeeds() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config())
            .await
            .unwrap();
        assert!(agent.kill(None).is_ok());
        assert_eq!(agent.status(), None);
    }

    #[tokio::test]
    async fn aionrs_agent_kill_with_reason_succeeds() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config())
            .await
            .unwrap();
        assert!(agent.kill(Some(AgentKillReason::IdleTimeout)).is_ok());
    }

    #[tokio::test]
    async fn aionrs_agent_confirmations_initially_empty() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config())
            .await
            .unwrap();
        assert!(agent.get_confirmations().is_empty());
    }

    #[tokio::test]
    async fn aionrs_agent_check_approval_returns_false_by_default() {
        let agent = AionrsAgentManager::new("conv-1".into(), "/project".into(), make_test_config())
            .await
            .unwrap();
        assert!(!agent.check_approval("any_action", None));
    }

    #[tokio::test]
    async fn stop_emits_finish_event_and_sets_status() {
        let agent =
            AionrsAgentManager::new("conv-stop".into(), "/project".into(), make_test_config())
                .await
                .unwrap();
        let mut rx = agent.subscribe();

        agent.stop().await.unwrap();

        assert_eq!(agent.status(), Some(ConversationStatus::Finished));

        match rx.try_recv().unwrap() {
            AgentStreamEvent::Error(data) => {
                assert!(data.message.contains("Stopped"));
            }
            other => panic!("Expected Error, got {:?}", other),
        }
        match rx.try_recv().unwrap() {
            AgentStreamEvent::Finish(_) => {}
            other => panic!("Expected Finish, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn event_tx_can_send_error_and_finish() {
        let agent =
            AionrsAgentManager::new("conv-err".into(), "/project".into(), make_test_config())
                .await
                .unwrap();
        let mut rx = agent.subscribe();

        let _ = agent.event_tx.send(AgentStreamEvent::Error(
            crate::stream_event::ErrorEventData {
                message: "test error".into(),
                code: None,
            },
        ));
        let _ = agent.event_tx.send(AgentStreamEvent::Finish(
            crate::stream_event::FinishEventData { session_id: None },
        ));

        match rx.try_recv().unwrap() {
            AgentStreamEvent::Error(data) => assert_eq!(data.message, "test error"),
            other => panic!("Expected Error, got {:?}", other),
        }
        match rx.try_recv().unwrap() {
            AgentStreamEvent::Finish(_) => {}
            other => panic!("Expected Finish, got {:?}", other),
        }
    }
}
