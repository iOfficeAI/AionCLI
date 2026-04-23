//! Shared primitives: error types, enums, ID generation, crypto, timestamps, and pagination.
pub mod constants;

mod crypto;
mod enums;
mod error;
mod id;
mod pagination;
mod timestamp;
mod types;

pub use crypto::{decrypt_string, encrypt_string};
pub use enums::{
    AcpBackend, AgentKillReason, AgentType, ConversationSource, ConversationStatus,
    FileChangeOperation, McpServerStatus, McpSource, MessagePosition, MessageStatus, MessageType,
    PreviewContentType, ProtocolType, RemoteAgentAuthType, RemoteAgentProtocol, RemoteAgentStatus,
};
pub use error::AppError;
pub use id::{fnv1a_hex8, generate_id, generate_prefixed_id};
pub use pagination::PaginatedResult;
pub use timestamp::{TimestampMs, now_ms};
pub use types::{Confirmation, ConfirmationOption, ProviderWithModel, UpdateType, VersionInfo};
