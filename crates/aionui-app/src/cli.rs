//! CLI argument definitions for the `aionui-backend` binary.
//!
//! Kept separate from `main.rs` to isolate the clap surface (struct + enum +
//! attribute soup) from the runtime entry point. Visibility is `pub(crate)`
//! because only `main.rs` consumes it.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "aionui-backend", about = "AionUi Backend Server")]
pub(crate) struct Cli {
    /// Host address to listen on.
    #[arg(long, default_value_t = String::from(aionui_common::constants::DEFAULT_HOST))]
    pub host: String,

    /// Port number to listen on.
    #[arg(long, default_value_t = aionui_common::constants::DEFAULT_PORT)]
    pub port: u16,

    /// Data directory for database and file storage.
    #[arg(long, default_value = "data")]
    pub data_dir: PathBuf,

    /// Working directory for conversation workspaces.
    /// Falls back to AIONUI_WORK_DIR env, then to data-dir.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// Host application version used for extension engine compatibility.
    #[arg(long, default_value_t = env!("CARGO_PKG_VERSION").to_string())]
    pub app_version: String,

    /// Run in local embedded mode (skip authentication, use system_default_user).
    #[arg(long)]
    pub local: bool,

    /// Directory for log files. Defaults to {data-dir}/logs/.
    #[arg(long)]
    pub log_dir: Option<PathBuf>,

    /// Log level filter (e.g. "info", "debug", "info,aionui_mcp=trace").
    #[arg(long)]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

// `Mcp` prefix is load-bearing — clap derives kebab-case subcommand names
// (`mcp-bridge`, `mcp-guide-stdio`, `mcp-team-stdio`) that external callers
// (ACP agent CLI, team MCP bridge spec) depend on verbatim.
#[derive(Subcommand)]
#[allow(clippy::enum_variant_names)]
pub(crate) enum Command {
    /// Stdio ↔ TCP bridge for the team MCP server (spawned by the ACP agent CLI).
    McpBridge,
    /// MCP stdio server for team-guide tools (spawned by the ACP agent CLI).
    McpGuideStdio,
    /// MCP stdio server for team tools (spawned by the ACP agent CLI).
    McpTeamStdio,
}
