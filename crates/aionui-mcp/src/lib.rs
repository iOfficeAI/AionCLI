pub mod adapter;
pub mod adapters;
pub mod error;
pub mod routes;
pub mod service;
pub mod sync_service;
pub mod types;

pub use adapter::{DetectedServer, McpAgentAdapter};
pub use adapters::{
    AionrsAdapter, AionuiAdapter, ClaudeAdapter, CodeBuddyAdapter, CodexAdapter, GeminiAdapter,
    IFlowAdapter, OpencodeAdapter, QwenAdapter,
};
pub use error::McpError;
pub use routes::{McpRouterState, mcp_routes};
pub use service::McpConfigService;
pub use sync_service::McpSyncService;
pub use types::{McpServer, McpServerTransport, McpTool};
