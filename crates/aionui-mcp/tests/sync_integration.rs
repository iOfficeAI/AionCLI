//! Integration tests for McpSyncService.
//!
//! Tests from test-plan §3 (Agent Sync) at the service layer.
//! Uses mock adapters (real CLI tests are environment-dependent).

use std::collections::HashMap;
use std::sync::Arc;

use aionui_api_types::{CreateMcpServerRequest, McpTransport};
use aionui_common::McpSource;
use aionui_db::SqliteMcpServerRepository;
use aionui_mcp::{
    DetectedServer, McpAgentAdapter, McpConfigService, McpError, McpServerTransport,
    McpSyncService,
};

// ---------------------------------------------------------------------------
// Mock adapter for integration tests
// ---------------------------------------------------------------------------

struct MockAdapter {
    source: McpSource,
    installed: bool,
    servers: std::sync::Mutex<Vec<DetectedServer>>,
}

impl MockAdapter {
    fn new(source: McpSource, installed: bool) -> Self {
        Self {
            source,
            installed,
            servers: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn with_servers(source: McpSource, servers: Vec<DetectedServer>) -> Self {
        Self {
            source,
            installed: true,
            servers: std::sync::Mutex::new(servers),
        }
    }

    fn current_servers(&self) -> Vec<DetectedServer> {
        self.servers.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl McpAgentAdapter for MockAdapter {
    fn source(&self) -> McpSource {
        self.source
    }

    async fn is_installed(&self) -> Result<bool, McpError> {
        Ok(self.installed)
    }

    async fn detect_existing(&self) -> Result<Vec<DetectedServer>, McpError> {
        if !self.installed {
            return Err(McpError::AgentNotInstalled(format!("{:?}", self.source)));
        }
        Ok(self.servers.lock().unwrap().clone())
    }

    async fn install_server(
        &self,
        name: &str,
        transport: &McpServerTransport,
    ) -> Result<(), McpError> {
        if !self.installed {
            return Err(McpError::AgentNotInstalled(format!("{:?}", self.source)));
        }
        let mut servers = self.servers.lock().unwrap();
        servers.retain(|s| s.name != name);
        servers.push(DetectedServer {
            name: name.to_owned(),
            transport: transport.clone(),
        });
        Ok(())
    }

    async fn remove_server(&self, name: &str) -> Result<(), McpError> {
        if !self.installed {
            return Err(McpError::AgentNotInstalled(format!("{:?}", self.source)));
        }
        let mut servers = self.servers.lock().unwrap();
        servers.retain(|s| s.name != name);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn make_services(
    adapters: Vec<Arc<dyn McpAgentAdapter>>,
) -> (McpConfigService, McpSyncService) {
    let db = aionui_db::init_database_memory().await.unwrap();
    let repo: Arc<dyn aionui_db::IMcpServerRepository> =
        Arc::new(SqliteMcpServerRepository::new(db.pool().clone()));
    let config_svc = McpConfigService::new(repo.clone());
    let sync_svc = McpSyncService::new(repo, adapters);
    (config_svc, sync_svc)
}

fn stdio_req(name: &str) -> CreateMcpServerRequest {
    CreateMcpServerRequest {
        name: name.to_owned(),
        description: Some("test".to_owned()),
        transport: McpTransport::Stdio {
            command: "npx".into(),
            args: vec!["-y".into(), "@test/server".into()],
            env: HashMap::new(),
        },
        original_json: None,
        builtin: false,
    }
}

fn http_req(name: &str) -> CreateMcpServerRequest {
    CreateMcpServerRequest {
        name: name.to_owned(),
        description: None,
        transport: McpTransport::Http {
            url: "https://example.com/mcp".into(),
            headers: HashMap::new(),
        },
        original_json: None,
        builtin: false,
    }
}

fn stdio_transport() -> McpServerTransport {
    McpServerTransport::Stdio {
        command: "npx".into(),
        args: vec!["-y".into(), "@test/server".into()],
        env: HashMap::new(),
    }
}

// ---------------------------------------------------------------------------
// AS-1: Get all Agent MCP configs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_agent_configs_returns_installed_agents() {
    let adapter_claude = Arc::new(MockAdapter::with_servers(
        McpSource::Claude,
        vec![DetectedServer {
            name: "existing-srv".into(),
            transport: stdio_transport(),
        }],
    ));
    let adapter_gemini = Arc::new(MockAdapter::new(McpSource::Gemini, false));
    let adapter_qwen = Arc::new(MockAdapter::new(McpSource::Qwen, true));

    let (_config_svc, sync_svc) = make_services(vec![
        adapter_claude as Arc<dyn McpAgentAdapter>,
        adapter_gemini,
        adapter_qwen,
    ])
    .await;

    let configs = sync_svc.get_agent_configs().await.unwrap();

    // Claude installed with 1 server, Qwen installed with 0 servers
    // Gemini not installed → skipped
    assert_eq!(configs.len(), 2);
    assert_eq!(configs[0].source, McpSource::Claude);
    assert_eq!(configs[0].servers.len(), 1);
    assert_eq!(configs[0].servers[0].name, "existing-srv");
    assert_eq!(configs[1].source, McpSource::Qwen);
    assert!(configs[1].servers.is_empty());
}

// ---------------------------------------------------------------------------
// AS-2: No Agent installed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_agent_configs_empty_when_none_installed() {
    let adapter = Arc::new(MockAdapter::new(McpSource::Claude, false));
    let (_config_svc, sync_svc) =
        make_services(vec![adapter as Arc<dyn McpAgentAdapter>]).await;

    let configs = sync_svc.get_agent_configs().await.unwrap();
    assert!(configs.is_empty());
}

// ---------------------------------------------------------------------------
// AS-3: Sync servers to all agents
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sync_to_agents_installs_servers() {
    let adapter: Arc<MockAdapter> = Arc::new(MockAdapter::new(McpSource::Claude, true));
    let (config_svc, sync_svc) =
        make_services(vec![adapter.clone() as Arc<dyn McpAgentAdapter>]).await;

    // Create a server in DB
    let server = config_svc.add_server(stdio_req("test-srv")).await.unwrap();

    // Sync it
    let result = sync_svc.sync_to_agents(&[server.id]).await.unwrap();

    assert!(result.success);
    assert_eq!(result.results.len(), 1);
    assert!(result.results[0].success);

    // Verify adapter received it
    let current = adapter.current_servers();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].name, "test-srv");
}

// ---------------------------------------------------------------------------
// AS-4: Sync empty list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sync_empty_server_list_succeeds() {
    let adapter: Arc<dyn McpAgentAdapter> =
        Arc::new(MockAdapter::new(McpSource::Claude, true));
    let (_config_svc, sync_svc) = make_services(vec![adapter]).await;

    let result = sync_svc.sync_to_agents(&[]).await.unwrap();
    assert!(result.success);
}

// ---------------------------------------------------------------------------
// AS-5: Partial agent failure
// ---------------------------------------------------------------------------

/// Failing adapter that always errors on install.
struct FailingAdapter;

#[async_trait::async_trait]
impl McpAgentAdapter for FailingAdapter {
    fn source(&self) -> McpSource {
        McpSource::Gemini
    }
    async fn is_installed(&self) -> Result<bool, McpError> {
        Ok(true)
    }
    async fn detect_existing(&self) -> Result<Vec<DetectedServer>, McpError> {
        Ok(vec![])
    }
    async fn install_server(
        &self,
        _name: &str,
        _transport: &McpServerTransport,
    ) -> Result<(), McpError> {
        Err(McpError::AgentOperationFailed("CLI not found".into()))
    }
    async fn remove_server(&self, _name: &str) -> Result<(), McpError> {
        Ok(())
    }
}

#[tokio::test]
async fn sync_partial_failure_reports_per_agent() {
    let ok_adapter: Arc<dyn McpAgentAdapter> =
        Arc::new(MockAdapter::new(McpSource::Claude, true));
    let fail_adapter: Arc<dyn McpAgentAdapter> = Arc::new(FailingAdapter);

    let (config_svc, sync_svc) = make_services(vec![ok_adapter, fail_adapter]).await;

    let server = config_svc.add_server(stdio_req("test-srv")).await.unwrap();
    let result = sync_svc.sync_to_agents(&[server.id]).await.unwrap();

    // Overall failure because Gemini failed
    assert!(!result.success);
    assert_eq!(result.results.len(), 2);
    assert!(result.results[0].success); // Claude OK
    assert!(!result.results[1].success); // Gemini failed
    assert!(result.results[1].error.is_some());
}

// ---------------------------------------------------------------------------
// AS-6: Remove from agents
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remove_from_agents_removes_servers() {
    let adapter: Arc<MockAdapter> = Arc::new(MockAdapter::with_servers(
        McpSource::Claude,
        vec![DetectedServer {
            name: "srv-to-remove".into(),
            transport: stdio_transport(),
        }],
    ));
    let (_config_svc, sync_svc) =
        make_services(vec![adapter.clone() as Arc<dyn McpAgentAdapter>]).await;

    let result = sync_svc
        .remove_from_agents(&["srv-to-remove".into()])
        .await
        .unwrap();

    assert!(result.success);
    assert!(adapter.current_servers().is_empty());
}

// ---------------------------------------------------------------------------
// AS-7: Remove non-existent server name
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remove_nonexistent_server_name_succeeds() {
    let adapter: Arc<dyn McpAgentAdapter> =
        Arc::new(MockAdapter::new(McpSource::Claude, true));
    let (_config_svc, sync_svc) = make_services(vec![adapter]).await;

    let result = sync_svc
        .remove_from_agents(&["does-not-exist".into()])
        .await
        .unwrap();

    // remove_server is idempotent → success
    assert!(result.success);
}

// ---------------------------------------------------------------------------
// Sync diff: skip identical, replace changed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sync_skips_identical_and_replaces_changed() {
    let adapter: Arc<MockAdapter> = Arc::new(MockAdapter::with_servers(
        McpSource::Claude,
        vec![DetectedServer {
            name: "unchanged-srv".into(),
            transport: stdio_transport(),
        }],
    ));

