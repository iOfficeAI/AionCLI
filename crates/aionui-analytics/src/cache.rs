use crate::parser::LogParser;
use crate::types::{ParseError, ParsedSession};
use dashmap::DashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
struct Entry {
    mtime: std::time::SystemTime,
    size: u64,
    session: Arc<ParsedSession>,
}

#[derive(Default)]
pub struct UsageCache {
    map: DashMap<PathBuf, Entry>,
}

impl UsageCache {
    pub fn new() -> Self {
        Self { map: DashMap::new() }
    }

    /// mtime+size 未变则返回缓存; 否则用 parser 重解析并更新。
    /// `bypass=true` 时忽略缓存但仍写回。
    pub fn get_or_parse(
        &self,
        path: &Path,
        parser: &dyn LogParser,
        bypass: bool,
    ) -> Result<Arc<ParsedSession>, ParseError> {
        let meta = std::fs::metadata(path)?;
        let mtime = meta.modified()?;
        let size = meta.len();

        if !bypass
            && let Some(e) = self.map.get(path)
            && e.mtime == mtime
            && e.size == size
        {
            return Ok(e.session.clone());
        }
        let session = Arc::new(parser.parse_file(path)?);
        self.map.insert(
            path.to_path_buf(),
            Entry {
                mtime,
                size,
                session: session.clone(),
            },
        );
        Ok(session)
    }
}
