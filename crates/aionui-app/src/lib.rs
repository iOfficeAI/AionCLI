//! Application entry point: assembles all crates into an Axum server with DI and middleware.
pub mod bridge;
pub mod guide_stdio;
mod state_builders;
pub mod team_stdio;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::Method;
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use axum::{Json, Router, middleware};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use aionui_ai_agent::{
    AcpSessionSyncService, AcpSkillManager, AgentFactoryDeps, AgentRegistry, AgentRouterState, IWorkerTaskManager,
    RemoteAgentRouterState, WorkerTaskManagerImpl, agent_routes, build_agent_factory, remote_agent_routes,
};
use aionui_api_types::GuideMcpConfig;
use aionui_assets::{AssetRouterState, asset_routes};
use aionui_assistant::{AssistantRouterState, assistant_routes};
use aionui_auth::{
    AuthRouterState, AuthState, CookieConfig, JwtService, QrTokenStore, auth_middleware, auth_routes, csrf_middleware,
    resolve_jwt_secret, security_headers_middleware,
};
#[cfg(feature = "weixin")]
use aionui_channel::weixin_login_route;
use aionui_channel::{ChannelRouterState, channel_routes};
use aionui_common::OnConversationDelete;
use aionui_conversation::{ConversationRouterState, conversation_ops_routes, conversation_routes};
use aionui_cron::{CronRouterState, cron_routes};
use aionui_db::{
    Database, IAcpSessionRepository, IAgentMetadataRepository, IConversationRepository, IUserRepository,
    SqliteAcpSessionRepository, SqliteAgentMetadataRepository, SqliteConversationRepository, SqliteProviderRepository,
    SqliteRemoteAgentRepository, SqliteUserRepository,
};
use aionui_extension::{
    ExtensionRouterState, HubRouterState, SkillRouterState, extension_routes, hub_routes, skill_routes,
};
use aionui_file::{FileRouterState, file_routes};
use aionui_mcp::{McpRouterState, mcp_routes};
use aionui_office::{OfficeRouterState, office_proxy_routes, office_routes};
use aionui_realtime::{BroadcastEventBus, WebSocketManager, WsHandlerState, ws_upgrade_handler};
use aionui_shell::{ShellRouterState, shell_routes};
use aionui_system::{ConnectionTestRouterState, SystemRouterState, connection_test_routes, system_routes};
use aionui_team::{GuideMcpServer, TeamRouterState, team_routes};

pub use state_builders::{
    ChannelOrchestratorComponents, build_assistant_state, build_conversation_state, build_extension_states,
    build_module_states, build_ws_state,
};

/// Application configuration parsed from CLI arguments.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub data_dir: PathBuf,
    pub work_dir: PathBuf,
    pub app_version: String,
    /// Run in local embedded mode (skip authentication, use system_default_user).
    pub local: bool,
}

impl AppConfig {
    /// Format as `host:port` for socket binding.
    pub fn socket_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Path to the SQLite database file.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("aionui-backend.db")
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            host: aionui_common::constants::DEFAULT_HOST.to_string(),
            port: aionui_common::constants::DEFAULT_PORT,
            data_dir: PathBuf::from("data"),
            work_dir: PathBuf::from("data"),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            local: false,
        }
    }
}

/// Shared application services for dependency injection.
pub struct AppServices {
    pub database: Database,
    pub jwt_service: Arc<JwtService>,
    pub user_repo: Arc<dyn IUserRepository>,
    pub cookie_config: Arc<CookieConfig>,
    pub qr_token_store: Arc<QrTokenStore>,
    pub ws_manager: Arc<WebSocketManager>,
    pub event_bus: Arc<BroadcastEventBus>,
    pub worker_task_manager: Arc<dyn IWorkerTaskManager>,
    /// Same instance as `worker_task_manager`, exposed through the
    /// `OnConversationDelete` trait so `ConversationService::with_delete_hook`
    /// can wire it up. Optional because tests construct `AppServices` with a
    /// mock `worker_task_manager` that does not implement the trait.
    pub task_manager_delete_hook: Option<Arc<dyn OnConversationDelete>>,
    pub agent_registry: Arc<AgentRegistry>,
    pub conversation_repo: Arc<dyn IConversationRepository>,
    pub acp_session_sync: Arc<AcpSessionSyncService>,
    /// Raw JWT secret string, used to derive encryption keys.
    pub jwt_secret_raw: String,
    pub data_dir: PathBuf,
    pub work_dir: PathBuf,
    /// When `true`, skip JWT authentication and use a fixed default user.
    pub local: bool,
    pub app_version: String,
    /// Resolved skill paths. Shared with the `ConversationService` for
    /// snapshot resolution at create time.
    pub skill_paths: Arc<aionui_extension::SkillPaths>,
    /// Guide MCP server config (port, token, binary_path).
    /// `None` when the server failed to start (graceful degradation).
    pub guide_mcp_config: Option<GuideMcpConfig>,
    /// Guide MCP server instance kept alive for the app lifetime.
    _guide_server: Option<GuideMcpServer>,
}

