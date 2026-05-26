use aionui_analytics::parser::{LogParser, codex::CodexParser};
use aionui_analytics::types::Agent;

#[test]
fn parses_codex_session_event_level() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_sample.jsonl");
    let session = CodexParser.parse_file(&path).expect("should parse");

    assert_eq!(session.agent, Agent::Codex);
    assert_eq!(session.session_id, "sess-codex-1");
    assert_eq!(session.project, "/work/proj-b");
    assert_eq!(session.model, "gpt-5.1-codex");
    assert_eq!(session.events.len(), 2);
    assert_eq!(session.message_count, 2);
    assert_eq!(session.events[0].input_tokens, 1000);
    assert_eq!(session.events[0].cache_read_tokens, 200);
    assert_eq!(session.events[1].input_tokens, 500);
    assert!(session.events[0].at < session.events[1].at);
}

#[test]
fn codex_fallback_when_no_event_level_usage() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let path = dir.join("codex_fallback.jsonl");
    let session = CodexParser.parse_file(&path).expect("must fallback, not Empty");
    assert_eq!(session.agent, Agent::Codex);
    assert_eq!(session.events.len(), 1);
    let e = &session.events[0];
    assert_eq!(e.input_tokens, 700);
    assert_eq!(e.output_tokens, 40);
    assert_eq!(e.at, session.started_at);
}

#[test]
fn codex_truncated_log_without_session_meta_is_dropped() {
    // 文档化已知取舍 (review Important): 损坏/截断的 Codex 日志若缺 session_meta,
    // started_at 无从得知, 即便有 total_token_usage 也会被丢弃 (ParseError::Empty)。
    // 这是有意取舍 (损坏日志罕见); 此测试锁定该行为, 防止未来无意改变语义。
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_no_meta.jsonl");
    let result = CodexParser.parse_file(&path);
    assert!(result.is_err(), "无 session_meta 的截断日志当前被丢弃 (已知取舍)");
}
