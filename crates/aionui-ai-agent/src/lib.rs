//! AI agent lifecycle, worker task dispatch, and skill management.
pub mod acp_agent;
pub mod acp_agent_service;
pub mod acp_error;
pub mod acp_protocol;
pub mod acp_routes;
pub mod acp_runtime_snapshot;
pub mod agent_manager;
pub mod agent_registry;
pub mod agent_routes;
pub mod aionrs_agent;
pub mod auxiliary_routes;
pub mod backend_output_sink;
pub mod backend_protocol_sink;
pub mod cli_process;
pub mod factory;
pub mod first_message_injector;
pub mod idle_scanner;
pub mod nanobot_agent;
pub mod openclaw;
pub mod remote_agent;
pub mod remote_agent_routes;
pub mod remote_agent_service;
pub mod skill_manager;
pub mod stream_event;
pub mod task_manager;
mod team_guide_prompt;
pub mod types;

pub use acp_agent::AcpAgentManager;
pub use acp_agent_service::AcpAgentService;
pub use acp_routes::{AcpRouterState, acp_routes};
pub use agent_manager::{AgentManagerHandle, IAgentManager, approval_key};
pub use agent_registry::AgentRegistry;
pub use agent_routes::{AgentRouterState, agent_routes};
pub use aionrs_agent::AionrsAgentManager;
pub use auxiliary_routes::{AuxiliaryRouterState, auxiliary_routes};
pub use backend_output_sink::BackendOutputSink;
pub use backend_protocol_sink::BackendProtocolSink;
pub use cli_process::CliAgentProcess;
pub use factory::{AgentFactoryDeps, build_agent_factory};
pub use idle_scanner::start_idle_scanner;
pub use nanobot_agent::NanobotAgentManager;
pub use openclaw::OpenClawAgentManager;
pub use remote_agent::{RemoteAgentConfig, RemoteAgentManager};
pub use remote_agent_routes::{RemoteAgentRouterState, remote_agent_routes};
pub use remote_agent_service::RemoteAgentService;
pub use skill_manager::{
    AcpSkillManager, SkillDefinition, SkillIndex, build_skills_index_text, build_system_instructions,
    build_system_instructions_with_skills_index, detect_skill_load_request, prepare_first_message,
    prepare_first_message_with_skills_index,
};
pub use stream_event::AgentStreamEvent;
pub use task_manager::{AgentFactory, IWorkerTaskManager, WorkerTaskManagerImpl};
pub use types::{
    AcpBuildExtra, AcpModelInfo, AcpSessionConfigOption, AgentStreamChunk, AionrsBuildExtra, AionrsCompatOverrides,
    AionrsResolvedConfig, BuildTaskOptions, OpenClawBuildExtra, OpenClawGatewayConfig, RemoteBuildExtra,
    SendMessageData, SlashCommandItem,
};