impl AppServices {
    /// Replace the worker task manager after construction.
    ///
    /// Primarily used by tests to inject mock implementations.
    pub fn with_worker_task_manager(mut self, wtm: Arc<dyn IWorkerTaskManager>) -> Self {
        self.worker_task_manager = wtm;
        self
    }

    /// Wire the TeamSessionService into the Guide MCP server so
    /// `aion_create_team` requests can call `service.create_team(...)`.
    /// Called from `create_router` after `build_module_states`.
    async fn inject_guide_service(&self, service: std::sync::Weak<aionui_team::TeamSessionService>) {
        if let Some(server) = &self._guide_server {
            server.set_service(service).await;
        }
    }

    pub async fn from_config(database: Database, config: &AppConfig) -> anyhow::Result<Self> {
        let data_dir = config.data_dir.clone();
        let work_dir = config.work_dir.clone();
        let local = config.local;
        let app_version = config.app_version.clone();
        let user_repo: Arc<dyn IUserRepository> = Arc::new(SqliteUserRepository::new(database.pool().clone()));

        // Resolve JWT secret: env var → system user db field → random generation
        let env_secret = std::env::var("JWT_SECRET").ok();
        let system_user = user_repo
            .get_system_user()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get system user: {e}"))?;

        let db_secret = system_user
            .as_ref()
            .and_then(|u| u.jwt_secret.as_deref())
            .filter(|s| !s.is_empty());

        let (secret, is_new) = resolve_jwt_secret(env_secret.as_deref(), db_secret);

        // Persist newly generated secret to database
        if is_new && let Some(user) = &system_user {
            user_repo
                .update_jwt_secret(&user.id, &secret)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to persist JWT secret: {e}"))?;
            tracing::info!("Generated and persisted new JWT secret");
        }

        let encryption_key = derive_encryption_key(&secret);

        let remote_agent_repo = Arc::new(SqliteRemoteAgentRepository::new(database.pool().clone()));
        let provider_repo = Arc::new(SqliteProviderRepository::new(database.pool().clone()));

        let agent_metadata_repo: Arc<dyn IAgentMetadataRepository> =
            Arc::new(SqliteAgentMetadataRepository::new(database.pool().clone()));
        let agent_registry = AgentRegistry::new(agent_metadata_repo);
        agent_registry
            .hydrate()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to hydrate agent registry: {e}"))?;

        let acp_session_repo: Arc<dyn IAcpSessionRepository> =
            Arc::new(SqliteAcpSessionRepository::new(database.pool().clone()));
        let acp_agent_service = AcpSessionSyncService::new(acp_session_repo.clone());

        let conversation_repo: Arc<dyn IConversationRepository> =
            Arc::new(SqliteConversationRepository::new(database.pool().clone()));

        // Skill paths need app resource dir (for builtin rules) + data dir
        // (for user skills + materialized views). AcpSkillManager uses these
        // for first-message skill index/body loading.
        let app_resource_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .and_then(|p| p.parent().map(|pp| pp.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let skill_paths = Arc::new(aionui_extension::resolve_skill_paths(&app_resource_dir, &data_dir));

        // Absolute path to this process's binary. Reused as the `command` for
        // the stdio MCP bridge spawned by ACP CLIs when a team session is
        // attached to a conversation (phase1 mcp.md §4.6 single-binary model).
        let backend_binary_path =
            Arc::new(std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("aionui-backend")));

        // Start Guide MCP server. Failure is non-fatal: solo agents simply
        // won't get the `aion_create_team` tool.
        let (guide_server, guide_mcp_config) = match GuideMcpServer::start().await {
            Ok(srv) => {
                let config = GuideMcpConfig {
                    port: srv.http_port(),
                    token: srv.auth_token().to_owned(),
                    binary_path: backend_binary_path.to_string_lossy().to_string(),
                };
                tracing::info!(port = config.port, "Guide MCP server started");
                (Some(srv), Some(config))
            }
            Err(e) => {
                tracing::warn!(error = %e, "Guide MCP server failed to start; solo create-team disabled");
                (None, None)
            }
        };

        let factory = build_agent_factory(AgentFactoryDeps {
            skill_manager: AcpSkillManager::new(skill_paths.clone()),
            remote_agent_repo,
            provider_repo,
            encryption_key,
            agent_registry: agent_registry.clone(),
            acp_agent_service: acp_agent_service.clone(),
            data_dir: data_dir.clone(),
            backend_binary_path: backend_binary_path.clone(),
            guide_mcp_config: guide_mcp_config.clone(),
        });

        // Agent factory is now wired. Future extension/custom agents
        // that get written to `agent_metadata` will show up after the
        // relevant service calls `AgentRegistry::hydrate`.
        let task_manager_concrete = Arc::new(WorkerTaskManagerImpl::new(factory));
        let worker_task_manager: Arc<dyn IWorkerTaskManager> = task_manager_concrete.clone();
        let task_manager_delete_hook: Arc<dyn OnConversationDelete> = task_manager_concrete;

        Ok(Self {
            database,
            jwt_service: Arc::new(JwtService::new(secret.clone())),
            user_repo,
            cookie_config: Arc::new(CookieConfig::from_env()),
            qr_token_store: Arc::new(QrTokenStore::new()),
            ws_manager: Arc::new(WebSocketManager::new()),
            event_bus: Arc::new(BroadcastEventBus::new(256)),
            worker_task_manager,
            task_manager_delete_hook: Some(task_manager_delete_hook),
            agent_registry,
            conversation_repo,
            acp_session_sync: acp_agent_service,
            jwt_secret_raw: secret,
            data_dir,
            work_dir,
            local,
            app_version,
            skill_paths,
            guide_mcp_config: guide_mcp_config.clone(),
            _guide_server: guide_server,
        })
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    build_time: &'static str,
}

async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        build_time: env!("BUILD_TIME"),
    })
}

