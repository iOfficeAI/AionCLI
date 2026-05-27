//! AI agent lifecycle, connector dispatch, and skill management.
pub(crate) mod agent_runtime;
pub(crate) mod agent_task;
pub mod capability;
pub mod cc_switch;
pub mod connector;
pub mod connector_factory;
pub mod factory;
pub mod manager;
pub(crate) mod persistence;
pub mod protocol;
pub mod registry;
pub mod routes;
pub(crate) mod services;
pub mod shared_kernel;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod types;

pub use agent_runtime::AgentRuntime;
pub use aionui_api_types::{
    AcpBuildExtra, AcpModelInfo, AionrsBuildExtra, OpenClawBuildExtra, OpenClawGatewayConfig, RemoteBuildExtra,
    SlashCommandItem,
};
pub use capability::skill_manager::{
    AcpSkillManager, SkillDefinition, SkillIndex, build_skills_index_text, build_system_instructions,
    build_system_instructions_with_skills_index, detect_skill_load_request, prepare_first_message,
    prepare_first_message_with_skills_index,
};
pub use connector::{
    ChunkPayload, ConnectorError, ConnectorEvent, ExitInfo, IAgentConnector, StopReason, ToolUsePayload, TurnSummary,
};
pub use connector_factory::{ConnectorBuildFn, ConnectorFactory, IAgentConnectorFactory};
pub use factory::{AgentFactoryDeps, build_agent_factory};
pub use persistence::AcpSessionSyncService;
pub use protocol::events::AgentStreamEvent;
pub use registry::{AgentRegistry, UnavailableReason};
pub use routes::{AgentRouterState, RemoteAgentRouterState, agent_routes, remote_agent_routes};
pub use services::AgentService;
pub use services::RemoteAgentService;
