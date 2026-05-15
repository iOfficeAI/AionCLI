mod cli;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use aionui_app::{AppConfig, AppServices, bridge, create_router, guide_stdio, team_stdio};
use cli::{Cli, Command};

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
        Some(Command::McpBridge) => return Ok(bridge::run_mcp_bridge().await),
        Some(Command::McpGuideStdio) => return Ok(guide_stdio::run_guide_stdio().await),
        Some(Command::McpTeamStdio) => return Ok(team_stdio::run_team_stdio().await),
        None => {}
    }

    let log_dir = cli.log_dir.unwrap_or_else(|| cli.data_dir.join("logs"));
    let _log_guard = init_tracing(&log_dir, cli.log_level.as_deref());

    tracing::info!(
        path_segments = merged_path.split(if cfg!(windows) { ';' } else { ':' }).count(),
        path_len = merged_path.len(),
        "startup: PATH ready"
    );

    let work_dir = cli.work_dir.unwrap_or_else(|| {
        std::env::var("AIONUI_WORK_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| cli.data_dir.clone())
    });

    // SAFETY: called before any service initialization; no concurrent reads.
    unsafe {
        std::env::set_var("AIONUI_WORK_DIR", &work_dir);
    }

    let config = AppConfig {
        host: cli.host,
        port: cli.port,
        data_dir: cli.data_dir,
        work_dir,
        app_version: cli.app_version,
        local: cli.local,
    };

    let boot = Instant::now();

    // Initialize database and all services
    let db_path = config.database_path();
    aionui_db::maybe_copy_legacy_database(&db_path)?;
    info!("Initializing database at {}", db_path.display());
    let database = aionui_db::init_database(&db_path).await?;
    info!(elapsed_ms = boot.elapsed().as_millis(), "startup: database initialized");

    // Materialize the embedded builtin-skills corpus to disk before any
    // service can read from it. Gated by a .version file so this is a
    // no-op on subsequent starts with the same binary. When
    // `AIONUI_BUILTIN_SKILLS_PATH` is set, skip materialization — the
    // override path is the source of truth in that mode.
    if std::env::var(aionui_extension::BUILTIN_SKILLS_ENV_VAR)
        .map(|v| v.is_empty())
        .unwrap_or(true)
    {
        aionui_extension::materialize_if_needed(
            &config.data_dir,
            aionui_extension::builtin_skills_corpus(),
            env!("CARGO_PKG_VERSION"),
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to materialize builtin skills: {e}"))?;

        // Best-effort cleanup of directories left behind by pre-symlink
        // refactors. Failures are non-fatal — stale empty dirs are
        // harmless. Runs ONLY after `materialize_if_needed` succeeded so
        // we never touch data_dir until the builtin skills tree is in
        // place. Do NOT generalize this list — it is an explicit
        // allowlist of known-dead directories.
        for stale in ["builtin-skills-view", "tmp", "agent-skills"] {
            let path = config.data_dir.join(stale);
            if path.exists()
                && let Err(e) = std::fs::remove_dir_all(&path)
            {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to clean up stale data dir entry",
                );
            }
        }
    }
    info!(
        elapsed_ms = boot.elapsed().as_millis(),
        "startup: builtin skills materialized"
    );

    let services = AppServices::from_config(database, &config).await?;
    info!(elapsed_ms = boot.elapsed().as_millis(), "startup: services constructed");

    if config.local {
        info!("Running in local mode — authentication is disabled");
    }

    // Check bootstrap status
    let has_users = services.user_repo.has_users().await?;
    if !has_users {
        info!("No configured users detected — initial setup required via /api/auth/status");
    }

    let router = create_router(&services).await;
    let addr = config.socket_addr();
    let listener = TcpListener::bind(&addr).await?;

    info!(elapsed_ms = boot.elapsed().as_millis(), "Server listening on {addr}");

    // Kick off the idle-ACP-agent reaper. `start_idle_scanner` returns
    // immediately with a `JoinHandle`; the scanner task polls every 60 s
    // and kills ACP agents whose `status == Finished` + last_activity
    // exceeds the default 5-minute idle threshold. The watch channel
    // propagates graceful-shutdown so the scanner exits on SIGINT/SIGTERM.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let idle_scanner_handle =
        aionui_ai_agent::start_idle_scanner(services.worker_task_manager.clone(), None, shutdown_rx);

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            // Best-effort: only fails if every receiver has been dropped, in
            // which case the scanner is already gone and there is nothing to
            // signal.
            let _ = shutdown_tx.send(true);
        })
        .await?;

    // Wait for the scanner to observe the shutdown watch value and
    // return; at worst this blocks for the current 60 s tick. Log-only
    // on failure since the HTTP server has already drained.
    if let Err(e) = idle_scanner_handle.await {
        tracing::warn!(error = %e, "idle scanner join failed");
    }

    // Graceful shutdown: close database connections
    services.database.close().await;
    info!("Server shut down gracefully");

    Ok(ExitCode::SUCCESS)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {
            info!("Received SIGINT, shutting down...");
        }
        () = terminate => {
            info!("Received SIGTERM, shutting down...");
        }
    }
}
