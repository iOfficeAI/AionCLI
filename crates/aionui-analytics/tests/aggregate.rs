use aionui_analytics::aggregate::aggregate;
use aionui_analytics::types::{Agent, ParsedSession, UsageEvent};
use chrono::{TimeZone, Utc};

fn ev(day: u32, tok: u64) -> UsageEvent {
    UsageEvent {
        at: Utc.with_ymd_and_hms(2026, 5, day, 12, 0, 0).unwrap(),
        input_tokens: tok,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
    }
}

fn session() -> ParsedSession {
    ParsedSession {
        agent: Agent::Codex,
        session_id: "s1".into(),
        project: "/work/p".into(),
        model: "gpt-5.1-codex".into(),
        started_at: Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap(),
        last_active_at: Utc.with_ymd_and_hms(2026, 5, 17, 12, 0, 0).unwrap(),
        events: vec![ev(16, 100), ev(17, 200)],
        message_count: 2,
    }
}

#[test]
fn cross_day_session_split_into_daily_buckets() {
    let resp = aggregate(vec![session()], "day", "all", 200, 0);

    let codex = resp.summary.by_agent.iter().find(|a| a.agent == "codex").unwrap();
    assert_eq!(codex.total_tokens, 300);

    let mut sum = 0u64;
    for p in &resp.trend.points {
        sum += p.by_agent.get("codex").copied().unwrap_or(0);
    }
    assert_eq!(sum, 300, "趋势之和应等于汇总");
    assert_eq!(resp.trend.points.len(), 2);

    assert_eq!(resp.sessions_total, 1);
    assert_eq!(resp.sessions.len(), 1);
    assert_eq!(resp.sessions[0].total_tokens, 300);
}

#[test]
fn time_range_filters_events_by_event_time() {
    let resp = aggregate(vec![session()], "day", "all", 200, 0);
    assert_eq!(resp.trend.points[0].bucket, "2026-05-16");
}
