//! Integration tests for the ACP prompt pipeline.
//!
//! Unlike acp_agent_integration.rs, these tests do not exercise
//! AcpAgentManager or the JSON-RPC protocol. They construct a
//! PromptPipeline with the two built-in hooks and invoke
//! pre_send against a real PromptCtx, asserting the observable
//! prompt transformation.

use std::collections::HashMap;
use std::sync::Arc;

use aionui_ai_agent::capability::prompt_pipeline::{PromptCtx, PromptPipeline};
use aionui_ai_agent::factory::acp_assembler::{AcpSessionParams, WorkspaceInfo, assemble_acp_params};
use aionui_ai_agent::manager::acp::{AcpSession, ModelIdentityReminderHook, SessionNewPreludeHook};
use aionui_ai_agent::registry::AgentRegistry;
use aionui_ai_agent::shared_kernel::ModelId;
use aionui_ai_agent::{AcpBuildExtra, AcpSkillManager, AgentRuntime};
use aionui_db::{SqliteAgentMetadataRepository, init_database_memory};

// ── Fixtures ──────────────────────────────────────────────────────────────────

async fn fixture_params(
    backend: &str,
    preset_context: Option<&str>,
    is_custom_workspace: bool,
) -> Arc<AcpSessionParams> {
    let db = init_database_memory().await.unwrap();
    let repo = Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone()));
    let registry = AgentRegistry::new(repo);
    registry.hydrate().await.unwrap();

    let metadata = registry
        .find_builtin_by_backend(backend)
        .await
        .expect("seeded backend row must exist");

    let config = AcpBuildExtra {
        agent_id: None,
        backend: Some(backend.to_owned()),
        cli_path: None,
        agent_name: None,
        custom_agent_id: None,
        preset_context: preset_context.map(str::to_owned),
        skills: vec![],
        preset_assistant_id: None,
        session_mode: None,
        cron_job_id: None,
        team_mcp_stdio_config: None,
        guide_mcp_config: None,
        user_id: None,
    };

    Arc::new(
        assemble_acp_params(
            "conv-pp-test".into(),
            WorkspaceInfo {
                path: "/tmp".into(),
                is_custom: is_custom_workspace,
            },
            metadata,
            aionui_common::CommandSpec {
                command: "/usr/bin/true".into(),
                args: vec![],
                env: vec![],
                cwd: None,
            },
            config,
            None,
            std::env::temp_dir(),
        )
        .await,
    )
}

fn fixture_skill_manager() -> Arc<AcpSkillManager> {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = Arc::new(aionui_extension::resolve_skill_paths(tmp.path(), tmp.path()));
    // tmp dir needs to live until the test finishes.
    // mem::forget is acceptable in test code — we just don't need the Drop cleanup.
    std::mem::forget(tmp);
    AcpSkillManager::new(paths)
}

fn fixture_runtime() -> AgentRuntime {
    AgentRuntime::new("conv-pp-test", "/tmp", 64)
}

