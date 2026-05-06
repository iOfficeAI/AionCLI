//! AI agent lifecycle, worker task dispatch, and skill management.
pub mod acp_agent;
pub mod acp_routes;
pub mod agent_manager;
pub mod agent_registry;
pub mod agent_routes;
pub mod agent_task;
pub mod capability;
pub mod factory;
pub mod idle_scanner;
pub mod manager;
pub mod protocol;
pub mod routes;
pub mod shared_kernel;
pub mod task_manager;
pub mod types;

pub use acp_agent::AcpAgentManager;
pub use acp_routes::{AcpRouterState, acp_routes};
pub use agent_manager::approval_key;
pub use agent_registry::AgentRegistry;
pub use agent_routes::{AgentRouterState, agent_routes};
#[cfg(any(test, feature = "test-support"))]
pub use agent_task::IMockAgent;
pub use agent_task::{AgentInstance, IAgentTask};
pub use aionui_api_types::{
    AcpBuildExtra, AcpModelInfo, AcpSessionConfigOption, AionrsBuildExtra, OpenClawBuildExtra, OpenClawGatewayConfig,
    RemoteBuildExtra, SlashCommandItem,
};
pub use capability::backend_output_sink::BackendOutputSink;
pub use capability::backend_protocol_sink::BackendProtocolSink;
pub use capability::cli_process::CliAgentProcess;
pub use capability::skill_manager::{
    AcpSkillManager, SkillDefinition, SkillIndex, build_skills_index_text, build_system_instructions,
    build_system_instructions_with_skills_index, detect_skill_load_request, prepare_first_message,
    prepare_first_message_with_skills_index,
};
pub use factory::{AgentFactoryDeps, build_agent_factory};
pub use idle_scanner::start_idle_scanner;
pub use manager::acp::AcpSessionSyncService;
pub use manager::aionrs::AionrsAgentManager;
pub use manager::nanobot::NanobotAgentManager;
pub use manager::openclaw::OpenClawAgentManager;
pub use manager::remote::{
    RemoteAgentConfig, RemoteAgentManager, RemoteAgentRouterState, RemoteAgentService, remote_agent_routes,
};
pub use protocol::events::AgentStreamEvent;
pub use routes::{SessionRouterState, session_routes};
pub use task_manager::{AgentFactory, IWorkerTaskManager, WorkerTaskManagerImpl};
pub use types::{AgentStreamChunk, AionrsCompatOverrides, AionrsResolvedConfig, BuildTaskOptions, SendMessageData};
