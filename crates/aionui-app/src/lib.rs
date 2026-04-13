use std::sync::Arc;

use axum::middleware::from_fn_with_state;
use axum::routing::get;
use axum::{Json, Router, middleware};
use serde::Serialize;
use sha2::{Digest, Sha256};

use aionui_ai_agent::{
    AcpRouterState, AgentFactory, ConnectionTestRouterState, ConnectionTestService,
    IWorkerTaskManager, RemoteAgentRouterState, RemoteAgentService, WorkerTaskManagerImpl,
    acp_routes, connection_test_routes, remote_agent_routes,
};
use aionui_auth::{
    AuthRouterState, AuthState, CookieConfig, JwtService, QrTokenStore, auth_middleware,
    auth_routes, csrf_middleware, extract_token_from_ws_headers, resolve_jwt_secret,
    security_headers_middleware,
};
use aionui_common::AppError;
use aionui_conversation::{ConversationRouterState, ConversationService, conversation_routes};
use aionui_db::{
    Database, IUserRepository, SqliteClientPreferenceRepository, SqliteConversationRepository,
    SqliteProviderRepository, SqliteRemoteAgentRepository, SqliteSettingsRepository,
    SqliteUserRepository,
};
use aionui_realtime::{
    BroadcastEventBus, NoopMessageRouter, WebSocketManager, WsHandlerState, ws_upgrade_handler,
};
use aionui_system::{
    ClientPrefService, ModelFetchService, ProtocolDetectionService, ProviderService,
    SettingsService, SystemRouterState, VersionCheckService, system_routes,
};

/// Application configuration parsed from CLI arguments.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub data_dir: String,
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
    /// Raw JWT secret string, used to derive encryption keys.
    pub jwt_secret_raw: String,
}

impl AppServices {
    /// Build application services from an initialized database.
    ///
    /// Resolves JWT secret (env → db → generate), constructs all shared
    /// services, and persists a newly generated secret to the database.
    pub async fn from_database(database: Database) -> anyhow::Result<Self> {
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
        if is_new
            && let Some(user) = &system_user
        {
            user_repo
                .update_jwt_secret(&user.id, &secret)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to persist JWT secret: {e}"))?;
            tracing::info!("Generated and persisted new JWT secret");
        }

        // Stub factory — real agent construction will be wired in task 6.15
        let stub_factory: AgentFactory = Arc::new(|_| {
            Err(AppError::Internal("Agent factory not configured".into()))
        });
        let worker_task_manager: Arc<dyn IWorkerTaskManager> =
            Arc::new(WorkerTaskManagerImpl::new(stub_factory));

        Ok(Self {
            database,
            jwt_service: Arc::new(JwtService::new(secret.clone())),
            user_repo,
            cookie_config: Arc::new(CookieConfig::from_env()),
            qr_token_store: Arc::new(QrTokenStore::new()),
            ws_manager: Arc::new(WebSocketManager::new()),
            event_bus: Arc::new(BroadcastEventBus::new(256)),
            worker_task_manager,
            jwt_secret_raw: secret,
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

/// Build the default `SystemRouterState` from application services.
///
/// Tests can call this and override individual fields before passing
/// to [`create_router_with_system_state`].
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
/// 3. Route handlers (auth routes + system routes + conversation routes + health check)
pub fn create_router(services: &AppServices) -> Router {
    let system_state = build_system_state(services);
    let conversation_state = build_conversation_state(services);
    let remote_agent_state = build_remote_agent_state(services);
    let acp_state = build_acp_state(services);
    let connection_test_state = build_connection_test_state();
    create_router_with_system_state(
        services,
        system_state,
        conversation_state,
        remote_agent_state,
        acp_state,
        connection_test_state,
    )
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
    }
}

/// Build the default `ConnectionTestRouterState`.
pub fn build_connection_test_state() -> ConnectionTestRouterState {
    ConnectionTestRouterState {
        service: ConnectionTestService::new(reqwest::Client::new()),
    }
}

/// Build the default `WsHandlerState` from application services.
///
/// Tests can call this and override individual fields before passing
/// to [`create_router_with_ws_state`].
pub fn build_ws_state(services: &AppServices) -> WsHandlerState {
    let jwt_service = services.jwt_service.clone();
    let token_validator = Arc::new(move |token: &str| jwt_service.verify(token).is_ok());

    let token_extractor = Arc::new(|headers: &axum::http::HeaderMap| {
        extract_token_from_ws_headers(headers)
    });

    WsHandlerState {
        manager: services.ws_manager.clone(),
        router: Arc::new(NoopMessageRouter),
        token_validator,
        token_extractor,
    }
}

/// Create the application router with a custom system state.
///
/// Used for testing when specific service overrides are needed
/// (e.g. injecting a mock HTTP server URL for version check).
pub fn create_router_with_system_state(
    services: &AppServices,
    system_state: SystemRouterState,
    conversation_state: ConversationRouterState,
    remote_agent_state: RemoteAgentRouterState,
    acp_state: AcpRouterState,
    connection_test_state: ConnectionTestRouterState,
) -> Router {
    let ws_state = build_ws_state(services);
    create_router_with_all_state(
        services,
        system_state,
        conversation_state,
        remote_agent_state,
        acp_state,
        connection_test_state,
        ws_state,
    )
}

/// Create the application router with custom system, conversation, and WebSocket state.
///
/// Full-control variant used by tests that need to override
/// system services, conversation services, and WebSocket behaviour.
pub fn create_router_with_all_state(
    services: &AppServices,
    system_state: SystemRouterState,
    conversation_state: ConversationRouterState,
    remote_agent_state: RemoteAgentRouterState,
    acp_state: AcpRouterState,
    connection_test_state: ConnectionTestRouterState,
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
    };

    // System routes protected by auth middleware
    let system_authenticated = system_routes(system_state)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Conversation routes protected by auth middleware
    let conversation_authenticated = conversation_routes(conversation_state)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Remote agent routes protected by auth middleware
    let remote_agent_authenticated = remote_agent_routes(remote_agent_state)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // ACP management routes protected by auth middleware
    let acp_authenticated = acp_routes(acp_state)
        .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

    // Connection test routes (Bedrock, Gemini) protected by auth middleware
    let connection_test_authenticated = connection_test_routes(connection_test_state)
        .route_layer(from_fn_with_state(auth_mw_state, auth_middleware));

    // WebSocket upgrade route — exempt from CSRF (no cookie-based
    // double-submit) but still gets security response headers.
    let ws_routes = Router::new()
        .route("/ws", get(ws_upgrade_handler))
        .with_state(ws_state);

    Router::new()
        .route("/health", get(health_check))
        .merge(auth_routes(auth_state))
        .merge(system_authenticated)
        .merge(conversation_authenticated)
        .merge(remote_agent_authenticated)
        .merge(acp_authenticated)
        .merge(connection_test_authenticated)
        .layer(middleware::from_fn_with_state(
            services.cookie_config.clone(),
            csrf_middleware,
        ))
        .merge(ws_routes)
        .layer(middleware::from_fn(security_headers_middleware))
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
        };
        assert_eq!(config.socket_addr(), "0.0.0.0:3000");
    }

    #[test]
    fn test_app_config_database_path() {
        let config = AppConfig {
            host: "127.0.0.1".to_string(),
            port: 25808,
            data_dir: "/tmp/aionui".to_string(),
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
        let token = services
            .jwt_service
            .sign("test_user", "testuser")
            .unwrap();
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