#[derive(Serialize)]
struct GuideMcpStatusResponse {
    running: bool,
    port: Option<u16>,
    binary_path: Option<String>,
}

async fn guide_mcp_status(
    axum::extract::State(cfg): axum::extract::State<Option<GuideMcpConfig>>,
) -> Json<GuideMcpStatusResponse> {
    Json(match cfg {
        Some(c) => GuideMcpStatusResponse {
            running: true,
            port: Some(c.port),
            binary_path: Some(c.binary_path),
        },
        None => GuideMcpStatusResponse {
            running: false,
            port: None,
            binary_path: None,
        },
    })
}

/// Derive a 32-byte encryption key from the JWT secret using SHA-256.
pub fn derive_encryption_key(jwt_secret: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"aionui-encryption-key:");
    hasher.update(jwt_secret.as_bytes());
    hasher.finalize().into()
}

/// All module-level router states bundled into a single struct.
///
/// Reduces parameter bloat on router constructors and makes it easy for
/// tests to override individual modules.
pub struct ModuleStates {
    pub system: SystemRouterState,
    pub conversation: ConversationRouterState,
    pub remote_agent: RemoteAgentRouterState,
    pub agent: AgentRouterState,

    pub connection_test: ConnectionTestRouterState,
    pub file: FileRouterState,
    pub mcp: McpRouterState,
    pub extension: ExtensionRouterState,
    pub hub: HubRouterState,
    pub skill: SkillRouterState,
    pub channel: ChannelRouterState,
    pub team: TeamRouterState,
    pub cron: CronRouterState,
    pub office: OfficeRouterState,
    pub shell: ShellRouterState,
    pub assistant: AssistantRouterState,
}

/// Create the application router with all routes and global middleware.
///
/// Middleware stack (outermost → innermost):
/// 1. Security response headers (X-Frame-Options, etc.)
/// 2. CSRF protection (Double Submit Cookie)
/// 3. Route handlers (auth routes + system routes + conversation routes + file routes + health check)
pub async fn create_router(services: &AppServices) -> Router {
    // Bridge event bus → WebSocket manager: forward all broadcast events
    // to connected WebSocket clients.
    let mut event_rx = services.event_bus.subscribe();
    let ws_manager = services.ws_manager.clone();
    tokio::spawn(async move {
        while let Ok(event) = event_rx.recv().await {
            ws_manager.broadcast_all(event);
        }
    });

    let (states, channel_components) = build_module_states(services).await;

    // Wire TeamSessionService into Guide MCP server now that both are available.
    services
        .inject_guide_service(Arc::downgrade(&states.team.service))
        .await;

    // Start channel orchestrator (message loop)
    tokio::spawn(
        channel_components
            .orchestrator
            .run(channel_components.message_rx, channel_components.confirm_rx),
    );

    // Restore enabled channel plugins (starts receiving IM messages)
    let chan_mgr = channel_components.manager;
    let chan_factory = channel_components.plugin_factory;
    tokio::spawn(async move {
        if let Err(e) = chan_mgr.restore_plugins(&chan_factory).await {
            tracing::warn!(error = %e, "failed to restore channel plugins");
        }
    });

    create_router_with_states(services, states)
}

