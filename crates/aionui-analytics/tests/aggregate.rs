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
    let resp = aggregate(vec![session()], "day", "all", "agent", 200, 0);

    let codex = resp.summary.by_agent.iter().find(|a| a.agent == "codex").unwrap();
    assert_eq!(codex.total_tokens, 300);

    let mut sum = 0u64;
    for p in &resp.trend.points {
        sum += p.by_segment.get("codex").copied().unwrap_or(0);
    }
    assert_eq!(sum, 300, "趋势之和应等于汇总");
    assert_eq!(resp.trend.points.len(), 2);

    assert_eq!(resp.sessions_total, 1);
    assert_eq!(resp.sessions.len(), 1);
    assert_eq!(resp.sessions[0].total_tokens, 300);
}

#[test]
fn time_range_filters_events_by_event_time() {
    let resp = aggregate(vec![session()], "day", "all", "agent", 200, 0);
    assert_eq!(resp.trend.points[0].bucket, "2026-05-16");
}

#[test]
fn trend_splits_by_project_dimension() {
    // 两个 session，不同 project，同一天事件
    let s1 = ParsedSession {
        agent: Agent::Claude,
        session_id: "p1".into(),
        project: "/work/a".into(),
        model: "claude-opus-4-7".into(),
        started_at: Utc.with_ymd_and_hms(2026, 5, 20, 10, 0, 0).unwrap(),
        last_active_at: Utc.with_ymd_and_hms(2026, 5, 20, 11, 0, 0).unwrap(),
        events: vec![UsageEvent {
            at: Utc.with_ymd_and_hms(2026, 5, 20, 10, 0, 0).unwrap(),
            input_tokens: 150,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        }],
        message_count: 1,
    };
    let s2 = ParsedSession {
        agent: Agent::Claude,
        session_id: "p2".into(),
        project: "/work/b".into(),
        model: "claude-opus-4-7".into(),
        started_at: Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap(),
        last_active_at: Utc.with_ymd_and_hms(2026, 5, 20, 13, 0, 0).unwrap(),
        events: vec![UsageEvent {
            at: Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap(),
            input_tokens: 300,
            output_tokens: 100,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        }],
        message_count: 1,
    };

    let resp = aggregate(vec![s1, s2], "day", "all", "project", 200, 0);

    assert_eq!(resp.trend.points.len(), 1, "同一天应只有一个 bucket");
    let point = &resp.trend.points[0];
    assert_eq!(point.bucket, "2026-05-20");

    // /work/a: 150+50=200, /work/b: 300+100=400
    assert_eq!(
        point.by_segment.get("/work/a").copied().unwrap_or(0),
        200,
        "/work/a token 应为 200"
    );
    assert_eq!(
        point.by_segment.get("/work/b").copied().unwrap_or(0),
        400,
        "/work/b token 应为 400"
    );
    assert_eq!(point.by_segment.len(), 2, "应有两个 project 分段");

    // 分段之和等于 summary total
    let summary_total: u64 = resp.summary.by_agent.iter().map(|a| a.total_tokens).sum();
    let trend_total: u64 = point.by_segment.values().sum();
    assert_eq!(trend_total, summary_total, "趋势分段之和应等于 summary total");
}

#[test]
fn trend_splits_by_model_dimension() {
    // 两个 session，不同 model，同一天事件
    let s1 = ParsedSession {
        agent: Agent::Claude,
        session_id: "m1".into(),
        project: "/work/shared".into(),
        model: "claude-opus-4-7".into(),
        started_at: Utc.with_ymd_and_hms(2026, 5, 21, 8, 0, 0).unwrap(),
        last_active_at: Utc.with_ymd_and_hms(2026, 5, 21, 9, 0, 0).unwrap(),
        events: vec![UsageEvent {
            at: Utc.with_ymd_and_hms(2026, 5, 21, 8, 0, 0).unwrap(),
            input_tokens: 500,
            output_tokens: 100,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        }],
        message_count: 1,
    };
    let s2 = ParsedSession {
        agent: Agent::Claude,
        session_id: "m2".into(),
        project: "/work/shared".into(),
        model: "claude-sonnet-4-6".into(),
        started_at: Utc.with_ymd_and_hms(2026, 5, 21, 10, 0, 0).unwrap(),
        last_active_at: Utc.with_ymd_and_hms(2026, 5, 21, 11, 0, 0).unwrap(),
        events: vec![UsageEvent {
            at: Utc.with_ymd_and_hms(2026, 5, 21, 10, 0, 0).unwrap(),
            input_tokens: 200,
            output_tokens: 50,
            cache_read_tokens: 10,
            cache_creation_tokens: 0,
        }],
        message_count: 1,
    };

    let resp = aggregate(vec![s1, s2], "day", "all", "model", 200, 0);

    assert_eq!(resp.trend.points.len(), 1, "同一天应只有一个 bucket");
    let point = &resp.trend.points[0];
    assert_eq!(point.bucket, "2026-05-21");

    // claude-opus-4-7: 500+100=600, claude-sonnet-4-6: 200+50+10=260
    assert_eq!(
        point.by_segment.get("claude-opus-4-7").copied().unwrap_or(0),
        600,
        "claude-opus-4-7 token 应为 600"
    );
    assert_eq!(
        point.by_segment.get("claude-sonnet-4-6").copied().unwrap_or(0),
        260,
        "claude-sonnet-4-6 token 应为 260"
    );
    assert_eq!(point.by_segment.len(), 2, "应有两个 model 分段");

    // 分段之和等于 summary total
    let summary_total: u64 = resp.summary.by_agent.iter().map(|a| a.total_tokens).sum();
    let trend_total: u64 = point.by_segment.values().sum();
    assert_eq!(trend_total, summary_total, "趋势分段之和应等于 summary total");
}
