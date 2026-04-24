use anyhow::Result;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;

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
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,aionui_ai_agent=debug,aionui_conversation=debug,aionui_realtime=debug".into()
            }),
        )
        .with_target(true)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    init_tracing();

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