/// Create the application router with custom module states.
///
/// Used for testing when specific service overrides are needed
/// (e.g. injecting a mock HTTP server URL for version check).
pub fn create_router_with_states(services: &AppServices, states: ModuleStates) -> Router {
    let ws_state = build_ws_state(services);
    create_router_with_all_state(services, states, ws_state)
}

fn with_access_log(router: Router) -> Router {
    router.layer(
        TraceLayer::new_for_http()
            .make_span_with(|req: &axum::http::Request<_>| {
                tracing::info_span!(
                    "http",
                    method = %req.method(),
                    path = %req.uri().path(),
                )
            })
            .on_response(
                |res: &axum::http::Response<_>, latency: std::time::Duration, _span: &tracing::Span| {
                    let status = res.status().as_u16();
                    let latency_ms = latency.as_millis() as u64;
                    if status >= 500 {
                        tracing::error!(status, latency_ms, "response");
                    } else if status >= 400 {
                        tracing::warn!(status, latency_ms, "response");
                    } else {
                        tracing::info!(status, latency_ms, "response");
                    }
                },
            )
            .on_failure(
                |error: tower_http::classify::ServerErrorsFailureClass,
                 latency: std::time::Duration,
                 _span: &tracing::Span| {
                    tracing::error!(
                        %error,
                        latency_ms = latency.as_millis() as u64,
                        "request failed"
                    );
                },
            ),
    )
}

