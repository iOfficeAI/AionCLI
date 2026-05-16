//! Subcommand implementations for the `aionui-backend` binary.
//!
//! Each submodule corresponds to a CLI subcommand. The shared bootstrap
//! helpers (`init_environment`, `init_data_layer`) live here so that
//! different subcommands can compose only the initialization layers they need.

mod bridge;
mod server;
mod team_guide;
mod team_stdio;

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use tracing::{info, warn};

use aionui_app::AppConfig;
use aionui_db::Database;

use crate::cli::Cli;

pub use bridge::run_mcp_bridge;
pub use server::run_server;
pub use team_guide::run_team_guide;
pub use team_stdio::run_team_stdio;

// ---------------------------------------------------------------------------
// Shared bootstrap layers
// ---------------------------------------------------------------------------

/// Resolved environment needed by all non-MCP subcommands.
pub(crate) struct ServerEnvironment {
    /// Must be held alive for the process lifetime to flush log buffers.
    pub _log_guard: crate::LogGuards,
    pub config: AppConfig,
}

/// Layer 1: Logging + config resolution.
///
/// Cheap, synchronous, no IO beyond creating the log directory.
/// All subcommands that need logging and config should call this first.
pub(crate) fn init_environment(cli: &Cli, merged_path: &str) -> Result<ServerEnvironment> {
    let log_dir = cli.log_dir.clone().unwrap_or_else(|| cli.data_dir.join("logs"));
    let log_guard = crate::init_tracing(&log_dir, cli.log_level.as_deref());

    info!(
        path_segments = merged_path.split(if cfg!(windows) { ';' } else { ':' }).count(),
        path_len = merged_path.len(),
        "startup: PATH ready"
    );

    let work_dir = resolve_work_dir(cli.work_dir.clone(), &cli.data_dir);

    // SAFETY: called before any service initialization; no concurrent reads.
    unsafe {
        std::env::set_var("AIONUI_WORK_DIR", &work_dir);
    }

    let config = AppConfig {
        host: cli.host.clone(),
        port: cli.port,
        data_dir: cli.data_dir.clone(),
        work_dir,
        app_version: cli.app_version.clone(),
        local: cli.local,
    };
    info!(
        "Running in {} mode — authentication is {}",
        if config.local { "local" } else { "remote" },
        if config.local { "disabled" } else { "enabled" }
    );

    Ok(ServerEnvironment {
        _log_guard: log_guard,
        config,
    })
}

/// Layer 2: Materialize builtin skills + initialize the database.
///
/// Requires only `data_dir`. Subcommands that need persistent state
/// (database, skill files) should call this after `init_environment`.
pub(crate) async fn init_data_layer(config: &AppConfig) -> Result<Database> {
    let boot = Instant::now();

    materialize_builtin_skills(&config.data_dir).await?;
    info!(
        elapsed_ms = boot.elapsed().as_millis(),
        "startup: builtin skills materialized"
    );

    let db_path = config.database_path();
    aionui_db::maybe_copy_legacy_database(&db_path)?;
    info!("Initializing database at {}", db_path.display());
    let database = aionui_db::init_database(&db_path).await?;
    info!(elapsed_ms = boot.elapsed().as_millis(), "startup: database initialized");

    Ok(database)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Resolve the conversation workspace directory.
///
/// Priority: `--work-dir` CLI flag → `AIONUI_WORK_DIR` env (when non-empty) →
/// `--data-dir` fallback.
fn resolve_work_dir(cli_work_dir: Option<PathBuf>, data_dir: &Path) -> PathBuf {
    cli_work_dir.unwrap_or_else(|| {
        std::env::var("AIONUI_WORK_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.to_path_buf())
    })
}

/// Materialize the embedded builtin-skills corpus to disk and clean up
/// pre-symlink era stale directories.
///
/// Gated by a `.version` file so this is a no-op on subsequent starts with
/// the same binary. When `AIONUI_BUILTIN_SKILLS_PATH` is set, skip
/// materialization — the override path is the source of truth in that mode.
async fn materialize_builtin_skills(data_dir: &Path) -> Result<()> {
    let skip = std::env::var(aionui_extension::BUILTIN_SKILLS_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    if skip {
        return Ok(());
    }

    aionui_extension::materialize_if_needed(
        data_dir,
        aionui_extension::builtin_skills_corpus(),
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to materialize builtin skills: {e}"))?;

    // Best-effort cleanup of directories left behind by pre-symlink
    // refactors. Failures are non-fatal — stale empty dirs are harmless.
    for stale in ["builtin-skills-view", "tmp", "agent-skills"] {
        let path = data_dir.join(stale);
        if path.exists()
            && let Err(e) = std::fs::remove_dir_all(&path)
        {
            warn!(
                path = %path.display(),
                error = %e,
                "failed to clean up stale data dir entry",
            );
        }
    }
    Ok(())
}
