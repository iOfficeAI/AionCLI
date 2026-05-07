//! Cross-platform cache directory resolution for the bundled bun runtime.

use std::path::PathBuf;

/// Returns the root cache directory used for all aionui runtime artifacts.
///
/// Platform mapping (via `dirs::cache_dir()`):
/// - macOS:   `~/Library/Caches/aionui/runtime`
/// - Linux:   `$XDG_CACHE_HOME/aionui/runtime` (fallback `~/.cache/aionui/runtime`)
/// - Windows: `%LOCALAPPDATA%\aionui\runtime`
///
/// Returns `None` if no cache directory can be determined (exotic envs).
pub fn runtime_root() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("aionui").join("runtime"))
}

/// Per-version cache directory name: `bun-<version>-<sha12>`.
///
/// `sha12` is the first 12 hex chars of the bun binary sha256 — embedding
/// it means version bumps and content-level bumps both produce a new dir
/// so stale bytes never shadow a new build.
pub fn bun_dir_name(version: &str, sha256: &str) -> String {
    let sha12 = &sha256[..12.min(sha256.len())];
    format!("bun-{version}-{sha12}")
}

/// Full path for a specific (version, sha) cache directory.
pub fn bun_dir(version: &str, sha256: &str) -> Option<PathBuf> {
    runtime_root().map(|root| root.join(bun_dir_name(version, sha256)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bun_dir_name_format() {
        assert_eq!(bun_dir_name("1.1.38", "abc1234567890def"), "bun-1.1.38-abc123456789");
    }

    #[test]
    fn bun_dir_name_short_sha_does_not_panic() {
        // Defensive: if upstream ever passes <12 chars, don't panic.
        assert_eq!(bun_dir_name("1.0", "abc"), "bun-1.0-abc");
    }

    #[test]
    fn runtime_root_ends_with_expected_suffix() {
        let root = runtime_root().expect("cache dir available in test env");
        let tail: Vec<_> = root
            .components()
            .rev()
            .take(2)
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert_eq!(tail, vec!["runtime".to_string(), "aionui".to_string()]);
    }

    #[test]
    fn bun_dir_embeds_version_and_sha() {
        let dir = bun_dir("1.1.38", "deadbeefcafebabe").expect("cache available");
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name, "bun-1.1.38-deadbeefcafe");
    }
}