/// Create the application router with custom module states and WebSocket state.
///
/// Full-control variant used by tests that need to override
/// module services and WebSocket behaviour.
pub fn create_router_with_all_state(services: &AppServices, states: ModuleStates, ws_state: WsHandlerState) -> Router {
    let auth_state = AuthRouterState {
        jwt_service: services.jwt_service.clone(),
        user_repo: services.user_repo.clone(),
        cookie_config: services.cookie_config.clone(),
        qr_token_store: services.qr_token_store.clone(),
        local: services.local,
    };

    let auth_mw_state = AuthState {
        jwt_service: services.jwt_service.clone(),
        user_repo: services.user_repo.clone(),
        local: services.local,
    };

    // System routes protected by auth middleware
    let system_authenticated =
        system_routes(states.system).route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Conversation routes protected by auth middleware
    let conversation_authenticated = conversation_routes(states.conversation.clone())
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    let conversation_ops_authenticated = conversation_ops_routes(states.conversation)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Remote agent routes protected by auth middleware
    let remote_agent_authenticated = remote_agent_routes(states.remote_agent)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Unified agent listing/refresh/test routes protected by auth middleware
    let agent_authenticated =
        agent_routes(states.agent).route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Connection test routes (Bedrock, Gemini) protected by auth middleware
    let connection_test_authenticated = connection_test_routes(states.connection_test)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // File routes protected by auth middleware
    let file_authenticated =
        file_routes(states.file).route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // MCP routes protected by auth middleware
    let mcp_authenticated =
        mcp_routes(states.mcp).route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Extension routes protected by auth middleware
    let extension_authenticated =
        extension_routes(states.extension).route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Hub routes protected by auth middleware
    let hub_authenticated =
        hub_routes(states.hub).route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Skill routes protected by auth middleware
    let skill_authenticated =
        skill_routes(states.skill).route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Channel routes protected by auth middleware
    #[cfg(feature = "weixin")]
    let weixin_login_authenticated = weixin_login_route(states.channel.clone())
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));
    let channel_authenticated =
        channel_routes(states.channel).route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Team routes protected by auth middleware
    let team_authenticated =
        team_routes(states.team).route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Cron routes protected by auth middleware
    let cron_authenticated =
        cron_routes(states.cron).route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Office routes protected by auth middleware
    let office_authenticated =
        office_routes(states.office.clone()).route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Shell + STT routes protected by auth middleware
    let shell_authenticated =
        shell_routes(states.shell).route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Assistant routes protected by auth middleware (T1a skeleton: all
    // handlers return 500 "not implemented"; T1b wires real service)
    let assistant_authenticated =
        assistant_routes(states.assistant).route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Guide MCP diagnostic endpoint protected by auth middleware
    let guide_mcp_authenticated = Router::new()
        .route("/api/system/guide-mcp", get(guide_mcp_status))
        .with_state(services.guide_mcp_config.clone())
        .route_layer(from_fn_with_state(auth_mw_state, auth_middleware));

    // Office proxy routes — exempt from auth (serve iframe content)
    let office_proxy = office_proxy_routes(states.office);
    let public_assets = asset_routes(AssetRouterState::default());

    // WebSocket upgrade route — exempt from CSRF (no cookie-based
    // double-submit) but still gets security response headers.
    let ws_routes = Router::new().route("/ws", get(ws_upgrade_handler)).with_state(ws_state);

    let router = Router::new()
        .route("/health", get(health_check))
        .merge(auth_routes(auth_state))
        .merge(system_authenticated)
        .merge(conversation_authenticated)
        .merge(conversation_ops_authenticated)
        .merge(remote_agent_authenticated)
        .merge(agent_authenticated)
        .merge(connection_test_authenticated)
        .merge(file_authenticated)
        .merge(mcp_authenticated)
        .merge(extension_authenticated)
        .merge(hub_authenticated)
        .merge(skill_authenticated)
        .merge(channel_authenticated)
        .merge(team_authenticated)
        .merge(cron_authenticated)
        .merge(office_authenticated)
        .merge(shell_authenticated)
        .merge(assistant_authenticated)
        .merge(guide_mcp_authenticated);

    // Conditionally merge WeChat login SSE route (feature-gated)
    #[cfg(feature = "weixin")]
    let router = router.merge(weixin_login_authenticated);

    let router = if services.local {
        router
    } else {
        router.layer(middleware::from_fn_with_state(
            services.cookie_config.clone(),
            csrf_middleware,
        ))
    }
    .merge(ws_routes)
    .merge(office_proxy)
    .merge(public_assets)
    .layer(middleware::from_fn(security_headers_middleware));

    // Raise the default request body limit from axum's 2MB default to
    // `BODY_LIMIT` (10MB). Routes that need a larger cap (e.g. `/api/fs/upload`)
    // disable this default and install their own `RequestBodyLimitLayer`.
    let router = router.layer(DefaultBodyLimit::max(aionui_common::constants::BODY_LIMIT));

    let router = with_access_log(router);

    if services.local {
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers(Any);
        router.layer(cors)
    } else {
        router
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_config_default() {
        let config = AppConfig::default();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 25808);
        assert_eq!(config.data_dir, PathBuf::from("data"));
        assert_eq!(config.app_version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn test_app_config_socket_addr() {
        let config = AppConfig {
            host: "0.0.0.0".to_string(),
            port: 3000,
            ..Default::default()
        };
        assert_eq!(config.socket_addr(), "0.0.0.0:3000");
    }

    #[test]
    fn test_app_config_database_path() {
        let config = AppConfig {
            data_dir: PathBuf::from("/tmp/aionui"),
            ..Default::default()
        };
        assert_eq!(config.database_path(), PathBuf::from("/tmp/aionui/aionui-backend.db"));
    }

    #[tokio::test]
    async fn test_app_services_from_memory_db() {
        let db = aionui_db::init_database_memory().await.unwrap();
        let services = AppServices::from_config(db, &AppConfig::default()).await.unwrap();

        // JWT service should be functional
        let token = services.jwt_service.sign("test_user", "testuser").unwrap();
        let payload = services.jwt_service.verify(&token).unwrap();
        assert_eq!(payload.user_id, "test_user");

        // User repo should have system user
        let has_users = services.user_repo.has_users().await.unwrap();
        assert!(!has_users); // system user has empty password → not counted

        services.database.close().await;
    }

    #[tokio::test]
    async fn test_jwt_secret_persisted_to_db() {
        let db = aionui_db::init_database_memory().await.unwrap();
        let services = AppServices::from_config(db, &AppConfig::default()).await.unwrap();

        // System user should now have a jwt_secret persisted
        let system_user = services.user_repo.get_system_user().await.unwrap();
        let jwt_secret = system_user.unwrap().jwt_secret;
        assert!(jwt_secret.is_some());
        assert!(!jwt_secret.unwrap().is_empty());

        services.database.close().await;
    }

    #[tokio::test]
    async fn test_app_services_uses_supplied_app_version() {
        let db = aionui_db::init_database_memory().await.unwrap();
        let config = AppConfig {
            app_version: "9.9.9".to_string(),
            ..Default::default()
        };
        let services = AppServices::from_config(db, &config).await.unwrap();

        assert_eq!(services.app_version, "9.9.9");

        services.database.close().await;
    }
}
