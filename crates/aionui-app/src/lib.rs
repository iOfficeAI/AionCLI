//! Application entry point: assembles all crates into an Axum server with DI and middleware.
use std::sync::Arc;

use axum::http::Method;
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use axum::{Json, Router, middleware};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tower_http::cors::{Any, CorsLayer};

use aionui_ai_agent::{
    AcpRouterState, AcpSkillManager, AgentFactoryDeps, AgentRegistry, AuxiliaryRouterState,
    ConnectionTestRouterState, ConnectionTestService, IWorkerTaskManager, RemoteAgentRouterState,
    RemoteAgentService, WorkerTaskManagerImpl, acp_routes, auxiliary_routes, build_agent_factory,
    connection_test_routes, remote_agent_routes,
};
use aionui_api_types::{AgentSource, DetectedAgent, EnvVar};
use aionui_auth::{
    AuthRouterState, AuthState, CookieConfig, JwtService, QrTokenStore, auth_middleware,
    auth_routes, csrf_middleware, extract_token_from_ws_headers, resolve_jwt_secret,
    security_headers_middleware,
};
#[cfg(feature = "weixin")]
use aionui_channel::weixin_login_route;
use aionui_channel::{ChannelRouterState, channel_routes};
use aionui_common::AcpBackend;
use aionui_conversation::{ConversationRouterState, ConversationService, conversation_routes};
use aionui_cron::{CronEventEmitter, CronRouterState, cron_routes};
use aionui_db::{
    Database, IUserRepository, SqliteClientPreferenceRepository, SqliteConversationRepository,
    SqliteProviderRepository, SqliteRemoteAgentRepository, SqliteSettingsRepository,
    SqliteUserRepository,
};
use aionui_extension::{
    ExtensionRegistry, ExtensionRouterState, ExtensionStateStore, ExternalPathsManager,
    HubIndexManager, HubInstaller, HubRouterState, SkillRouterState, extension_routes, hub_routes,
    skill_routes,
};
use aionui_file::{FileRouterState, FileService, FileWatchService, SnapshotService, file_routes};
use aionui_mcp::{
    AionrsAdapter, AionuiAdapter, ClaudeAdapter, CodeBuddyAdapter, CodexAdapter, GeminiAdapter,
    IFlowAdapter, McpAgentAdapter, McpConfigService, McpConnectionTestService, McpRouterState,
    McpSyncService, OpencodeAdapter, QwenAdapter, mcp_routes,
};
use aionui_office::{
    ConversionService, OfficeRouterState, OfficecliWatchManager, ProxyService,
    SnapshotService as OfficeSnapshotService, StarOfficeDetector, office_proxy_routes,
    office_routes,
};
use aionui_realtime::{
    BroadcastEventBus, NoopMessageRouter, WebSocketManager, WsHandlerState, ws_upgrade_handler,
};
use aionui_shell::{ShellRouterState, shell_routes};
use aionui_system::{
    ClientPrefService, ModelFetchService, ProtocolDetectionService, ProviderService,
    SettingsService, SystemRouterState, VersionCheckService, system_routes,
};
use aionui_team::{TeamRouterState, TeamSessionService, team_routes};

/// Application configuration parsed from CLI arguments.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub data_dir: String,
    /// Run in local embedded mode (skip authentication, use system_default_user).
    pub local: bool,
}

impl AppConfig {
    /// Format as `host:port` for socket binding.
    pub fn socket_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Path to the SQLite database file.
    pub fn database_path(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.data_dir).join("aionui.db")
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            host: aionui_common::constants::DEFAULT_HOST.to_string(),
            port: aionui_common::constants::DEFAULT_PORT,
            data_dir: "data".to_string(),
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
    pub agent_registry: Arc<AgentRegistry>,
    /// Raw JWT secret string, used to derive encryption keys.
    pub jwt_secret_raw: String,
    pub data_dir: String,
    /// When `true`, skip JWT authentication and use a fixed default user.
    pub local: bool,
}

impl AppServices {
    /// Replace the worker task manager after construction.
    ///
    /// Primarily used by tests to inject mock implementations.
    pub fn with_worker_task_manager(mut self, wtm: Arc<dyn IWorkerTaskManager>) -> Self {
        self.worker_task_manager = wtm;
        self
    }

