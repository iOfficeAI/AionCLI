use aionui_common::AppError;

#[derive(Debug, thiserror::Error)]
pub enum TeamError {
    #[error("Team not found: {0}")]
    TeamNotFound(String),

    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    #[error("Conversation not found: {0}")]
    ConversationNotFound(String),

    #[error("Task not found: {0}")]
    TaskNotFound(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Blocked task not found: {0}")]
    BlockedTaskNotFound(String),

    #[error("{0}")]
    Database(#[from] aionui_db::DbError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<TeamError> for AppError {
    fn from(err: TeamError) -> Self {
        match err {
            TeamError::TeamNotFound(msg) => AppError::NotFound(msg),
            TeamError::AgentNotFound(msg) => AppError::NotFound(msg),
            TeamError::ConversationNotFound(msg) => AppError::NotFound(msg),
            TeamError::TaskNotFound(msg) => AppError::NotFound(msg),
            TeamError::InvalidRequest(msg) => AppError::BadRequest(msg),
            TeamError::SessionNotFound(msg) => AppError::NotFound(msg),
            TeamError::BlockedTaskNotFound(msg) => AppError::BadRequest(msg),
            TeamError::Database(db_err) => AppError::from(db_err),
            TeamError::Json(e) => AppError::Internal(format!("JSON error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_not_found_maps_to_app_not_found() {
        let err: AppError = TeamError::TeamNotFound("t1".into()).into();
        assert!(matches!(err, AppError::NotFound(msg) if msg == "t1"));
    }

    #[test]
    fn agent_not_found_maps_to_app_not_found() {
        let err: AppError = TeamError::AgentNotFound("slot-1".into()).into();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[test]
    fn task_not_found_maps_to_app_not_found() {
        let err: AppError = TeamError::TaskNotFound("tk-1".into()).into();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[test]
    fn conversation_not_found_maps_to_app_not_found() {
        let err: AppError = TeamError::ConversationNotFound("c1".into()).into();
        assert!(matches!(err, AppError::NotFound(msg) if msg == "c1"));
    }

    #[test]
    fn invalid_request_maps_to_bad_request() {
        let err: AppError = TeamError::InvalidRequest("empty agents".into()).into();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn session_not_found_maps_to_not_found() {
        let err: AppError = TeamError::SessionNotFound("t1".into()).into();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[test]
    fn blocked_task_not_found_maps_to_bad_request() {
        let err: AppError = TeamError::BlockedTaskNotFound("tk-x".into()).into();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn json_error_maps_to_internal() {
        let json_err = serde_json::from_str::<serde_json::Value>("bad").unwrap_err();
        let err: AppError = TeamError::Json(json_err).into();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[test]
    fn display_messages() {
        assert_eq!(
            TeamError::TeamNotFound("t1".into()).to_string(),
            "Team not found: t1"
        );
        assert_eq!(
            TeamError::AgentNotFound("s1".into()).to_string(),
            "Agent not found: s1"
        );
        assert_eq!(
            TeamError::TaskNotFound("tk1".into()).to_string(),
            "Task not found: tk1"
        );
    }
}
