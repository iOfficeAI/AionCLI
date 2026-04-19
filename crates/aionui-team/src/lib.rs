pub mod error;
pub mod events;
pub mod mailbox;
pub mod mcp;
pub mod prompts;
pub mod routes;
pub mod scheduler;
pub mod service;
pub mod session;
pub mod task_board;
#[cfg(test)]
pub(crate) mod test_utils;
pub mod types;

pub use error::TeamError;
pub use events::TeamEventEmitter;
pub use mailbox::Mailbox;
pub use mcp::{TeamMcpServer, TeamMcpStdioConfig};
pub use prompts::{build_lead_prompt, build_teammate_prompt, build_wake_payload};
pub use routes::{TeamRouterState, team_routes};
pub use scheduler::{SchedulerAction, TeammateManager, WakePayload, WAKE_TIMEOUT_MS};
pub use service::TeamSessionService;
pub use session::TeamSession;
pub use task_board::{TaskBoard, TaskUpdate};
pub use types::{
    MailboxMessage, MailboxMessageType, TaskStatus, Team, TeamAgent, TeamTask, TeammateRole,
    TeammateStatus,
};
