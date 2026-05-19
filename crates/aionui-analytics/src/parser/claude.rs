use super::{LogParser, for_each_json_line, parse_ts};
use crate::types::{Agent, ParseError, ParsedSession, UsageEvent};
use std::collections::HashMap;
use std::path::Path;

pub struct ClaudeParser;

impl LogParser for ClaudeParser {
    fn parse_file(&self, path: &Path) -> Result<ParsedSession, ParseError> {
        let mut events: Vec<UsageEvent> = Vec::new();
        let mut model_counts: HashMap<String, u64> = HashMap::new();
        let mut session_id = String::new();
        let mut project = String::new();
        let mut message_count: u64 = 0;

        for_each_json_line(path, |v| {
            if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
                return;
            }
            let ts = v.get("timestamp").and_then(|t| t.as_str()).and_then(parse_ts);
            let msg = v.get("message");
            let usage = msg.and_then(|m| m.get("usage"));
            let (ts, usage) = match (ts, usage) {
                (Some(ts), Some(u)) => (ts, u),
                _ => return,
            };
            if session_id.is_empty()
                && let Some(s) = v.get("sessionId").and_then(|s| s.as_str())
            {
                session_id = s.to_string();
            }
            if project.is_empty()
                && let Some(c) = v.get("cwd").and_then(|c| c.as_str())
            {
                project = c.to_string();
            }
            if let Some(m) = msg.and_then(|m| m.get("model")).and_then(|m| m.as_str()) {
                *model_counts.entry(m.to_string()).or_insert(0) += 1;
            }
            let g = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
            events.push(UsageEvent {
                at: ts,
                input_tokens: g("input_tokens"),
                output_tokens: g("output_tokens"),
                cache_read_tokens: g("cache_read_input_tokens"),
                cache_creation_tokens: g("cache_creation_input_tokens"),
            });
            message_count += 1;
        })?;

        if events.is_empty() {
            return Err(ParseError::Empty);
        }
        events.sort_by_key(|e| e.at);
        let model = model_counts
            .into_iter()
            .max_by_key(|(_, c)| *c)
            .map(|(m, _)| m)
            .unwrap_or_else(|| "unknown".to_string());
        let started_at = events.first().unwrap().at;
        let last_active_at = events.last().unwrap().at;

        Ok(ParsedSession {
            agent: Agent::Claude,
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
            model,
            started_at,
            last_active_at,
            events,
            message_count,
        })
    }
}
