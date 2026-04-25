use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

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

    /// Directory for log files. Defaults to {data-dir}/logs/.
    #[arg(long)]
    log_dir: Option<PathBuf>,

    /// Log level filter (e.g. "info", "debug", "info,aionui_mcp=trace").
    /// Overridden by RUST_LOG env var when set.
    #[arg(long)]
    log_level: Option<String>,
}

fn init_tracing(
    log_dir: &Path,
    log_level: Option<String>,
) -> tracing_appender::non_blocking::WorkerGuard {
    let default_filter =
        "info,aionui_ai_agent=debug,aionui_conversation=debug,aionui_realtime=debug";

    let make_filter = |fallback: &str| {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            log_level
                .as_deref()
                .map(EnvFilter::new)
                .unwrap_or_else(|| fallback.into())
        })
    };

    std::fs::create_dir_all(log_dir).expect("failed to create log directory");

    let console_layer = fmt::layer()
        .with_target(true)
        .with_filter(make_filter("info"));

    let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("backend")
        .filename_suffix("log")
        .build(log_dir)
        .expect("failed to create log file appender");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let file_layer = fmt::layer()
        .json()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_filter(make_filter(default_filter));

    tracing_subscriber::registry()
        .with(console_layer)
        .with(file_layer)
        .init();

    guard
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let log_dir = cli
        .log_dir
        .unwrap_or_else(|| Path::new(&cli.data_dir).join("logs"));
    let _log_guard = init_tracing(&log_dir, cli.log_level);

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
