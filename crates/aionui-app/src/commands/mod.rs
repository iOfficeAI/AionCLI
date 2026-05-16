//! Subcommand implementations for the `aionui-backend` binary.
//!
//! This file is a façade — module declarations and re-exports only.
//! All logic lives in the submodules.

mod bridge;
mod server;
mod team_guide;
mod team_stdio;

pub use bridge::run_mcp_bridge;
pub use server::run_server;
pub use team_guide::run_team_guide;
pub use team_stdio::run_team_stdio;
