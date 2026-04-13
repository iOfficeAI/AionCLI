use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::RwLock;

/// Duration for which a failed API key is frozen (blacklisted).
const BLACKLIST_DURATION: Duration = Duration::from_secs(90);

struct ApiKeyEntry {
    key: String,
    blacklisted_until: Option<Instant>,
}

impl ApiKeyEntry {
    fn is_available(&self) -> bool {
        match self.blacklisted_until {
            Some(until) => Instant::now() >= until,
            None => true,
        }
    }
}

/// Status snapshot of the key manager for diagnostic queries.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyStatus {
    pub auth_type: String,
    pub env_key: Option<String>,
    pub current: usize,
    pub total: usize,
    pub blacklisted: usize,
}

/// Manages a pool of API keys with rotation and blacklist support.
///
/// Keys are parsed from a comma- or newline-separated string.
/// A random initial key is selected. Failed keys are blacklisted
/// for 90 seconds. When a key is activated, the corresponding
/// environment variable is updated for child process inheritance.
pub struct ApiKeyManager {
    entries: RwLock<Vec<ApiKeyEntry>>,
    current_index: AtomicUsize,
    env_key_name: Option<String>,
}

impl ApiKeyManager {
    /// Create a new manager by parsing the given key string.
    ///
    /// `env_key_name` is the environment variable to sync
    /// (e.g. `"OPENAI_API_KEY"`).
    pub fn new(keys_str: &str, env_key_name: Option<String>) -> Self {
        let keys = parse_keys(keys_str);
        let initial_index = random_index(keys.len());

        let entries: Vec<ApiKeyEntry> = keys
            .into_iter()
            .map(|key| ApiKeyEntry {
                key,
                blacklisted_until: None,
            })
            .collect();

        // Sync initial key to env var
        if let Some(ref env_name) = env_key_name
            && let Some(entry) = entries.get(initial_index)
        {
            // SAFETY: We accept the inherent race-condition risk of
            // `set_var` for child-process environment inheritance.
            unsafe { std::env::set_var(env_name, &entry.key) };
        }

        Self {
            entries: RwLock::new(entries),
            current_index: AtomicUsize::new(initial_index),
            env_key_name,
        }
    }

    /// Total number of keys (including blacklisted).
    pub async fn total_keys(&self) -> usize {
        self.entries.read().await.len()
    }

    /// Get the next available (non-blacklisted) key, scanning forward
    /// from the current index. Returns `None` if all keys are blacklisted.
    pub async fn get_available_key(&self) -> Option<String> {
        let entries = self.entries.read().await;
        let total = entries.len();
        if total == 0 {
            return None;
        }

        let start = self.current_index.load(Ordering::Relaxed) % total;
        for offset in 0..total {
            let idx = (start + offset) % total;
            if entries[idx].is_available() {
                self.current_index.store(idx, Ordering::Relaxed);
                let key = entries[idx].key.clone();
                drop(entries);
                self.sync_env_var(&key);
                return Some(key);
            }
        }
        None
    }

    /// Rotate to the next available key (skipping the current one).
    /// Returns `None` if no other key is available.
    pub async fn rotate_key(&self) -> Option<String> {
        let entries = self.entries.read().await;
        let total = entries.len();
        if total <= 1 {
            return None;
        }

        let current = self.current_index.load(Ordering::Relaxed) % total;
        for offset in 1..total {
            let idx = (current + offset) % total;
            if entries[idx].is_available() {
                self.current_index.store(idx, Ordering::Relaxed);
                let key = entries[idx].key.clone();
                drop(entries);
                self.sync_env_var(&key);
                return Some(key);
            }
        }
        None
    }

    /// Blacklist the currently active key for [`BLACKLIST_DURATION`].
    pub async fn blacklist_current(&self) {
        let mut entries = self.entries.write().await;
        let total = entries.len();
        if total == 0 {
            return;
        }
        let idx = self.current_index.load(Ordering::Relaxed) % total;
        entries[idx].blacklisted_until = Some(Instant::now() + BLACKLIST_DURATION);
    }

    /// Return a diagnostic status snapshot.
    pub async fn get_status(&self, auth_type: &str) -> ApiKeyStatus {
        let entries = self.entries.read().await;
        let blacklisted = entries.iter().filter(|e| !e.is_available()).count();
        let total = entries.len();
        ApiKeyStatus {
            auth_type: auth_type.to_string(),
            env_key: self.env_key_name.clone(),
            current: self.current_index.load(Ordering::Relaxed) % total.max(1),
            total,
            blacklisted,
        }
    }

    fn sync_env_var(&self, key: &str) {
        if let Some(ref env_name) = self.env_key_name {
            // SAFETY: see comment in `new`.
            unsafe { std::env::set_var(env_name, key) };
        }
    }
}

