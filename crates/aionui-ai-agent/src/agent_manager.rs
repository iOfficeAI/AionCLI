//! Helpers shared across agent managers.
//!
//! Historically this module defined the fat `IAgentManager` trait, its
//! `Arc<dyn IAgentManager>` handle alias, and the `as_any` downcast hook.
//! Those are gone (see PR #8): the public agent surface is now the ten-
//! method `IAgentTask` trait in [`crate::agent_task`], and type-specific
//! operations are dispatched through the [`crate::agent_task::AgentInstance`]
//! enum. What remains here is the small free function every concrete
//! manager uses to build the session-level approval-memory key.

/// Build the approval memory key from action and optional command_type.
///
/// Used by agent implementations to key their session-level approval memory
/// when handling `always_allow` confirmations.
pub fn approval_key(action: Option<&str>, command_type: Option<&str>) -> String {
    match (action, command_type) {
        (Some(a), Some(ct)) => format!("{a}:{ct}"),
        (Some(a), None) => a.to_owned(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_key_formats_matches_previous_contract() {
        assert_eq!(approval_key(Some("exec"), Some("curl")), "exec:curl");
        assert_eq!(approval_key(Some("exec"), None), "exec");
        assert_eq!(approval_key(None, Some("curl")), "");
        assert_eq!(approval_key(None, None), "");
    }
}
