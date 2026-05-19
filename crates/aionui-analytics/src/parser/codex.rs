use super::{LogParser, for_each_json_line, parse_ts};
use crate::types::{Agent, ParseError, ParsedSession, UsageEvent};
use std::path::Path;

pub struct CodexParser;

impl LogParser for CodexParser {
    fn parse_file(&self, path: &Path) -> Result<ParsedSession, ParseError> {
        let mut events: Vec<UsageEvent> = Vec::new();
        let mut session_id = String::new();
        let mut project = String::new();
        let mut model = String::new();
        let mut started_at: Option<chrono::DateTime<chrono::Utc>> = None;
        let mut message_count: u64 = 0;
        // (input, output, cached) 最后一个 total_token_usage, 用于 fallback。
        let mut last_total: Option<(u64, u64, u64)> = None;

        for_each_json_line(path, |v| {
            let line_ts = v.get("timestamp").and_then(|t| t.as_str()).and_then(parse_ts);
            let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let payload = v.get("payload");

            match kind {
                "session_meta" => {
                    if let Some(p) = payload {
                        if let Some(s) = p.get("id").and_then(|s| s.as_str()) {
                            session_id = s.to_string();
                        }
                        if let Some(c) = p.get("cwd").and_then(|c| c.as_str()) {
                            project = c.to_string();
                        }
                        if let Some(t) = p.get("timestamp").and_then(|t| t.as_str()).and_then(parse_ts) {
                            started_at.get_or_insert(t);
                        }
                    }
                }
                "turn_context" => {
                    if model.is_empty()
                        && let Some(m) = payload.and_then(|p| p.get("model")).and_then(|m| m.as_str())
                    {
                        model = m.to_string();
                    }
                }
                "response_item" => {
                    let is_assistant_msg = payload
                        .map(|p| {
                            p.get("type").and_then(|t| t.as_str()) == Some("message")
                                && p.get("role").and_then(|r| r.as_str()) == Some("assistant")
                        })
                        .unwrap_or(false);
                    if is_assistant_msg {
                        message_count += 1;
                    }
                }
                "event_msg" => {
                    let is_tc = payload
                        .map(|p| p.get("type").and_then(|t| t.as_str()) == Some("token_count"))
                        .unwrap_or(false);
                    if !is_tc {
                        return;
                    }
                    let info = payload.and_then(|p| p.get("info"));
                    // 记录最后一个 total_token_usage 作为 fallback 兜底 (回应 review P2)。
                    if let Some(total) = info.and_then(|i| i.get("total_token_usage")) {
                        let g = |k: &str| total.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
                        last_total = Some((g("input_tokens"), g("output_tokens"), g("cached_input_tokens")));
                    }
                    let last = info.and_then(|i| i.get("last_token_usage"));
                    // 事件级: 需要 last_token_usage + 行级时间; 缺任一则不记事件,
                    // 由文件末尾的 fallback 兜底, 不静默丢弃整段用量。
                    let (last, at) = match (last, line_ts) {
                        (Some(l), Some(at)) => (l, at),
                        _ => return,
                    };
                    let g = |k: &str| last.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
                    events.push(UsageEvent {
                        at,
                        input_tokens: g("input_tokens"),
                        output_tokens: g("output_tokens"),
                        cache_read_tokens: g("cached_input_tokens"),
                        cache_creation_tokens: 0,
                    });
                }
                _ => {}
            }
        })?;

        // Fallback (回应 review P2, 见 design.md:158): 若无任何事件级用量, 但存在
        // total_token_usage, 把整会话总量归到会话开始日 (无开始日则跳过该会话)。
        if events.is_empty() {
            match (last_total, started_at) {
                (Some((i, o, c)), Some(start)) if i + o + c > 0 => {
                    events.push(UsageEvent {
                        at: start,
                        input_tokens: i,
                        output_tokens: o,
                        cache_read_tokens: c,
                        cache_creation_tokens: 0,
                    });
                }
                _ => return Err(ParseError::Empty),
            }
        }
        events.sort_by_key(|e| e.at);
        let started_at = started_at.unwrap_or_else(|| events.first().unwrap().at);
        let last_active_at = events.last().unwrap().at;

        Ok(ParsedSession {
            agent: Agent::Codex,
            session_id: if session_id.is_empty() {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            } else {
                session_id
            },
            project: if project.is_empty() {
                "unknown".to_string()
            } else {
                project
            },
            model: if model.is_empty() { "unknown".to_string() } else { model },
            started_at,
            last_active_at,
            events,
            message_count,
        })
    }
}
