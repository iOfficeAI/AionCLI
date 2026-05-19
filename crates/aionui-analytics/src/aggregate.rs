use crate::types::ParsedSession;
use aionui_api_types::{
    AgentUsageResponse, SessionRow, TrendPoint, UsageByAgent, UsageByModel, UsageSummary, UsageTrend,
};
use std::collections::BTreeMap;

/// 把 sessions 聚合为响应 (纯函数, 无 IO)。
/// `time_range` 仅写入响应回显; 事件级时间过滤由 service 在调用前完成。
pub fn aggregate(
    sessions: Vec<ParsedSession>,
    granularity: &str,
    time_range: &str,
    sessions_limit: u32,
    sessions_offset: u32,
) -> AgentUsageResponse {
    let gran = if granularity == "week" { "week" } else { "day" };

    #[derive(Default, Clone)]
    struct Acc {
        sessions: u64,
        messages: u64,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_creation: u64,
    }
    let mut by_agent: BTreeMap<&'static str, Acc> = BTreeMap::new();
    let mut by_model: BTreeMap<(&'static str, String), Acc> = BTreeMap::new();
    let mut trend: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();

    for s in &sessions {
        let an = s.agent.as_str();
        let a = by_agent.entry(an).or_default();
        a.sessions += 1;
        a.messages += s.message_count;
        let m = by_model.entry((an, s.model.clone())).or_default();
        m.sessions += 1;
        for e in &s.events {
            a.input += e.input_tokens;
            a.output += e.output_tokens;
            a.cache_read += e.cache_read_tokens;
            a.cache_creation += e.cache_creation_tokens;
            m.input += e.input_tokens;
            m.output += e.output_tokens;
            m.cache_read += e.cache_read_tokens;
            m.cache_creation += e.cache_creation_tokens;
            let bucket = bucket_key(e.at, gran);
            *trend.entry(bucket).or_default().entry(an.to_string()).or_insert(0) += e.total();
        }
    }

    let summary = UsageSummary {
        by_agent: by_agent
            .iter()
            .map(|(name, a)| UsageByAgent {
                agent: name.to_string(),
                sessions: a.sessions,
                messages: a.messages,
                input_tokens: a.input,
                output_tokens: a.output,
                cache_read_tokens: a.cache_read,
                cache_creation_tokens: a.cache_creation,
                total_tokens: a.input + a.output + a.cache_read + a.cache_creation,
            })
            .collect(),
    };

    let by_model_vec: Vec<UsageByModel> = by_model
        .iter()
        .map(|((agent, model), m)| UsageByModel {
            agent: agent.to_string(),
            model: model.clone(),
            sessions: m.sessions,
            input_tokens: m.input,
            output_tokens: m.output,
            cache_read_tokens: m.cache_read,
            cache_creation_tokens: m.cache_creation,
            total_tokens: m.input + m.output + m.cache_read + m.cache_creation,
        })
        .collect();

    let trend_points: Vec<TrendPoint> = trend
        .into_iter()
        .map(|(bucket, by_agent)| TrendPoint {
            bucket,
            by_agent: by_agent.into_iter().collect(),
        })
        .collect();

    let mut rows: Vec<&ParsedSession> = sessions.iter().collect();
    rows.sort_by_key(|b| std::cmp::Reverse(b.last_active_at));
    let sessions_total = rows.len() as u64;
    let page: Vec<SessionRow> = rows
        .into_iter()
        .skip(sessions_offset as usize)
        .take(sessions_limit as usize)
        .map(|s| SessionRow {
            agent: s.agent.as_str().to_string(),
            session_id: s.session_id.clone(),
            project: s.project.clone(),
            model: s.model.clone(),
            started_at: s.started_at.to_rfc3339(),
            last_active_at: s.last_active_at.to_rfc3339(),
            messages: s.message_count,
            total_tokens: s.events.iter().map(|e| e.total()).sum(),
        })
        .collect();

    AgentUsageResponse {
        scanned_at: chrono::Utc::now().to_rfc3339(),
        sources: Vec::new(),
        summary,
        by_model: by_model_vec,
        trend: UsageTrend {
            granularity: gran.to_string(),
            points: trend_points,
        },
        time_range: time_range.to_string(),
        sessions_total,
        sessions_limit,
        sessions_offset,
        sessions: page,
    }
}

fn bucket_key(at: chrono::DateTime<chrono::Utc>, gran: &str) -> String {
    use chrono::Datelike;
    if gran == "week" {
        let iso = at.iso_week();
        format!("{}-W{:02}", iso.year(), iso.week())
    } else {
        at.format("%Y-%m-%d").to_string()
    }
}
