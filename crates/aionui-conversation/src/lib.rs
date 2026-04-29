//! Conversation and message CRUD with streaming relay and event emission.
mod convert;
pub mod routes;
pub mod service;
pub mod skill_resolver;
pub mod skill_snapshot;
pub mod state;
pub mod stream_relay;
pub mod traits;

pub use routes::conversation_routes;
pub use service::{ConversationService, OnConversationDelete};
pub use state::ConversationRouterState;
pub use traits::ITeamMessageRouter;

#[cfg(test)]
#[path = "service_test.rs"]
mod service_test;
