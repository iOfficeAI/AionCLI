mod app;
mod client;
mod config;
mod event;
mod ui;

use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tokio::time;

use app::{App, AppState};
use client::{AionClient, ServerEvent};
use config::CliConfig;

#[derive(Parser)]
#[command(name = "aion", version, about = "Aion AI CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Server URL
    #[arg(long, env = "AION_SERVER_URL", default_value = "http://127.0.0.1:3456")]
    server_url: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a chat session
    Chat {
        /// Agent type
        #[arg(long, default_value = "acp")]
        agent: String,

        /// Model override
        #[arg(long)]
        model: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(io::stderr)
        .init();

    let cli = Cli::parse();

    let (agent, model) = match cli.command {
        Some(Commands::Chat { agent, model }) => (agent, model),
        None => ("acp".to_string(), None),
    };

    let config = CliConfig {
        server_url: cli.server_url,
        agent_type: agent,
        model,
    };

    run_chat(config).await
}

async fn run_chat(config: CliConfig) -> Result<()> {
    let client = AionClient::new(config.clone());

    // Connect WebSocket
    let (ws_tx, mut ws_rx) = mpsc::unbounded_channel::<ServerEvent>();
    client
        .connect_ws(ws_tx)
        .await
        .context("Failed to connect to server")?;

    // Create conversation
    let conversation_id = client.create_conversation().await?;

    // Initialize app
    let mut app = App::new(config.agent_type.clone(), config.model.clone());
    app.conversation_id = Some(conversation_id.clone());
    app.state = AppState::Idle;

    // Setup terminal
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Spawn terminal event reader
    let mut term_rx = event::spawn_terminal_event_reader();

    // Tick interval for UI refresh
    let mut tick = time::interval(Duration::from_millis(16));
    tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    // Main event loop
    let result: Result<()> = loop {
        tokio::select! {
            Some(ev) = term_rx.recv() => {
                handle_terminal_event(&mut app, &client, &conversation_id, ev).await;
            }
            Some(server_event) = ws_rx.recv() => {
                app.handle_server_event(server_event);
            }
            _ = tick.tick() => {}
        }

        terminal.draw(|f| ui::render(f, &app))?;

        if app.should_quit {
            break Ok(());
        }
    };

    // Restore terminal
    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn handle_terminal_event(
    app: &mut App,
    client: &AionClient,
    conversation_id: &str,
    event: Event,
) {
    let Event::Key(KeyEvent { code, modifiers, .. }) = event else {
        return;
    };

    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
            app.clear_input();
        }
        (KeyCode::Enter, _) => {
            if app.state != AppState::Idle {
                return;
            }
            if let Some(text) = app.submit_input() {
                if let Err(e) = client.send_message(conversation_id, &text).await {
                    app.handle_server_event(ServerEvent::StreamError {
                        message: e.to_string(),
                    });
                }
            }
        }
        (KeyCode::Backspace, _) => {
            app.delete_char();
        }
        (KeyCode::Left, _) => {
            app.move_cursor_left();
        }
        (KeyCode::Right, _) => {
            app.move_cursor_right();
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            app.insert_char(c);
        }
        _ => {}
    }
}
