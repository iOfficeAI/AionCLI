pub const TEAM_CAPABLE_BACKENDS: &[&str] = &["claude", "codex", "gemini", "aionrs", "codebuddy"];

pub fn is_team_capable_backend(backend: &str, mcp_stdio_capable: bool) -> bool {
    TEAM_CAPABLE_BACKENDS.contains(&backend) || mcp_stdio_capable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_backend_is_capable_regardless_of_mcp_flag() {
        assert!(is_team_capable_backend("claude", false));
        assert!(is_team_capable_backend("claude", true));
        assert!(is_team_capable_backend("codex", false));
        assert!(is_team_capable_backend("gemini", false));
        assert!(is_team_capable_backend("aionrs", false));
    }

    #[test]
    fn non_whitelist_backend_with_mcp_stdio_is_capable() {
        assert!(is_team_capable_backend("custom", true));
        assert!(is_team_capable_backend("unknown-backend", true));
    }

    #[test]
    fn non_whitelist_backend_without_mcp_stdio_is_not_capable() {
        assert!(!is_team_capable_backend("custom", false));
        assert!(!is_team_capable_backend("", false));
        assert!(!is_team_capable_backend("Claude", false));
    }
}
