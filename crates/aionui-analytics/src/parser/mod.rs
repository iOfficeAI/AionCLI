pub mod claude;
pub mod codex;

use crate::types::{ParseError, ParsedSession};
use std::path::Path;

pub trait LogParser {
    fn parse_file(&self, path: &Path) -> Result<ParsedSession, ParseError>;
}

/// 读取 jsonl 文件, 对每个非空行调用 `f`; 解析失败的行被静默跳过。
pub(crate) fn for_each_json_line<F>(path: &Path, mut f: F) -> Result<(), std::io::Error>
where
    F: FnMut(&serde_json::Value),
{
    use std::io::BufRead;
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            f(&v);
        }
    }
    Ok(())
}

/// 解析 RFC3339 时间戳; 失败返回 None。
pub(crate) fn parse_ts(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}
