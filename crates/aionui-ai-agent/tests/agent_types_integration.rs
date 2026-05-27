//! Integration tests for residual cross-cutting agent helpers.
//!
//! Tests that exercised the legacy task-manager / mock-agent fan-out
//! (`collect_idle_ignores_non_acp_agent_types`) are covered by
//! `aionui-team` and `aionui-cron` integration suites. What remains
//! here is what is genuinely scoped to this crate:
//! - Aionrs manager metadata via the new `IAgentConnector` surface
//! - workspace browsing (filesystem, no agent involved)
//! - `build_system_instructions_with_skills_index` text helper
//! - `AgentType` serde round-trip

use aionui_ai_agent::IAgentConnector;
use aionui_ai_agent::manager::aionrs::AionrsAgentManager;
use aionui_ai_agent::types::AionrsResolvedConfig;
use aionui_ai_agent::{SkillIndex, build_system_instructions_with_skills_index};
use aionui_api_types::ConversationStatus;
use aionui_common::{AgentKillReason, AgentType};
use serde_json::json;

// ---------------------------------------------------------------------------
// Aionrs agent tests (real implementation with AgentEngine)
// ---------------------------------------------------------------------------

fn make_aionrs_config() -> AionrsResolvedConfig {
    AionrsResolvedConfig {
        provider: "anthropic".into(),
        api_key: "sk-test-key".into(),
        model: "claude-sonnet-4-20250514".into(),
        base_url: None,
        system_prompt: None,
        max_tokens: 4096,
        max_turns: None,
        compat_overrides: Default::default(),
        session_directory: std::env::temp_dir().join("aionrs-test-sessions"),
        session_mode: None,
        extra_mcp_servers: Default::default(),
        bedrock_config: None,
    }
}

#[tokio::test]
async fn aionrs_agent_kill_succeeds() {
    let agent = AionrsAgentManager::new("conv-1".into(), "/proj".into(), make_aionrs_config(), None)
        .await
        .unwrap();
    assert!(IAgentConnector::kill(&agent, None).is_ok());
    assert!(IAgentConnector::kill(&agent, Some(AgentKillReason::IdleTimeout)).is_ok());
}

#[tokio::test]
async fn aionrs_agent_confirm_succeeds() {
    let agent = AionrsAgentManager::new("conv-1".into(), "/proj".into(), make_aionrs_config(), None)
        .await
        .unwrap();
    // `confirm` is an inherent method on `AionrsAgentManager` (the
    // `IAgentConnector::confirm` trait method dispatches to it); the
    // test calls the inherent method directly to keep argument shape
    // identical to the production callers in the conv layer.
    let result = agent.confirm("msg", "call", json!({}), false);
    assert!(result.is_ok());
}

#[tokio::test]
async fn aionrs_agent_metadata() {
    let agent = AionrsAgentManager::new("conv-abc".into(), "/work".into(), make_aionrs_config(), None)
        .await
        .unwrap();
    assert_eq!(IAgentConnector::agent_type(&agent), AgentType::Aionrs);
    assert_eq!(IAgentConnector::workspace(&agent), "/work");
    assert_eq!(IAgentConnector::conversation_id(&agent), "conv-abc");
    assert_eq!(IAgentConnector::status(&agent), Some(ConversationStatus::Pending));
    assert!(agent.get_confirmations().is_empty());
    assert!(!agent.check_approval("any", None));
}

// ---------------------------------------------------------------------------
// Workspace browsing (uses real filesystem via tempdir)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn workspace_browse_reads_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let base = tmp.path();

    // Create test files and dirs
    std::fs::create_dir(base.join("src")).unwrap();
    std::fs::create_dir(base.join("tests")).unwrap();
    std::fs::write(base.join("Cargo.toml"), "# test").unwrap();
    std::fs::write(base.join("README.md"), "# readme").unwrap();

    let mut entries = Vec::new();
    let mut dir_reader = tokio::fs::read_dir(base).await.unwrap();
    while let Ok(Some(entry)) = dir_reader.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ft = entry.file_type().await.unwrap();
        let entry_type = if ft.is_dir() { "directory" } else { "file" };
        entries.push((name, entry_type.to_string()));
    }

    assert_eq!(entries.len(), 4);

    // Check that directories exist
    let dir_names: Vec<&str> = entries
        .iter()
        .filter(|(_, t)| t == "directory")
        .map(|(n, _)| n.as_str())
        .collect();
    assert!(dir_names.contains(&"src"));
    assert!(dir_names.contains(&"tests"));

    // Check that files exist
    let file_names: Vec<&str> = entries
        .iter()
        .filter(|(_, t)| t == "file")
        .map(|(n, _)| n.as_str())
        .collect();
    assert!(file_names.contains(&"Cargo.toml"));
    assert!(file_names.contains(&"README.md"));
}

// ---------------------------------------------------------------------------
// build_system_instructions_with_skills_index (M-16 fix)
// ---------------------------------------------------------------------------

#[test]
fn build_system_instructions_with_skills_index_empty() {
    let result = build_system_instructions_with_skills_index("Base prompt", &[]);
    assert_eq!(result, "Base prompt");
}

#[test]
fn build_system_instructions_with_skills_index_appends_index() {
    let skills = vec![
        SkillIndex {
            name: "review".into(),
            description: "Code review".into(),
        },
        SkillIndex {
            name: "debug".into(),
            description: "Debugging".into(),
        },
    ];
    let result = build_system_instructions_with_skills_index("You are an AI assistant.", &skills);
    assert!(result.starts_with("You are an AI assistant."));
    assert!(result.contains("## Available Skills"));
    assert!(result.contains("- **review**: Code review"));
    assert!(result.contains("- **debug**: Debugging"));
    assert!(result.contains("[LOAD_SKILL: skill-name]"));
}

// ---------------------------------------------------------------------------
// Agent type metadata validation
// ---------------------------------------------------------------------------

#[test]
fn agent_type_serde_all_variants() {
    // Verify that all AgentType variants serialize/deserialize correctly
    for (variant, expected_json) in [
        (AgentType::Acp, "\"acp\""),
        (AgentType::OpenclawGateway, "\"openclaw-gateway\""),
        (AgentType::Nanobot, "\"nanobot\""),
        (AgentType::Remote, "\"remote\""),
        (AgentType::Aionrs, "\"aionrs\""),
    ] {
        let json = serde_json::to_string(&variant).unwrap();
        assert_eq!(json, expected_json, "Failed for {variant:?}");
        let parsed: AgentType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, variant);
    }
}
