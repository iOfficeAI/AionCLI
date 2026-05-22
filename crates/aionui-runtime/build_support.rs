//! Pure helpers for `build.rs`. Kept separate so they can be unit-tested
//! without running the build script itself.
//!
//! Exposed to `build.rs` via `#[path = "build_support.rs"] mod build_support;`.

/// Whether this target has an embedded node asset upstream.
pub fn has_embedded_node(target: &str) -> bool {
    target != "aarch64-pc-windows-msvc"
}

/// Map a Rust target triple to the upstream node release asset filename
/// template. The literal substring `{ver}` is replaced by the caller —
/// keeping it inert here means the helper stays testable without an env.
pub fn node_asset_for_target(target: &str) -> Option<&'static str> {
    Some(match target {
        "x86_64-apple-darwin" => "node-v{ver}-darwin-x64.tar.xz",
        "aarch64-apple-darwin" => "node-v{ver}-darwin-arm64.tar.xz",
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => "node-v{ver}-linux-x64.tar.xz",
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => "node-v{ver}-linux-arm64.tar.xz",
        "x86_64-pc-windows-msvc" | "x86_64-pc-windows-gnu" => "node-v{ver}-win-x64.zip",
        _ => return None,
    })
}

/// Compose the official nodejs.org download URL for `version` + `asset`.
/// Caller is expected to have already substituted `{ver}` in `asset`.
pub fn node_download_url(version: &str, asset: &str) -> String {
    format!("https://nodejs.org/dist/v{version}/{asset}")
}

/// Per-version cache directory name: `node-<version>-<sha12>`.
pub fn node_dir_name(version: &str, sha256: &str) -> String {
    let sha12 = &sha256[..12.min(sha256.len())];
    format!("node-{version}-{sha12}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_asset_for_known_targets() {
        assert_eq!(
            node_asset_for_target("aarch64-apple-darwin"),
            Some("node-v{ver}-darwin-arm64.tar.xz")
        );
        assert_eq!(
            node_asset_for_target("x86_64-apple-darwin"),
            Some("node-v{ver}-darwin-x64.tar.xz")
        );
        assert_eq!(
            node_asset_for_target("x86_64-unknown-linux-gnu"),
            Some("node-v{ver}-linux-x64.tar.xz")
        );
        assert_eq!(
            node_asset_for_target("aarch64-unknown-linux-gnu"),
            Some("node-v{ver}-linux-arm64.tar.xz")
        );
        assert_eq!(
            node_asset_for_target("x86_64-pc-windows-msvc"),
            Some("node-v{ver}-win-x64.zip")
        );
        assert_eq!(node_asset_for_target("aarch64-pc-windows-msvc"), None);
    }

    #[test]
    fn has_embedded_node_false_only_for_win_arm64() {
        assert!(has_embedded_node("x86_64-apple-darwin"));
        assert!(has_embedded_node("aarch64-apple-darwin"));
        assert!(has_embedded_node("x86_64-unknown-linux-gnu"));
        assert!(has_embedded_node("aarch64-unknown-linux-gnu"));
        assert!(has_embedded_node("x86_64-pc-windows-msvc"));
        assert!(!has_embedded_node("aarch64-pc-windows-msvc"));
    }

    #[test]
    fn node_download_url_format() {
        let url = node_download_url("22.11.0", "node-v22.11.0-darwin-arm64.tar.xz");
        assert_eq!(
            url,
            "https://nodejs.org/dist/v22.11.0/node-v22.11.0-darwin-arm64.tar.xz"
        );
    }

    #[test]
    fn node_dir_name_format() {
        assert_eq!(
            node_dir_name("22.11.0", "abc1234567890def"),
            "node-22.11.0-abc123456789"
        );
    }

    #[test]
    fn node_dir_name_short_sha_does_not_panic() {
        assert_eq!(node_dir_name("1.0", "abc"), "node-1.0-abc");
    }
}
