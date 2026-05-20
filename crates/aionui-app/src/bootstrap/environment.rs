//! Bootstrap layers shared by non-MCP subcommands.

use std::time::Instant;

use anyhow::Result;
use tracing::{info, warn};

use aionui_app::AppConfig;
use aionui_db::Database;

use crate::cli::Cli;

use super::builtin_skills::materialize_builtin_skills;
use super::tracing_init::{LogGuards, init_tracing};
use super::work_dir::resolve_work_dir;

/// Resolved environment needed by all non-MCP subcommands.
pub struct ServerEnvironment {
    /// Must be held alive for the process lifetime to flush log buffers.
    pub _log_guard: LogGuards,
    pub config: AppConfig,
}

/// Layer 1: Logging + config resolution.
///
/// Cheap, synchronous, no IO beyond creating the log directory.
/// All subcommands that need logging and config should call this first.
pub fn init_environment(cli: &Cli, merged_path: &str) -> Result<ServerEnvironment> {
    let log_dir = cli.log_dir.clone().unwrap_or_else(|| cli.data_dir.join("logs"));
    let log_guard = init_tracing(&log_dir, cli.log_level.as_deref());

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
pub async fn init_data_layer(config: &AppConfig) -> Result<Database> {
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

    ensure_acp_packages(&config.data_dir, &database).await;
    info!(elapsed_ms = boot.elapsed().as_millis(), "startup: acp packages ready");

    Ok(database)
}

/// Pre-install ACP packages so runtime spawns avoid `bun x` install overhead.
/// Queries `agent_metadata` for bun-launched agents and installs their packages.
/// Failures are logged but never block startup (graceful degradation).
async fn ensure_acp_packages(data_dir: &std::path::Path, database: &Database) {
    use aionui_db::{IAgentMetadataRepository, SqliteAgentMetadataRepository};
    use aionui_runtime::acp_package::{ensure_packages, parse_bun_x_args};

    const PREINSTALL_IDS: &[&str] = &["2d23ff1c", "8e1acf31"];

    let repo = SqliteAgentMetadataRepository::new(database.pool().clone());
    let all_rows = match repo.list_all().await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "failed to query agent_metadata for ACP packages");
            return;
        }
    };

    let specs: Vec<_> = all_rows
        .iter()
        .filter(|row| row.command.as_deref() == Some("bun") && PREINSTALL_IDS.contains(&row.id.as_str()))
        .filter_map(|row| row.args.as_deref().and_then(parse_bun_x_args))
        .collect();

    if specs.is_empty() {
        return;
    }

    ensure_packages(data_dir, &specs).await;
}
