//! AI agent lifecycle, worker task dispatch, and skill management.
pub mod acp_agent;
pub mod acp_error;
pub mod acp_protocol;
pub mod acp_routes;
pub mod agent_manager;
pub mod agent_registry;
pub mod agent_routes;
pub mod agent_task;
pub mod aionrs_agent;
pub mod auxiliary_routes;
pub mod backend_output_sink;
pub mod backend_protocol_sink;
pub mod cli_process;
pub mod factory;
pub mod first_message_injector;
pub mod idle_scanner;
pub mod manager;
pub mod nanobot_agent;
pub mod openclaw;
pub mod protocol;
pub mod shared_kernel;
pub mod skill_manager;
pub mod stream_event;
pub mod task_manager;
mod team_guide_prompt;
pub mod types;

pub use acp_agent::AcpAgentManager;
pub use acp_routes::{AcpRouterState, acp_routes};
pub use agent_manager::approval_key;
pub use agent_registry::AgentRegistry;
pub use agent_routes::{AgentRouterState, agent_routes};
#[cfg(any(test, feature = "test-support"))]
pub use agent_task::IMockAgent;
pub use agent_task::{AgentInstance, IAgentTask};
pub use aionrs_agent::AionrsAgentManager;
pub use aionui_api_types::{
    AcpBuildExtra, AcpModelInfo, AcpSessionConfigOption, AionrsBuildExtra, OpenClawBuildExtra, OpenClawGatewayConfig,
    RemoteBuildExtra, SlashCommandItem,
};
pub use auxiliary_routes::{AuxiliaryRouterState, auxiliary_routes};
pub use backend_output_sink::BackendOutputSink;
pub use backend_protocol_sink::BackendProtocolSink;
pub use cli_process::CliAgentProcess;
pub use factory::{AgentFactoryDeps, build_agent_factory};
pub use idle_scanner::start_idle_scanner;
pub use manager::acp::AcpSessionSyncService;
pub use manager::remote::{
    RemoteAgentConfig, RemoteAgentManager, RemoteAgentRouterState, RemoteAgentService, remote_agent_routes,
};
pub use nanobot_agent::NanobotAgentManager;
pub use openclaw::OpenClawAgentManager;
pub use skill_manager::{
    AcpSkillManager, SkillDefinition, SkillIndex, build_skills_index_text, build_system_instructions,
    build_system_instructions_with_skills_index, detect_skill_load_request, prepare_first_message,
    prepare_first_message_with_skills_index,
};
pub use stream_event::AgentStreamEvent;
pub use task_manager::{AgentFactory, IWorkerTaskManager, WorkerTaskManagerImpl};
pub use types::{AgentStreamChunk, AionrsCompatOverrides, AionrsResolvedConfig, BuildTaskOptions, SendMessageData};
