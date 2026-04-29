use aionui_common::AppError;
use async_trait::async_trait;

/// Routes a single-chat message to the team runtime when the target
/// conversation belongs to a team.
///
/// Defined here (instead of in `aionui-team`) so `aionui-conversation` can
/// depend only on the trait and avoid a reverse dependency on the team crate.
#[async_trait]
pub trait ITeamMessageRouter: Send + Sync {
    /// Called by `ConversationService::send_message` after detecting that
    /// the conversation's `extra.team_id` is non-empty.
    ///
    /// Implementations resolve `conversation_id` back to a slot on the team
    /// session and forward the message to that agent.
    async fn route_agent_message(
        &self,
        conversation_id: &str,
        content: &str,
        silent: bool,
    ) -> Result<(), AppError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Compile-time check: `ITeamMessageRouter` must be object-safe so the
    // service can hold `Arc<dyn ITeamMessageRouter>`.
    #[allow(dead_code)]
    fn _assert_object_safe(_: Arc<dyn ITeamMessageRouter>) {}

    struct NoopRouter;

    #[async_trait]
    impl ITeamMessageRouter for NoopRouter {
        async fn route_agent_message(
            &self,
            _conversation_id: &str,
            _content: &str,
            _silent: bool,
        ) -> Result<(), AppError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn trait_is_object_safe_and_callable() {
        let router: Arc<dyn ITeamMessageRouter> = Arc::new(NoopRouter);
        router
            .route_agent_message("conv-1", "hello", false)
            .await
            .unwrap();
    }
}
