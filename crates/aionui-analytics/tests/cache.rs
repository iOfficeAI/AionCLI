//! UsageCache unit tests — verifies the mtime+size cache-hit / bypass logic
//! (cache.rs get_or_parse), which the service tests only exercise indirectly.

use aionui_analytics::cache::UsageCache;
use aionui_analytics::parser::LogParser;
use aionui_analytics::types::{Agent, ParseError, ParsedSession};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Parser stub that counts how many times parse_file actually ran.
struct CountingParser {
    calls: AtomicUsize,
}

impl LogParser for CountingParser {
    fn parse_file(&self, _path: &Path) -> Result<ParsedSession, ParseError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ParsedSession {
            agent: Agent::Claude,
            session_id: "s".into(),
            project: "/p".into(),
            model: "m".into(),
            started_at: chrono::Utc::now(),
            last_active_at: chrono::Utc::now(),
            events: vec![],
            message_count: 0,
        })
    }
}

#[test]
fn second_call_hits_cache_without_reparsing() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.jsonl");
    std::fs::write(&f, "x").unwrap();
    let cache = UsageCache::new();
    let parser = CountingParser {
        calls: AtomicUsize::new(0),
    };

    cache.get_or_parse(&f, &parser, false).unwrap();
    assert_eq!(parser.calls.load(Ordering::SeqCst), 1, "首次必须解析");

    // Same file, unchanged mtime+size → cache hit, parser NOT called again.
    cache.get_or_parse(&f, &parser, false).unwrap();
    assert_eq!(parser.calls.load(Ordering::SeqCst), 1, "命中缓存不应重新解析");
}

#[test]
fn bypass_forces_reparse_even_when_cached() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("b.jsonl");
    std::fs::write(&f, "x").unwrap();
    let cache = UsageCache::new();
    let parser = CountingParser {
        calls: AtomicUsize::new(0),
    };

    cache.get_or_parse(&f, &parser, false).unwrap();
    // bypass=true → ignore cache, reparse, but still write back.
    cache.get_or_parse(&f, &parser, true).unwrap();
    assert_eq!(parser.calls.load(Ordering::SeqCst), 2, "bypass 必须强制重新解析");
}

#[test]
fn changed_file_invalidates_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("c.jsonl");
    std::fs::write(&f, "x").unwrap();
    let cache = UsageCache::new();
    let parser = CountingParser {
        calls: AtomicUsize::new(0),
    };

    cache.get_or_parse(&f, &parser, false).unwrap();
    // Change size (and mtime) → cache entry stale → reparse.
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(&f, "xxxxxx").unwrap();
    cache.get_or_parse(&f, &parser, false).unwrap();
    assert_eq!(parser.calls.load(Ordering::SeqCst), 2, "文件变化应使缓存失效并重新解析");
}

#[test]
fn missing_file_returns_io_error() {
    let cache = UsageCache::new();
    let parser = CountingParser {
        calls: AtomicUsize::new(0),
    };
    let r = cache.get_or_parse(Path::new("/no/such/file.jsonl"), &parser, false);
    assert!(matches!(r, Err(ParseError::Io(_))), "不存在的文件应返回 Io 错误");
}
