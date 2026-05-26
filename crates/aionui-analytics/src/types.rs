use serde::Serialize;

/// Agent 标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }
}

/// 单个用量事件 (Claude 一条 assistant 消息 / Codex 一个 token_count 事件)。
#[derive(Debug, Clone)]
pub struct UsageEvent {
    /// 事件 UTC 时间戳。
    pub at: chrono::DateTime<chrono::Utc>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

impl UsageEvent {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_creation_tokens
    }
}

/// 解析一个日志文件得到的会话。
#[derive(Debug, Clone)]
pub struct ParsedSession {
    pub agent: Agent,
    pub session_id: String,
    pub project: String,
    /// 会话级主要 model (出现最多者; 缺失为 "unknown")。
    pub model: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub last_active_at: chrono::DateTime<chrono::Utc>,
    /// 该会话所有用量事件 (已按时间升序)。
    pub events: Vec<UsageEvent>,
    /// 消息计数 (Claude: assistant 消息数; Codex: assistant role 的 response_item 数)。
    pub message_count: u64,
}

/// 解析失败原因 (用于上层计 skipped)。
#[derive(Debug)]
pub enum ParseError {
    Io(std::io::Error),
    /// 文件存在但无任何可识别内容。
    Empty,
}

impl From<std::io::Error> for ParseError {
    fn from(e: std::io::Error) -> Self {
        ParseError::Io(e)
    }
}