/// Parse a key string into individual keys, splitting on comma or newline.
fn parse_keys(input: &str) -> Vec<String> {
    input
        .split([',', '\n'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Return a random index in `[0, max)`. Returns `0` if `max == 0`.
fn random_index(max: usize) -> usize {
    if max == 0 {
        return 0;
    }
    let mut buf = [0u8; 8];
    if getrandom::getrandom(&mut buf).is_ok() {
        usize::from_le_bytes(buf) % max
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_key() {
        let keys = parse_keys("sk-abc123");
        assert_eq!(keys, vec!["sk-abc123"]);
    }

    #[test]
    fn parse_comma_separated_keys() {
        let keys = parse_keys("sk-a, sk-b,sk-c");
        assert_eq!(keys, vec!["sk-a", "sk-b", "sk-c"]);
    }

    #[test]
    fn parse_newline_separated_keys() {
        let keys = parse_keys("sk-a\nsk-b\nsk-c");
        assert_eq!(keys, vec!["sk-a", "sk-b", "sk-c"]);
    }

    #[test]
    fn parse_mixed_separators() {
        let keys = parse_keys("sk-a,sk-b\nsk-c");
        assert_eq!(keys, vec!["sk-a", "sk-b", "sk-c"]);
    }

    #[test]
    fn parse_empty_input() {
        let keys = parse_keys("");
        assert!(keys.is_empty());
    }

    #[test]
    fn parse_trims_whitespace() {
        let keys = parse_keys("  sk-a , sk-b  ");
        assert_eq!(keys, vec!["sk-a", "sk-b"]);
    }

    #[test]
    fn parse_filters_empty_segments() {
        let keys = parse_keys("sk-a,,sk-b,  ,sk-c");
        assert_eq!(keys, vec!["sk-a", "sk-b", "sk-c"]);
    }

    #[test]
    fn random_index_zero_returns_zero() {
        assert_eq!(random_index(0), 0);
    }

    #[test]
    fn random_index_within_range() {
        for _ in 0..100 {
            let idx = random_index(5);
            assert!(idx < 5, "random_index(5) returned {idx}");
        }
    }

    #[tokio::test]
    async fn get_available_key_single() {
        let mgr = ApiKeyManager::new("sk-only", None);
        assert_eq!(mgr.get_available_key().await, Some("sk-only".into()));
    }

    #[tokio::test]
    async fn get_available_key_empty() {
        let mgr = ApiKeyManager::new("", None);
        assert_eq!(mgr.get_available_key().await, None);
    }

    #[tokio::test]
    async fn rotate_key_cycles_through_keys() {
        let mgr = ApiKeyManager::new("sk-a,sk-b,sk-c", None);
        let first = mgr.get_available_key().await.unwrap();
        let second = mgr.rotate_key().await.unwrap();
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn rotate_key_single_returns_none() {
        let mgr = ApiKeyManager::new("sk-only", None);
        let _ = mgr.get_available_key().await;
        assert_eq!(mgr.rotate_key().await, None);
    }

    #[tokio::test]
    async fn blacklist_and_rotate() {
        let mgr = ApiKeyManager::new("sk-a,sk-b", None);
        let _ = mgr.get_available_key().await;
        mgr.blacklist_current().await;
        let next = mgr.get_available_key().await.unwrap();
        // Should get the non-blacklisted key
        assert!(next == "sk-a" || next == "sk-b");
    }

    #[tokio::test]
    async fn all_keys_blacklisted_returns_none() {
        let mgr = ApiKeyManager::new("sk-a,sk-b", None);
        // Blacklist both keys
        let _ = mgr.get_available_key().await;
        mgr.blacklist_current().await;
        let _ = mgr.rotate_key().await;
        mgr.blacklist_current().await;
        // Now both are blacklisted
        assert_eq!(mgr.get_available_key().await, None);
    }

    #[tokio::test]
    async fn status_reflects_state() {
        let mgr = ApiKeyManager::new("sk-a,sk-b,sk-c", None);
        let status = mgr.get_status("USE_OPENAI").await;
        assert_eq!(status.total, 3);
        assert_eq!(status.blacklisted, 0);
        assert_eq!(status.auth_type, "USE_OPENAI");

        let _ = mgr.get_available_key().await;
        mgr.blacklist_current().await;
        let status = mgr.get_status("USE_OPENAI").await;
        assert_eq!(status.blacklisted, 1);
    }

    #[tokio::test]
    async fn total_keys_returns_count() {
        let mgr = ApiKeyManager::new("sk-a,sk-b", None);
        assert_eq!(mgr.total_keys().await, 2);
    }
}