    /// Build application services from an initialized database.
    ///
    /// Resolves JWT secret (env → db → generate), constructs all shared
    /// services, and persists a newly generated secret to the database.
    pub async fn from_database(database: Database) -> anyhow::Result<Self> {
        Self::from_database_with_data_dir(database, "data".to_string(), false).await
    }

    pub async fn from_database_with_data_dir(
        database: Database,
        data_dir: String,
        local: bool,
    ) -> anyhow::Result<Self> {
        let user_repo: Arc<dyn IUserRepository> =
            Arc::new(SqliteUserRepository::new(database.pool().clone()));

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
        let agent_registry = Arc::new(AgentRegistry::new());
        let factory = build_agent_factory(AgentFactoryDeps {
            skill_manager: AcpSkillManager::new(),
            remote_agent_repo,
            encryption_key,
            agent_registry: agent_registry.clone(),
        });
        let worker_task_manager: Arc<dyn IWorkerTaskManager> =
            Arc::new(WorkerTaskManagerImpl::new(factory));

        Ok(Self {
            database,
            jwt_service: Arc::new(JwtService::new(secret.clone())),
            user_repo,
            cookie_config: Arc::new(CookieConfig::from_env()),
            qr_token_store: Arc::new(QrTokenStore::new()),
            ws_manager: Arc::new(WebSocketManager::new()),
            event_bus: Arc::new(BroadcastEventBus::new(256)),
            worker_task_manager,
            agent_registry,
            jwt_secret_raw: secret,
            data_dir,
            local,
        })
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
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
    pub acp: AcpRouterState,
    pub connection_test: ConnectionTestRouterState,
    pub auxiliary: AuxiliaryRouterState,
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
}

/// Convert extension-contributed ACP adapters into `DetectedAgent` values.
async fn resolve_extension_agents(registry: &ExtensionRegistry) -> Vec<DetectedAgent> {
    registry
        .get_acp_adapters()
        .await
        .into_iter()
        .filter(|a| {
            a.connection_type
                .as_deref()
                .is_none_or(|ct| ct == "cli" || ct == "stdio")
        })
        .map(|a| DetectedAgent {
            id: a.id,
            name: a.name,
            backend: AcpBackend::Custom,
            available: true,
            source: AgentSource::Extension,
            command: a.default_cli_path.or(a.cli_command),
            args: a.acp_args,
            env: a
                .env
                .into_iter()
                .map(|(k, v)| EnvVar { name: k, value: v })
                .collect(),
        })
        .collect()
}

/// Build all default `ModuleStates` from application services.
pub async fn build_module_states(services: &AppServices) -> ModuleStates {
    let (ext_state, hub_state, skill_state) = build_extension_states(services).await;

    let extensions = resolve_extension_agents(&ext_state.registry).await;
    // TODO: load custom agent configs from settings/DB and convert to DetectedAgent
    services.agent_registry.initialize(extensions, vec![]).await;

    ModuleStates {
        system: build_system_state(services),
        conversation: build_conversation_state(services),
        remote_agent: build_remote_agent_state(services),
        acp: build_acp_state(services),
        connection_test: build_connection_test_state(),
        auxiliary: build_auxiliary_state(services),
        file: build_file_state(services),
        mcp: build_mcp_state(services),
        extension: ext_state,
        hub: hub_state,
        skill: skill_state,
        channel: build_channel_state(services),
        team: build_team_state(services),
        cron: build_cron_state(services),
        office: build_office_state(services),
        shell: build_shell_state(services),
    }
}

/// Build the default `SystemRouterState` from application services.
///
/// Tests can call this and override individual fields before passing
/// to [`create_router_with_states`].
pub fn build_system_state(services: &AppServices) -> SystemRouterState {
    let encryption_key = derive_encryption_key(&services.jwt_secret_raw);
    let pool = services.database.pool().clone();
    let provider_repo = Arc::new(SqliteProviderRepository::new(pool.clone()));
    let http_client = reqwest::Client::new();

    SystemRouterState {
        settings_service: SettingsService::new(Arc::new(SqliteSettingsRepository::new(
            pool.clone(),
        ))),
        client_pref_service: ClientPrefService::new(Arc::new(
            SqliteClientPreferenceRepository::new(pool),
        )),
        provider_service: ProviderService::new(provider_repo.clone(), encryption_key),
        model_fetch_service: ModelFetchService::new(
            provider_repo,
            encryption_key,
            http_client.clone(),
        ),
        protocol_detection_service: ProtocolDetectionService::new(http_client.clone()),
        version_check_service: VersionCheckService::new(
            http_client,
            env!("CARGO_PKG_VERSION").to_owned(),
        ),
    }
}

/// Create the application router with all routes and global middleware.
///
/// Middleware stack (outermost → innermost):
/// 1. Security response headers (X-Frame-Options, etc.)
/// 2. CSRF protection (Double Submit Cookie)
/// 3. Route handlers (auth routes + system routes + conversation routes + file routes + health check)
pub async fn create_router(services: &AppServices) -> Router {
    let states = build_module_states(services).await;
    create_router_with_states(services, states)
}

/// Build the default `ConversationRouterState` from application services.
pub fn build_conversation_state(services: &AppServices) -> ConversationRouterState {
    let pool = services.database.pool().clone();
    let repo = Arc::new(SqliteConversationRepository::new(pool));
    ConversationRouterState {
        conversation_service: ConversationService::new(repo, services.event_bus.clone()),
        worker_task_manager: services.worker_task_manager.clone(),
    }
}

/// Build the default `RemoteAgentRouterState` from application services.
pub fn build_remote_agent_state(services: &AppServices) -> RemoteAgentRouterState {
    let encryption_key = derive_encryption_key(&services.jwt_secret_raw);
    let pool = services.database.pool().clone();
    let repo = Arc::new(SqliteRemoteAgentRepository::new(pool));
    RemoteAgentRouterState {
        service: RemoteAgentService::new(repo, encryption_key),
    }
}

/// Build the default `AcpRouterState` from application services.
pub fn build_acp_state(services: &AppServices) -> AcpRouterState {
    AcpRouterState {
        worker_task_manager: services.worker_task_manager.clone(),
        agent_registry: services.agent_registry.clone(),
    }
}

/// Build the default `ConnectionTestRouterState`.
pub fn build_connection_test_state() -> ConnectionTestRouterState {
    ConnectionTestRouterState {
        service: ConnectionTestService::new(reqwest::Client::new()),
    }
}

/// Build the default `AuxiliaryRouterState` from application services.
pub fn build_auxiliary_state(services: &AppServices) -> AuxiliaryRouterState {
    AuxiliaryRouterState {
        worker_task_manager: services.worker_task_manager.clone(),
    }
}

/// Build the default `FileRouterState` from application services.
pub fn build_file_state(services: &AppServices) -> FileRouterState {
    let broadcaster = services.event_bus.clone();
    let allowed_roots = vec![
        std::env::temp_dir(),
        dirs::home_dir().unwrap_or_else(std::env::temp_dir),
    ];
    let file_service = Arc::new(FileService::new(broadcaster.clone(), allowed_roots));
    let watch_service =
        Arc::new(FileWatchService::new(broadcaster).expect("file watch service initialization"));
    let snapshot_service = Arc::new(SnapshotService::new());
    FileRouterState {
        file_service,
        watch_service,
        snapshot_service,
    }
}

/// Build the default `McpRouterState` from application services.
pub fn build_mcp_state(services: &AppServices) -> McpRouterState {
    let pool = services.database.pool().clone();
    let repo: Arc<dyn aionui_db::IMcpServerRepository> =
        Arc::new(aionui_db::SqliteMcpServerRepository::new(pool));

    let adapters: Vec<Arc<dyn McpAgentAdapter>> = vec![
        Arc::new(ClaudeAdapter),
        Arc::new(GeminiAdapter),
        Arc::new(QwenAdapter),
        Arc::new(IFlowAdapter),
        Arc::new(CodexAdapter),
        Arc::new(CodeBuddyAdapter),
        Arc::new(OpencodeAdapter),
        Arc::new(AionrsAdapter),
        Arc::new(AionuiAdapter::new(repo.clone())),
    ];

    let oauth_token_repo: Arc<dyn aionui_db::IOAuthTokenRepository> = Arc::new(
        aionui_db::SqliteOAuthTokenRepository::new(services.database.pool().clone()),
    );
    let http_client = reqwest::Client::new();

    McpRouterState {
        config_service: McpConfigService::new(repo.clone()),
        sync_service: McpSyncService::new(repo, adapters),
        connection_test_service: McpConnectionTestService::new(http_client.clone()),
        oauth_service: aionui_mcp::McpOAuthService::new(oauth_token_repo, http_client),
    }
}

/// Build the default `ChannelRouterState` from application services.
pub fn build_channel_state(services: &AppServices) -> ChannelRouterState {
    let pool = services.database.pool().clone();
    let repo: Arc<dyn aionui_db::IChannelRepository> =
        Arc::new(aionui_db::SqliteChannelRepository::new(pool));
    let encryption_key = derive_encryption_key(&services.jwt_secret_raw);

    let (message_tx, _message_rx) = tokio::sync::mpsc::channel(256);
    let (confirm_tx, _confirm_rx) = tokio::sync::mpsc::channel(256);

    let manager = Arc::new(aionui_channel::manager::ChannelManager::new(
        repo.clone(),
        services.event_bus.clone(),
        encryption_key,
        message_tx,
        confirm_tx,
    ));

    let pairing_service = Arc::new(aionui_channel::pairing::PairingService::new(
        repo.clone(),
        services.event_bus.clone(),
    ));

    let session_manager = Arc::new(aionui_channel::session::SessionManager::new(repo.clone()));

    let plugin_factory: Arc<aionui_channel::manager::PluginFactory> =
        Arc::new(Box::new(aionui_channel::plugins::create_plugin));

    ChannelRouterState {
        manager,
        pairing_service,
        session_manager,
        repo,
        plugin_factory,
    }
}

/// Build the default `TeamRouterState` from application services.
pub fn build_team_state(services: &AppServices) -> TeamRouterState {
    let pool = services.database.pool().clone();
    let team_repo: Arc<dyn aionui_db::ITeamRepository> =
        Arc::new(aionui_db::SqliteTeamRepository::new(pool.clone()));
    let conv_repo: Arc<dyn aionui_db::IConversationRepository> =
        Arc::new(SqliteConversationRepository::new(pool));
    let conv_service = ConversationService::new(conv_repo, services.event_bus.clone());
    let service = Arc::new(TeamSessionService::new(
        team_repo,
        conv_service,
        services.event_bus.clone(),
    ));
    TeamRouterState { service }
}

/// Build the default `CronRouterState` from application services.
pub fn build_cron_state(services: &AppServices) -> CronRouterState {
    let pool = services.database.pool().clone();
    let cron_repo: Arc<dyn aionui_db::ICronRepository> =
        Arc::new(aionui_db::SqliteCronRepository::new(pool.clone()));

    let conv_repo: Arc<dyn aionui_db::IConversationRepository> =
        Arc::new(SqliteConversationRepository::new(pool));
    let conv_service = ConversationService::new(conv_repo.clone(), services.event_bus.clone());

    let busy_guard = Arc::new(aionui_cron::busy_guard::CronBusyGuard::new());
    let executor = Arc::new(aionui_cron::executor::JobExecutor::new(
        services.worker_task_manager.clone(),
        conv_repo,
        Arc::new(conv_service.clone()),
        busy_guard,
    ));

    let tick_service_ref: Arc<CronServiceTickRef> = Arc::new(CronServiceTickRef::default());
    let tick_ref = tick_service_ref.clone();
    let scheduler = Arc::new(aionui_cron::scheduler::CronScheduler::new(Arc::new(
        move |job_id: String| {
            let svc = tick_ref.0.lock().unwrap().clone();
            tokio::spawn(async move {
                if let Some(svc) = svc {
                    svc.tick(&job_id).await;
                }
            });
        },
    )));

    let emitter = CronEventEmitter::new(services.event_bus.clone());
    let cron_service = Arc::new(aionui_cron::service::CronService::new(
        cron_repo, scheduler, executor, emitter,
    ));

    tick_service_ref
        .0
        .lock()
        .unwrap()
        .replace(cron_service.clone());

    CronRouterState {
        cron_service,
        conversation_service: conv_service,
    }
}

/// Build the default `OfficeRouterState` from application services.
pub fn build_office_state(services: &AppServices) -> OfficeRouterState {
    let data_dir = std::path::Path::new(&services.data_dir);

    let spawner: Arc<dyn aionui_office::ProcessSpawner> =
        Arc::new(aionui_office::DefaultProcessSpawner);
    let watch_manager = Arc::new(OfficecliWatchManager::new(
        spawner,
        services.event_bus.clone(),
    ));

    let snapshot_service = Arc::new(OfficeSnapshotService::new(data_dir));
    let star_office_detector = Arc::new(StarOfficeDetector::new(reqwest::Client::new()));
    let conversion_service = Arc::new(ConversionService::new(None));
    let proxy_service = Arc::new(ProxyService::new(watch_manager.clone()));

    OfficeRouterState {
        watch_manager,
        snapshot_service,
        star_office_detector,
        conversion_service,
        proxy_service,
    }
}

/// Build the default `ShellRouterState` from application services.
pub fn build_shell_state(services: &AppServices) -> ShellRouterState {
    let pool = services.database.pool().clone();
    let client_pref_repo = Arc::new(SqliteClientPreferenceRepository::new(pool));
    let client_pref_service = ClientPrefService::new(client_pref_repo);

    ShellRouterState {
        shell_service: Arc::new(aionui_shell::ShellService::new(Arc::new(
            aionui_shell::DefaultSystemOpener,
        ))),
        stt_service: Arc::new(aionui_shell::SttService::new(reqwest::Client::new())),
        client_pref_service,
    }
}

/// Helper to break the circular reference between CronScheduler and CronService.
///
/// The scheduler's tick callback needs to call `CronService::tick()`, but
/// `CronService` owns the scheduler. We use a `Mutex<Option<Arc<CronService>>>`
/// that gets populated after both are constructed.
#[derive(Default)]
struct CronServiceTickRef(std::sync::Mutex<Option<Arc<aionui_cron::service::CronService>>>);

/// Build the default extension-related router states.
///
/// Returns `(ExtensionRouterState, HubRouterState, SkillRouterState)`.
pub async fn build_extension_states(
    services: &AppServices,
) -> (ExtensionRouterState, HubRouterState, SkillRouterState) {
    let data_dir = dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".aionui");

