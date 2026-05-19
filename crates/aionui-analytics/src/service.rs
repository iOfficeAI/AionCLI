use crate::aggregate::aggregate;
use crate::cache::UsageCache;
use crate::parser::{LogParser, claude::ClaudeParser, codex::CodexParser};
use crate::types::{Agent, ParseError, ParsedSession};
use aionui_api_types::{AgentUsageResponse, UsageSourceStatus};
use aionui_common::AppError;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct UsageRequest {
    pub trend_granularity: String,
    pub trend_dimension: String,
    pub time_range: String,
    pub refresh: bool,
    pub sessions_limit: u32,
    pub sessions_offset: u32,
    pub is_remote: bool,
}

#[derive(Clone)]
pub struct AgentUsageService {
    home: PathBuf,
    cache: Arc<UsageCache>,
}

impl AgentUsageService {
    pub fn new() -> Self {
        Self::with_home(dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")))
    }

    pub fn with_home(home: PathBuf) -> Self {
        Self {
            home,
            cache: Arc::new(UsageCache::new()),
        }
    }

    pub async fn build(&self, req: UsageRequest) -> Result<AgentUsageResponse, AppError> {
        let cutoff = time_range_cutoff(&req.time_range);

        let (claude_sessions, claude_status) =
            self.scan(Agent::Claude, &self.claude_dir(), &ClaudeParser, req.refresh, cutoff);
        let (codex_sessions, codex_status) =
            self.scan(Agent::Codex, &self.codex_dir(), &CodexParser, req.refresh, cutoff);

        let mut all: Vec<ParsedSession> = Vec::new();
        all.extend(claude_sessions);
        all.extend(codex_sessions);

        if req.is_remote {
            for s in &mut all {
                s.project = sanitize_project(&s.project);
            }
        }

        let mut resp = aggregate(
            all,
            &req.trend_granularity,
            &req.time_range,
            &req.trend_dimension,
            req.sessions_limit.clamp(1, 1000),
            req.sessions_offset,
        );
        resp.sources = vec![claude_status, codex_status];
        Ok(resp)
    }

    fn claude_dir(&self) -> PathBuf {
        self.home.join(".claude/projects")
    }

    fn codex_dir(&self) -> PathBuf {
        self.home.join(".codex/sessions")
    }

    fn scan(
        &self,
        agent: Agent,
        dir: &Path,
        parser: &dyn LogParser,
        refresh: bool,
        cutoff: Option<chrono::DateTime<chrono::Utc>>,
    ) -> (Vec<ParsedSession>, UsageSourceStatus) {
        let mut status = UsageSourceStatus {
            agent: agent.as_str().to_string(),
            files_total: 0,
            files_parsed: 0,
            files_skipped: 0,
            available: std::fs::read_dir(dir).is_ok(),
            error: None,
        };
        if std::fs::read_dir(dir).is_err() {
            return (Vec::new(), status);
        }
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            // 文件级 mtime 粗筛: cutoff 之前且 mtime 早于 cutoff 直接跳过。
            if let Some(c) = cutoff
                && let Ok(meta) = std::fs::metadata(path)
                && let Ok(mt) = meta.modified()
                && Into::<chrono::DateTime<chrono::Utc>>::into(mt) < c
            {
                continue;
            }
            status.files_total += 1;
            match self.cache.get_or_parse(path, parser, refresh) {
                Ok(session) => {
                    let mut s = (*session).clone();
                    // 事件级精筛。
                    if let Some(c) = cutoff {
                        s.events.retain(|e| e.at >= c);
                        if s.events.is_empty() {
                            status.files_parsed += 1;
                            continue;
                        }
                        s.last_active_at = s.events.last().unwrap().at;
                    }
                    status.files_parsed += 1;
                    out.push(s);
                }
                Err(ParseError::Empty) => status.files_skipped += 1,
                Err(ParseError::Io(_)) => status.files_skipped += 1,
            }
        }
        (out, status)
    }
}

impl Default for AgentUsageService {
    fn default() -> Self {
        Self::new()
    }
}

fn time_range_cutoff(tr: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let days = match tr {
        "7d" => 7,
        "90d" => 90,
        "all" => return None,
        _ => 30, // 默认 30d
    };
    Some(chrono::Utc::now() - chrono::Duration::days(days))
}

fn sanitize_project(p: &str) -> String {
    Path::new(p)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}
