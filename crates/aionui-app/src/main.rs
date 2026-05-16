mod cli;
mod commands;

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use aionui_app::AppServices;
use cli::{Cli, Command};
use commands::{init_data_layer, init_environment};

const NOISE_SUPPRESSIONS: &[&str] = &["sqlx::query=warn", "hyper_util=warn", "reqwest=warn"];

const AIONRS_TARGETS: &[&str] = &[
    "aion_agent",
    "aion_config",
    "aion_compact",
    "aion_mcp",
    "aion_providers",
    "aion_protocol",
    "aion_tools",
    "aion_skills",
    "aion_memory",
];

fn build_env_filter(log_level: Option<&str>) -> EnvFilter {
    let user_directives = log_level.unwrap_or("info");
    let suppressions = NOISE_SUPPRESSIONS.join(",");
    EnvFilter::new(format!("{suppressions},{user_directives}"))
}

fn build_backend_filter(log_level: Option<&str>) -> EnvFilter {
    let user_directives = log_level.unwrap_or("info");
    let suppressions = NOISE_SUPPRESSIONS.join(",");
    let aionrs_off: String = AIONRS_TARGETS
        .iter()
        .map(|t| format!("{t}=off"))
        .collect::<Vec<_>>()
        .join(",");
    EnvFilter::new(format!("{suppressions},{aionrs_off},{user_directives}"))
}

struct LogGuards {
    _backend: tracing_appender::non_blocking::WorkerGuard,
    _aionrs: tracing_appender::non_blocking::WorkerGuard,
}

fn init_tracing(log_dir: &Path, log_level: Option<&str>) -> LogGuards {
    std::fs::create_dir_all(log_dir).expect("failed to create log directory");

    let console_layer = fmt::layer().with_target(true).with_filter(build_env_filter(log_level));

    // Backend file layer — excludes aion_* targets
    let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_suffix("backend.log")
        .build(log_dir)
        .expect("failed to create backend log file appender");
    let (non_blocking, backend_guard) = tracing_appender::non_blocking(file_appender);

    let backend_file_layer = fmt::layer()
        .json()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_filter(build_backend_filter(log_level));

    // Aionrs file layer — only aion_* targets
    let aionrs_level = {
        let level = log_level.unwrap_or("info");
        AIONRS_TARGETS
            .iter()
            .map(|t| format!("{t}={level}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let aionrs_resolved = aion_config::logging::ResolvedLogging {
        enabled: true,
        level: aionrs_level,
        dir: log_dir.to_path_buf(),
    };
    let (aionrs_layer, aionrs_guard) =
        aion_config::logging::create_file_layer(&aionrs_resolved).expect("failed to create aionrs log layer");

    tracing_subscriber::registry()
        .with(console_layer)
        .with(backend_file_layer)
        .with(aionrs_layer)
        .init();

    LogGuards {
        _backend: backend_guard,
        _aionrs: aionrs_guard,
    }
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();

    // mcp-* subcommands route into short-lived stdio helpers that live entirely
    // outside the main HTTP server. They share the global flags so clap can
    // parse a uniform CLI, but bypass `aionui_runtime::init` (which would
    // anchor the bun cache under --data-dir) — these helpers don't host agents.
    if cli.command.is_none() {
        aionui_runtime::init(&cli.data_dir);
    }

    // SAFETY: called before any worker thread exists (including the tokio
    // runtime constructed below). Rust 2024 requires `unsafe` for
    // `std::env::set_var` invoked inside `enhance_process_path`.
    let merged_path = unsafe { aionui_runtime::enhance_process_path() };

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async_main(merged_path, cli))
}

async fn async_main(merged_path: String, cli: Cli) -> Result<ExitCode> {
    // MCP stdio helpers must not touch the database, logging setup, or `AppServices`.
    match cli.command {
        Some(Command::McpBridge) => Ok(commands::run_mcp_bridge().await),
        Some(Command::McpGuideStdio) => Ok(commands::run_team_guide().await),
        Some(Command::McpTeamStdio) => Ok(commands::run_team_stdio().await),
        None => {
            let env = init_environment(&cli, &merged_path)?;
            let database = init_data_layer(&env.config).await?;
            let services = AppServices::from_config(database, &env.config).await?;
            commands::run_server(env, services).await
        }
    }
}