    let state_store = ExtensionStateStore::new(data_dir.join("extension-states.json"));
    let registry = ExtensionRegistry::new(
        state_store,
        services.event_bus.clone(),
        env!("CARGO_PKG_VERSION").to_string(),
    );

    let hub_dir = data_dir.join("extensions");
    let index_manager = HubIndexManager::new(hub_dir, registry.clone());
    let installer = HubInstaller::new(index_manager.clone(), registry.clone());

    // Skill paths: use app resource dir (binary's parent) for built-in resources.
    let app_resource_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|pp| pp.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let skill_paths = aionui_extension::resolve_skill_paths(&app_resource_dir);

    let ext_paths_mgr = Arc::new(ExternalPathsManager::new(&data_dir).await);

    let ext_state = ExtensionRouterState {
        registry: registry.clone(),
    };

    let hub_state = HubRouterState {
        index_manager,
        installer,
    };

    let skill_state = SkillRouterState {
        skill_paths,
        external_paths_manager: ext_paths_mgr,
    };

    (ext_state, hub_state, skill_state)
}

/// Build the default `WsHandlerState` from application services.
///
/// Tests can call this and override individual fields before passing
/// to [`create_router_with_ws_state`].
pub fn build_ws_state(services: &AppServices) -> WsHandlerState {
    let jwt_service = services.jwt_service.clone();
    let token_validator = Arc::new(move |token: &str| jwt_service.verify(token).is_ok());

    let token_extractor =
        Arc::new(|headers: &axum::http::HeaderMap| extract_token_from_ws_headers(headers));

    WsHandlerState {
        manager: services.ws_manager.clone(),
        router: Arc::new(NoopMessageRouter),
        token_validator,
        token_extractor,
    }
}

