//! Subcommand implementations for the `aioncli` binary.
//!
//! This file is a façade — module declarations and re-exports only.
//! All logic lives in the submodules.

mod bridge;
mod doctor;
mod server;
mod team_guide;
mod team_stdio;

pub use bridge::run_mcp_bridge;
pub use doctor::run_doctor;
pub use server::run_server;
pub use team_guide::run_team_guide;
pub use team_stdio::run_team_stdio;