fn make_pipeline() -> PromptPipeline {
    PromptPipeline::new(vec![
        Arc::new(SessionNewPreludeHook),
        Arc::new(ModelIdentityReminderHook),
    ])
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// First prompt after session/new: prelude block injected, flag consumed.
#[tokio::test(flavor = "current_thread")]
async fn brand_new_first_prompt_injects_preset_context() {
    let params = fixture_params("claude", Some("Rule A"), true).await;
    let skill_manager = fixture_skill_manager();
    let runtime = fixture_runtime();
    let mut session = AcpSession::new(None, None, HashMap::new());

    // Simulate: open_session_new just succeeded.
    session.mark_pending_session_new_prelude();

    let pipeline = make_pipeline();

    let mut ctx = PromptCtx {
        session: &mut session,
        params: &params,
        skill_manager: &skill_manager,
        runtime: &runtime,
    };

    let out = pipeline.pre_send(&mut ctx, "hello".into()).await;
    assert!(out.contains("[Assistant Rules]"), "prelude block missing: {out}");
    assert!(out.contains("Rule A"), "preset_context missing: {out}");
    assert!(out.ends_with("hello"), "user content should be at the end: {out}");

    // Flag must have been consumed.
    assert!(
        !session.take_pending_session_new_prelude(),
        "pending_session_new_prelude must be false after pre_send consumed it"
    );
}

/// Second prompt: no prelude, no reminder — pure passthrough.
#[tokio::test(flavor = "current_thread")]
async fn second_prompt_is_passthrough() {
    let params = fixture_params("claude", Some("Rule A"), true).await;
    let skill_manager = fixture_skill_manager();
    let runtime = fixture_runtime();
    let mut session = AcpSession::new(None, None, HashMap::new());
    session.mark_pending_session_new_prelude();

    let pipeline = make_pipeline();

    // First prompt consumes the flag.
    {
        let mut ctx = PromptCtx {
            session: &mut session,
            params: &params,
            skill_manager: &skill_manager,
            runtime: &runtime,
        };
        let _ = pipeline.pre_send(&mut ctx, "first".into()).await;
    }

    // Second prompt: flag already consumed.
    let mut ctx = PromptCtx {
        session: &mut session,
        params: &params,
        skill_manager: &skill_manager,
        runtime: &runtime,
    };
    let out = pipeline.pre_send(&mut ctx, "second".into()).await;
    assert_eq!(out, "second", "no prelude / no reminder expected on second turn");
}

/// Resume path: no mark_pending_session_new_prelude — prompt must be unchanged.
#[tokio::test(flavor = "current_thread")]
async fn resume_path_does_not_inject() {
    let params = fixture_params("claude", Some("Rule A"), true).await;
    let skill_manager = fixture_skill_manager();
    let runtime = fixture_runtime();

    // Resume: session opened by open_session_resume which does NOT call
    // mark_pending_session_new_prelude. The flag stays false.
    let mut session = AcpSession::new(None, None, HashMap::new());

    let pipeline = make_pipeline();

    let mut ctx = PromptCtx {
        session: &mut session,
        params: &params,
        skill_manager: &skill_manager,
        runtime: &runtime,
    };

    let out = pipeline.pre_send(&mut ctx, "continue the story".into()).await;
    assert_eq!(out, "continue the story");
}

/// Pending model notice: reminder prepended, then drained so second call is clean.
#[tokio::test(flavor = "current_thread")]
async fn pending_model_notice_triggers_reminder_prepend() {
    let params = fixture_params("claude", None, true).await;
    let skill_manager = fixture_skill_manager();
    let runtime = fixture_runtime();
    let mut session = AcpSession::new(None, None, HashMap::new());

    // Simulate set_model reconciled successfully and stuck the notice.
    session.set_pending_model_notice(ModelId::new("claude-opus-4"));

    let pipeline = make_pipeline();

    let mut ctx = PromptCtx {
        session: &mut session,
        params: &params,
        skill_manager: &skill_manager,
        runtime: &runtime,
    };

    let out = pipeline.pre_send(&mut ctx, "go".into()).await;
    assert!(out.contains("<system-reminder>"), "reminder missing: {out}");
    assert!(out.ends_with("go"), "user content must survive at the end: {out}");

    // Second call: notice already drained — no reminder.
    let mut ctx2 = PromptCtx {
        session: &mut session,
        params: &params,
        skill_manager: &skill_manager,
        runtime: &runtime,
    };
    let out2 = pipeline.pre_send(&mut ctx2, "next".into()).await;
    assert_eq!(out2, "next");
}

/// Both flags set: reminder (outermost) wraps the prelude block.
#[tokio::test(flavor = "current_thread")]
async fn both_flags_prepend_reminder_outermost() {
    let params = fixture_params("claude", Some("Rule A"), true).await;
    let skill_manager = fixture_skill_manager();
    let runtime = fixture_runtime();
    let mut session = AcpSession::new(None, None, HashMap::new());
    session.mark_pending_session_new_prelude();
    session.set_pending_model_notice(ModelId::new("claude-opus-4"));

    let pipeline = make_pipeline();

    let mut ctx = PromptCtx {
        session: &mut session,
        params: &params,
        skill_manager: &skill_manager,
        runtime: &runtime,
    };

    let out = pipeline.pre_send(&mut ctx, "hi".into()).await;
    let reminder_idx = out.find("<system-reminder>").expect("reminder must be present");
    let rules_idx = out.find("[Assistant Rules]").expect("rules block must be present");
    assert!(
        reminder_idx < rules_idx,
        "reminder must sit outside (before) the assistant rules block:\n{out}"
    );
    assert!(out.ends_with("hi"));
}

/// Skeleton: unlock once inject_first_message_prefix surfaces errors.
#[tokio::test(flavor = "current_thread")]
#[ignore = "SessionNewPreludeHook relies on inject_first_message_prefix which currently swallows I/O errors internally; unlocking this test requires surfacing a fallible boundary"]
async fn prelude_io_failure_emits_prompt_hook_warning() {
    // When inject_first_message_prefix exposes an error path, the hook
    // should call emit_hook_warning("session_new_prelude", ...) and
    // return the user content unchanged. Subscribers on runtime.subscribe()
    // must then receive an AgentStreamEvent::AcpPromptHookWarning whose
    // payload deserializes to AcpPromptHookWarningPayload with
    // hook == "session_new_prelude".
    let _ = fixture_params("claude", Some("ctx"), true).await;
}