/// Create the application router with custom module states.
///
/// Used for testing when specific service overrides are needed
/// (e.g. injecting a mock HTTP server URL for version check).
pub fn create_router_with_states(services: &AppServices, states: ModuleStates) -> Router {
    let ws_state = build_ws_state(services);
    create_router_with_all_state(services, states, ws_state)
}

/// Create the application router with custom module states and WebSocket state.
///
/// Full-control variant used by tests that need to override
/// module services and WebSocket behaviour.
pub fn create_router_with_all_state(
    services: &AppServices,
    states: ModuleStates,
    ws_state: WsHandlerState,
) -> Router {
    let auth_state = AuthRouterState {
        jwt_service: services.jwt_service.clone(),
        user_repo: services.user_repo.clone(),
        cookie_config: services.cookie_config.clone(),
        qr_token_store: services.qr_token_store.clone(),
    };

    let auth_mw_state = AuthState {
        jwt_service: services.jwt_service.clone(),
        user_repo: services.user_repo.clone(),
        local: services.local,
    };

    // System routes protected by auth middleware
    let system_authenticated = system_routes(states.system)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Conversation routes protected by auth middleware
    let conversation_authenticated = conversation_routes(states.conversation)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Remote agent routes protected by auth middleware
    let remote_agent_authenticated = remote_agent_routes(states.remote_agent)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // ACP management routes protected by auth middleware
    let acp_authenticated = acp_routes(states.acp)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Connection test routes (Bedrock, Gemini) protected by auth middleware
    let connection_test_authenticated = connection_test_routes(states.connection_test)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Auxiliary routes (workspace, side-question, reload-context, slash-commands, openclaw runtime)
    let auxiliary_authenticated = auxiliary_routes(states.auxiliary)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // File routes protected by auth middleware
    let file_authenticated = file_routes(states.file)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // MCP routes protected by auth middleware
    let mcp_authenticated = mcp_routes(states.mcp)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Extension routes protected by auth middleware
    let extension_authenticated = extension_routes(states.extension)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Hub routes protected by auth middleware
    let hub_authenticated = hub_routes(states.hub)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Skill routes protected by auth middleware
    let skill_authenticated = skill_routes(states.skill)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Channel routes protected by auth middleware
    #[cfg(feature = "weixin")]
    let weixin_login_authenticated = weixin_login_route(states.channel.clone())
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));
    let channel_authenticated = channel_routes(states.channel)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Team routes protected by auth middleware
    let team_authenticated = team_routes(states.team)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Cron routes protected by auth middleware
    let cron_authenticated = cron_routes(states.cron)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Office routes protected by auth middleware
    let office_authenticated = office_routes(states.office.clone())
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Shell + STT routes protected by auth middleware
    let shell_authenticated =
        shell_routes(states.shell).route_layer(from_fn_with_state(auth_mw_state, auth_middleware));

    // Office proxy routes — exempt from auth (serve iframe content)
    let office_proxy = office_proxy_routes(states.office);

    // WebSocket upgrade route — exempt from CSRF (no cookie-based
    // double-submit) but still gets security response headers.
    let ws_routes = Router::new()
        .route("/ws", get(ws_upgrade_handler))
        .with_state(ws_state);

    let router = Router::new()
        .route("/health", get(health_check))
        .merge(auth_routes(auth_state))
        .merge(system_authenticated)
        .merge(conversation_authenticated)
        .merge(remote_agent_authenticated)
        .merge(acp_authenticated)
        .merge(connection_test_authenticated)
        .merge(auxiliary_authenticated)
        .merge(file_authenticated)
        .merge(mcp_authenticated)
        .merge(extension_authenticated)
        .merge(hub_authenticated)
        .merge(skill_authenticated)
        .merge(channel_authenticated)
        .merge(team_authenticated)
        .merge(cron_authenticated)
        .merge(office_authenticated)
        .merge(shell_authenticated);

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
    .layer(middleware::from_fn(security_headers_middleware));

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
        assert_eq!(config.data_dir, "data");
    }

    #[test]
    fn test_app_config_socket_addr() {
        let config = AppConfig {
            host: "0.0.0.0".to_string(),
            port: 3000,
            data_dir: "data".to_string(),
            local: false,
        };
        assert_eq!(config.socket_addr(), "0.0.0.0:3000");
    }

    #[test]
    fn test_app_config_database_path() {
        let config = AppConfig {
            host: "127.0.0.1".to_string(),
            port: 25808,
            data_dir: "/tmp/aionui".to_string(),
            local: false,
        };
        assert_eq!(
            config.database_path(),
            std::path::PathBuf::from("/tmp/aionui/aionui.db")
        );
    }

    #[tokio::test]
    async fn test_app_services_from_memory_db() {
        let db = aionui_db::init_database_memory().await.unwrap();
        let services = AppServices::from_database(db).await.unwrap();

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
        let services = AppServices::from_database(db).await.unwrap();

        // System user should now have a jwt_secret persisted
        let system_user = services.user_repo.get_system_user().await.unwrap();
        let jwt_secret = system_user.unwrap().jwt_secret;
        assert!(jwt_secret.is_some());
        assert!(!jwt_secret.unwrap().is_empty());

        services.database.close().await;
    }
}
