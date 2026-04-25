use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use aionui_app::{AppConfig, AppServices, create_router};

#[derive(Parser)]
#[command(name = "aionui-backend", about = "AionUi Backend Server")]
struct Cli {
    /// Host address to listen on.
    #[arg(long, default_value_t = String::from(aionui_common::constants::DEFAULT_HOST))]
    host: String,

    /// Port number to listen on.
    #[arg(long, default_value_t = aionui_common::constants::DEFAULT_PORT)]
    port: u16,

    /// Data directory for database and file storage.
    #[arg(long, default_value = "data")]
    data_dir: String,

    /// Run in local embedded mode (skip authentication, use system_default_user).
    #[arg(long)]
    local: bool,

    /// Directory for log files. When set, logs are written to rolling daily
    /// files under this directory. Without this, logs go to stderr only.
    #[arg(long)]
    log_dir: Option<PathBuf>,

    /// Log level filter (e.g. "warn", "info", "debug", "info,aionui_ai_agent=debug").
    /// Defaults to "warn" when no log directory is set, "info" when one is.
    #[arg(long)]
    log_level: Option<String>,
}

fn init_tracing(log_dir: Option<PathBuf>, log_level: Option<String>) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let default_level = if log_dir.is_some() { "info" } else { "warn" };
    let filter_str = log_level.unwrap_or_else(|| default_level.to_owned());

    let make_filter = |s: &str| EnvFilter::try_new(s).unwrap_or_else(|_| EnvFilter::new(default_level));

    match log_dir {
        Some(dir) => {
            let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix("backend")
                .filename_suffix("log")
                .build(&dir)
                .expect("failed to create log file appender");
            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .with_ansi(false)
                        .with_target(true)
                        .with_filter(make_filter(&filter_str)),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(std::io::stderr)
                        .with_target(true)
                        .with_filter(EnvFilter::new("warn")),
                )
                .init();

            Some(guard)
        }
        None => {
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(std::io::stderr)
                        .with_target(true)
                        .with_filter(make_filter(&filter_str)),
                )
                .init();

            None
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let _log_guard = init_tracing(cli.log_dir, cli.log_level);

    let config = AppConfig {
        host: cli.host,
        port: cli.port,
        data_dir: cli.data_dir,
        local: cli.local,
    };

    // Initialize database and all services
    info!(
        "Initializing database at {}",
        config.database_path().display()
    );
    let database = aionui_db::init_database(&config.database_path()).await?;
    let services =
        AppServices::from_database_with_data_dir(database, config.data_dir.clone(), config.local)
            .await?;

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

    info!("Server listening on {addr}");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // Graceful shutdown: close database connections
    services.database.close().await;
    info!("Server shut down gracefully");

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
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
