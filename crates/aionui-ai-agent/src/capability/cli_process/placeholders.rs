//! `${NAME}` placeholder dictionary for ACP agent spawn.
//!
//! Builds a strict, deterministic `HashMap<String,String>` that the runtime
//! Builder consumes via `expand_placeholders`. Adding a new placeholder is a
//! deliberate change here — DB rows that reference an unknown placeholder
//! will fail to spawn.

use std::collections::HashMap;
use std::path::Path;

/// Whitelist of characters allowed in the per-agent directory name. Strips
/// path-separators and other surprising bytes from `binary_name` before it
/// is concatenated with `data_dir` — mitigates a malformed DB row turning
/// into a path-traversal during spawn.
fn sanitize_segment(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// Build the placeholder map for an ACP agent spawn.
///
/// `binary_name` is the agent's display-stable short name (e.g. "claude")
/// taken from `agent_metadata.agent_source_info.binary_name`. `agent_id`
/// is the row's primary key — appended to disambiguate hub-installed
/// agents that may share a binary name in the future.
pub fn placeholder_env(data_dir: &Path, binary_name: &str, agent_id: &str) -> HashMap<String, String> {
    let bin = sanitize_segment(binary_name);
    let id = sanitize_segment(agent_id);
    let prefix = data_dir.join("agents").join("npx").join(format!("{bin}-{id}"));
    let cache = data_dir.join("agents").join("npx").join("_npm_cache");
    let mut env = HashMap::new();
    env.insert("AGENT_PREFIX".into(), prefix.to_string_lossy().into_owned());
    env.insert("AGENT_NPM_CACHE".into(), cache.to_string_lossy().into_owned());
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn placeholder_env_uses_binary_name_and_agent_id() {
        let env = placeholder_env(&PathBuf::from("/data"), "claude", "2d23ff1c");
        assert_eq!(env["AGENT_PREFIX"], "/data/agents/npx/claude-2d23ff1c");
        assert_eq!(env["AGENT_NPM_CACHE"], "/data/agents/npx/_npm_cache");
    }

    #[test]
    fn placeholder_env_normalizes_unsafe_chars_in_binary_name() {
        let env = placeholder_env(&PathBuf::from("/data"), "../evil", "id1");
        assert!(
            !env["AGENT_PREFIX"].contains(".."),
            "must not preserve `..` segments: {}",
            env["AGENT_PREFIX"]
        );
        assert!(
            !env["AGENT_PREFIX"].contains('/') || env["AGENT_PREFIX"].starts_with('/'),
            "no embedded slashes from binary_name"
        );
    }

    #[test]
    fn placeholder_env_accepts_dashes_and_underscores() {
        let env = placeholder_env(&PathBuf::from("/data"), "kiro-cli-chat", "e044_000d");
        assert_eq!(env["AGENT_PREFIX"], "/data/agents/npx/kiro-cli-chat-e044_000d");
    }
}
