use serde::{Deserialize, Serialize};

/// Query for `GET /api/analytics/agent-usage`.
#[derive(Debug, Default, Deserialize)]
pub struct AgentUsageQuery {
    pub trend_granularity: Option<String>,
    /// 趋势维度：agent | project | model，默认 agent。
    pub trend_dimension: Option<String>,
    pub refresh: Option<bool>,
    pub time_range: Option<String>,
    pub sessions_limit: Option<u32>,
    pub sessions_offset: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct UsageSourceStatus {
    pub agent: String,
    pub files_total: u32,
    pub files_parsed: u32,
    pub files_skipped: u32,
    pub available: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UsageByAgent {
    pub agent: String,
    pub sessions: u64,
    pub messages: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Serialize)]
pub struct UsageSummary {
    pub by_agent: Vec<UsageByAgent>,
}

#[derive(Debug, Serialize)]
pub struct UsageByModel {
    pub agent: String,
    pub model: String,
    pub sessions: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Serialize)]
pub struct UsageByProject {
    pub agent: String,
    pub project: String,
    pub sessions: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Serialize, Default)]
pub struct TokenKindBreakdown {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

#[derive(Debug, Serialize)]
pub struct TrendPoint {
    pub bucket: String,
    /// 分段名 (agent/project/model) → 该桶 total_tokens。
    pub by_segment: std::collections::BTreeMap<String, u64>,
    /// 该桶按 token 类型的分层 (四项之和 == by_segment 所有值之和)。
    pub by_token_kind: TokenKindBreakdown,
}

#[derive(Debug, Serialize)]
pub struct UsageTrend {
    pub granularity: String,
    pub points: Vec<TrendPoint>,
}

#[derive(Debug, Serialize)]
pub struct SessionRow {
    pub agent: String,
    pub session_id: String,
    pub project: String,
    pub model: String,
    pub started_at: String,
    pub last_active_at: String,
    pub messages: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Serialize)]
pub struct AgentUsageResponse {
    pub scanned_at: String,
    pub sources: Vec<UsageSourceStatus>,
    pub summary: UsageSummary,
    pub by_model: Vec<UsageByModel>,
    pub by_project: Vec<UsageByProject>,
    pub trend: UsageTrend,
    pub time_range: String,
    pub sessions_total: u64,
    pub sessions_limit: u32,
    pub sessions_offset: u32,
    pub sessions: Vec<SessionRow>,
}
