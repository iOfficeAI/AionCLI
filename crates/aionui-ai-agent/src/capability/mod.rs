//! Runtime capability modules shared across agent managers.
//!
//! These modules provide reusable primitives (CLI process supervision,
//! skill indexing, backend output/protocol sinks, first-message injection,
//! solo-team guide prompts) that any agent implementation can compose.

pub mod backend_output_sink;
pub mod backend_protocol_sink;
pub mod cli_process;
pub mod first_message_injector;
pub mod skill_manager;
pub mod team_guide_prompt;