    let (config_svc, sync_svc) =
        make_services(vec![adapter.clone() as Arc<dyn McpAgentAdapter>]).await;

    // Create two servers: one with same transport (should skip), one new
    let srv_same = config_svc
        .add_server(stdio_req("unchanged-srv"))
        .await
        .unwrap();
    let srv_new = config_svc
        .add_server(http_req("new-srv"))
        .await
        .unwrap();

    let result = sync_svc
        .sync_to_agents(&[srv_same.id, srv_new.id])
        .await
        .unwrap();
    assert!(result.success);

    let current = adapter.current_servers();
    assert_eq!(current.len(), 2);
    // unchanged-srv should still have stdio transport (skipped)
    let unchanged = current.iter().find(|s| s.name == "unchanged-srv").unwrap();
    assert_eq!(unchanged.transport, stdio_transport());
    // new-srv installed
    assert!(current.iter().any(|s| s.name == "new-srv"));
}

// ---------------------------------------------------------------------------
// CC-1: Concurrent sync requests don't interleave
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_sync_requests_are_serialized() {
    let adapter: Arc<dyn McpAgentAdapter> =
        Arc::new(MockAdapter::new(McpSource::Claude, true));
    let (config_svc, sync_svc) = make_services(vec![adapter]).await;

    let srv1 = config_svc.add_server(stdio_req("srv1")).await.unwrap();
    let srv2 = config_svc.add_server(http_req("srv2")).await.unwrap();

    let svc1 = sync_svc.clone();
    let svc2 = sync_svc.clone();
    let id1 = srv1.id.clone();
    let id2 = srv2.id.clone();

    // Spawn two concurrent sync operations
    let (r1, r2) = tokio::join!(
        tokio::spawn(async move { svc1.sync_to_agents(&[id1]).await }),
        tokio::spawn(async move { svc2.sync_to_agents(&[id2]).await }),
    );

    // Both should succeed without panic or data corruption
    let r1 = r1.unwrap().unwrap();
    let r2 = r2.unwrap().unwrap();
    assert!(r1.success);
    assert!(r2.success);
}

// ---------------------------------------------------------------------------
// CC-2: Concurrent CRUD + sync
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_crud_and_sync_no_panic() {
    let adapter: Arc<dyn McpAgentAdapter> =
        Arc::new(MockAdapter::new(McpSource::Claude, true));
    let (config_svc, sync_svc) = make_services(vec![adapter]).await;

    let srv = config_svc.add_server(stdio_req("srv")).await.unwrap();
    let srv_id = srv.id.clone();

    let svc = sync_svc.clone();
    let cfg = config_svc.clone();

    // Spawn sync and CRUD concurrently
    let (sync_result, crud_result) = tokio::join!(
        tokio::spawn(async move { svc.sync_to_agents(&[srv_id]).await }),
        tokio::spawn(async move {
            cfg.add_server(http_req("another-srv")).await
        }),
    );

    // Neither should panic
    let sync_r = sync_result.unwrap().unwrap();
    assert!(sync_r.success);
    let crud_r = crud_result.unwrap();
    assert!(crud_r.is_ok());
}
