//! Integration tests guarding the structural fix for ELECTRON-1KB.
//!
//! These tests assert that `cancel_current_turn()` is fully synchronous
//! with respect to the next `run_turn()` — no caller can observe `Busy`
//! after a successful cancel returns.

use std::sync::Arc;
use std::time::Duration;

use aionui_ai_agent::connector::{ConnectorError, IAgentConnector};
use aionui_ai_agent::manager::aionrs::AionrsAgentManager;
use aionui_ai_agent::types::{AionrsCompatOverrides, AionrsResolvedConfig, SendMessageData};

fn make_test_config() -> AionrsResolvedConfig {
    AionrsResolvedConfig {
        provider: "anthropic".into(),
        api_key: "sk-test-key".into(),
        model: "claude-sonnet-4-20250514".into(),
        base_url: None,
        system_prompt: None,
        max_tokens: 4096,
        max_turns: None,
        compat_overrides: AionrsCompatOverrides::default(),
        session_directory: std::env::temp_dir().join("aionrs-it-cancel"),
        session_mode: None,
        extra_mcp_servers: std::collections::HashMap::new(),
        bedrock_config: None,
    }
}

#[tokio::test]
async fn cancel_then_run_turn_does_not_return_busy() {
    let agent = Arc::new(
        AionrsAgentManager::new("conv-it".into(), "/project".into(), make_test_config(), None)
            .await
            .unwrap(),
    );

    // Simulate an in-flight turn via the test hook: hold a guard, then
    // cancel and immediately try to start another turn.
    let guard = agent.begin_turn_for_test().expect("turn slot free");
    let agent_for_cancel = agent.clone();
    let cancel = tokio::spawn(async move {
        agent_for_cancel.cancel_for_test().await.unwrap();
    });

    // Allow cancel to begin awaiting the done-receiver.
    tokio::time::sleep(Duration::from_millis(20)).await;
    drop(guard); // Releases done_tx → cancel future resolves.
    cancel.await.unwrap();

    // Immediately attempt a fresh turn. With the structural fix, the
    // connector slot is free and Busy is impossible.
    let connector: Arc<dyn IAgentConnector> = agent.clone();
    let res = connector
        .run_turn(SendMessageData {
            content: "hi".into(),
            msg_id: "m-after-cancel".into(),
            files: vec![],
            inject_skills: vec![],
        })
        .await;
    // We expect the call to *attempt* a turn; without a real LLM provider
    // it will fail with Protocol(_), but it MUST NOT be Busy.
    assert!(
        !matches!(res, Err(ConnectorError::Busy)),
        "cancel→run_turn race not closed: got Busy"
    );
}
