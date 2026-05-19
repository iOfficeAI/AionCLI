use aionui_analytics::service::{AgentUsageService, UsageRequest};

#[tokio::test]
async fn missing_dirs_degrade_not_error() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = AgentUsageService::with_home(tmp.path().join("nohome"));
    let resp = svc
        .build(UsageRequest {
            trend_granularity: "day".into(),
            time_range: "all".into(),
            refresh: false,
            sessions_limit: 200,
            sessions_offset: 0,
            is_remote: false,
        })
        .await
        .expect("must not error");
    assert_eq!(resp.sources.len(), 2);
    assert!(resp.sources.iter().all(|s| !s.available));
    assert_eq!(resp.sessions_total, 0);
}

#[tokio::test]
async fn remote_request_sanitizes_project_path() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let cdir = home.join(".claude/projects/encoded");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(
        cdir.join("sess.jsonl"),
        r#"{"type":"assistant","timestamp":"2026-05-17T10:00:00.000Z","cwd":"/Users/secret/proj","sessionId":"s1","message":{"model":"claude-opus-4-7","role":"assistant","usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
    )
    .unwrap();

    let svc = AgentUsageService::with_home(home.to_path_buf());
    let resp = svc
        .build(UsageRequest {
            trend_granularity: "day".into(),
            time_range: "all".into(),
            refresh: false,
            sessions_limit: 200,
            sessions_offset: 0,
            is_remote: true,
        })
        .await
        .unwrap();
    assert_eq!(resp.sessions.len(), 1);
    assert_eq!(resp.sessions[0].project, "proj");
    assert!(!resp.sessions[0].project.contains("secret"));
}

#[tokio::test]
async fn remote_sanitizes_both_claude_and_codex_projects() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let cdir = home.join(".claude/projects/encoded");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(
        cdir.join("claude.jsonl"),
        r#"{"type":"assistant","timestamp":"2026-05-17T10:00:00.000Z","cwd":"/Users/secret/claude-proj","sessionId":"c1","message":{"model":"claude-opus-4-7","role":"assistant","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
    ).unwrap();

    let ddir = home.join(".codex/sessions");
    std::fs::create_dir_all(&ddir).unwrap();
    std::fs::write(
        ddir.join("codex.jsonl"),
        "{\"timestamp\":\"2026-05-17T08:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"d1\",\"timestamp\":\"2026-05-17T08:00:00.000Z\",\"cwd\":\"/Users/secret/codex-proj\",\"cli_version\":\"0.1\",\"model_provider\":\"crs\"}}\n{\"timestamp\":\"2026-05-17T08:01:00.000Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_output_tokens\":0,\"total_tokens\":15},\"last_token_usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_output_tokens\":0,\"total_tokens\":15}}}}",
    ).unwrap();

    let svc = AgentUsageService::with_home(home.to_path_buf());
    let resp = svc
        .build(UsageRequest {
            trend_granularity: "day".into(),
            time_range: "all".into(),
            refresh: false,
            sessions_limit: 200,
            sessions_offset: 0,
            is_remote: true,
        })
        .await
        .unwrap();

    assert_eq!(resp.sessions.len(), 2);
    for s in &resp.sessions {
        assert!(
            !s.project.contains("secret"),
            "project {:?} still contains 'secret'",
            s.project
        );
    }
    let projects: Vec<&str> = resp.sessions.iter().map(|s| s.project.as_str()).collect();
    assert!(projects.contains(&"claude-proj"));
    assert!(projects.contains(&"codex-proj"));
}
