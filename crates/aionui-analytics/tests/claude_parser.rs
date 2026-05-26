use aionui_analytics::parser::{LogParser, claude::ClaudeParser};
use aionui_analytics::types::Agent;

#[test]
fn parses_claude_session_with_usage_accumulation() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_sample.jsonl");
    let session = ClaudeParser.parse_file(&path).expect("should parse");

    assert_eq!(session.agent, Agent::Claude);
    assert_eq!(session.session_id, "sess-claude-1");
    assert_eq!(session.project, "/work/proj-a");
    assert_eq!(session.model, "claude-opus-4-7");
    assert_eq!(session.events.len(), 2);
    assert_eq!(session.message_count, 2);
    let total: u64 = session.events.iter().map(|e| e.total()).sum();
    assert_eq!(total, 665);
    assert!(session.events[0].at < session.events[1].at);
    assert_eq!(session.started_at.to_rfc3339(), "2026-05-16T10:00:05+00:00");
}
