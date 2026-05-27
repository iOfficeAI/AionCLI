use std::sync::Arc;

use aionui_api_types::{DetectedMcpServerResponse, McpAgentSyncResult, McpSyncResult};
use aionui_common::McpSource;
use aionui_db::IMcpServerRepository;
use dashmap::DashMap;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::adapter::{DetectedServer, McpAgentAdapter};
use crate::error::McpError;
use crate::types::McpServer;

// ---------------------------------------------------------------------------
// McpSyncService
// ---------------------------------------------------------------------------

/// Orchestrates MCP configuration sync across multiple Agent CLIs.
///
/// Provides three heavy operations — `get_agent_configs`, `sync_to_agents`,
/// and `remove_from_agents` — behind a service-level lock that prevents
/// concurrent execution.  Each adapter also has a per-agent lock so that
/// overlapping requests to the same agent are serialised even when the
/// service lock is not held.
#[derive(Clone)]
pub struct McpSyncService {
    repo: Arc<dyn IMcpServerRepository>,
    adapters: Arc<Vec<Arc<dyn McpAgentAdapter>>>,

    /// Service-level lock: prevents sync/remove/detect from running
    /// concurrently.  These operations may spawn many child processes,
    /// and running them in parallel risks exhausting system resources.
    service_lock: Arc<Mutex<()>>,

    /// Per-agent locks: ensures operations on the same agent are
    /// serialised (e.g. two rapid toggles on the same CLI won't
    /// interleave CLI commands).
    agent_locks: Arc<DashMap<McpSource, Arc<Mutex<()>>>>,
}

impl McpSyncService {
    pub fn new(repo: Arc<dyn IMcpServerRepository>, adapters: Vec<Arc<dyn McpAgentAdapter>>) -> Self {
        Self {
            repo,
            adapters: Arc::new(adapters),
            service_lock: Arc::new(Mutex::new(())),
            agent_locks: Arc::new(DashMap::new()),
        }
    }

    // -- public API ----------------------------------------------------------

    /// Scan all installed Agent CLIs and return each one's current MCP
    /// server configurations.
    ///
    /// Agents that are not installed are silently skipped.
    pub async fn get_agent_configs(&self) -> Result<Vec<DetectedMcpServerResponse>, McpError> {
        let _guard = self.service_lock.lock().await;

        let mut results = Vec::new();
        for adapter in self.adapters.iter() {
            let _agent_guard = self.agent_lock(adapter.source()).await;

            let installed = adapter.is_installed().await.unwrap_or(false);
            if !installed {
                continue;
            }

            match adapter.detect_existing().await {
                Ok(detected) => {
                    let servers = detected.into_iter().map(detected_to_response).collect();
                    results.push(DetectedMcpServerResponse {
                        source: adapter.source(),
                        servers,
                    });
                }
                Err(e) => {
                    warn!(
                        agent = ?adapter.source(),
                        error = %e,
                        "failed to detect existing MCP servers"
                    );
                }
            }
        }

        Ok(results)
    }

    /// Sync the specified MCP servers to all installed Agent CLIs.
    ///
    /// For each adapter the algorithm is:
    /// 1. `detect_existing()` — read what the agent currently has
    /// 2. Diff against the requested servers
    /// 3. Remove servers that exist but differ in transport config
    /// 4. Install servers that are new or were just removed (changed)
    pub async fn sync_to_agents(&self, server_ids: &[String]) -> Result<McpSyncResult, McpError> {
        let _guard = self.service_lock.lock().await;

        let servers = self.load_servers_by_ids(server_ids).await?;
        info!(
            requested_count = server_ids.len(),
            loaded_count = servers.len(),
            "syncing MCP servers to agents"
        );
        if !server_ids.is_empty() && servers.is_empty() {
            warn!(
                requested_count = server_ids.len(),
                "requested MCP sync loaded zero servers"
            );
        }

        let mut agent_results = Vec::new();
        for adapter in self.adapters.iter() {
            let _agent_guard = self.agent_lock(adapter.source()).await;

            let result = self.sync_adapter(adapter.as_ref(), &servers).await;
            agent_results.push(result);
        }

        let all_ok = agent_results.iter().all(|r| r.success);
        Ok(McpSyncResult {
            success: all_ok,
            results: agent_results,
        })
    }

    /// Remove the named MCP servers from all installed Agent CLIs.
    pub async fn remove_from_agents(&self, server_names: &[String]) -> Result<McpSyncResult, McpError> {
        let _guard = self.service_lock.lock().await;

        info!(names = ?server_names, "removing MCP servers from agents");

        let mut agent_results = Vec::new();
        for adapter in self.adapters.iter() {
            let _agent_guard = self.agent_lock(adapter.source()).await;

            let result = self.remove_from_adapter(adapter.as_ref(), server_names).await;
            agent_results.push(result);
        }

        let all_ok = agent_results.iter().all(|r| r.success);
        Ok(McpSyncResult {
            success: all_ok,
            results: agent_results,
        })
    }

    // -- internal helpers ----------------------------------------------------

    /// Acquire the per-agent lock, creating it on first use.
    async fn agent_lock(&self, source: McpSource) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self
            .agent_locks
            .entry(source)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        lock.lock_owned().await
    }

    /// Load `McpServer` domain objects for the given IDs.
    ///
    /// Unknown IDs are silently skipped (the server may have been
    /// deleted between request validation and execution).
    async fn load_servers_by_ids(&self, ids: &[String]) -> Result<Vec<McpServer>, McpError> {
        let mut servers = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(row) = self.repo.find_by_id(id).await? {
                servers.push(McpServer::from_row(row)?);
            }
        }
        Ok(servers)
    }

    /// Sync servers to a single adapter.  Returns the per-agent result.
    async fn sync_adapter(&self, adapter: &dyn McpAgentAdapter, servers: &[McpServer]) -> McpAgentSyncResult {
        let source = adapter.source();

        // Check if installed
        match adapter.is_installed().await {
            Ok(false) => {
                return McpAgentSyncResult {
                    agent: source,
                    success: true,
                    error: None,
                };
            }
            Err(e) => {
                return McpAgentSyncResult {
                    agent: source,
                    success: false,
                    error: Some(format!("install check failed: {e}")),
                };
            }
            Ok(true) => {}
        }

        // Detect existing
        let existing = match adapter.detect_existing().await {
            Ok(list) => list,
            Err(e) => {
                warn!(agent = ?source, error = %e, "detect_existing failed");
                return McpAgentSyncResult {
                    agent: source,
                    success: false,
                    error: Some(format!("detect failed: {e}")),
                };
            }
        };

        // Diff and apply
        let mut errors: Vec<String> = Vec::new();
        for server in servers {
            let existing_match = existing.iter().find(|d| d.name == server.name);

            match existing_match {
                Some(detected) if detected.transport == server.transport => {
                    // Already exists with same transport — skip
                    continue;
                }
                Some(_) => {
                    // Exists but transport differs — remove then reinstall
                    if let Err(e) = adapter.remove_server(&server.name).await {
                        errors.push(format!("remove '{}': {e}", server.name));
                        continue;
                    }
                }
                None => {
                    // New server — install directly
                }
            }

            if let Err(e) = adapter.install_server(&server.name, &server.transport).await {
                errors.push(format!("install '{}': {e}", server.name));
            }
        }

        if errors.is_empty() {
            McpAgentSyncResult {
                agent: source,
                success: true,
                error: None,
            }
        } else {
            McpAgentSyncResult {
                agent: source,
                success: false,
                error: Some(errors.join("; ")),
            }
        }
    }

    /// Remove named servers from a single adapter.
    async fn remove_from_adapter(&self, adapter: &dyn McpAgentAdapter, names: &[String]) -> McpAgentSyncResult {
        let source = adapter.source();

        match adapter.is_installed().await {
            Ok(false) => {
                return McpAgentSyncResult {
                    agent: source,
                    success: true,
                    error: None,
                };
            }
            Err(e) => {
                return McpAgentSyncResult {
                    agent: source,
                    success: false,
                    error: Some(format!("install check failed: {e}")),
                };
            }
            Ok(true) => {}
        }

        let mut errors: Vec<String> = Vec::new();
        for name in names {
            if let Err(e) = adapter.remove_server(name).await {
                errors.push(format!("remove '{name}': {e}"));
            }
        }

        if errors.is_empty() {
            McpAgentSyncResult {
                agent: source,
                success: true,
                error: None,
            }
        } else {
            McpAgentSyncResult {
                agent: source,
                success: false,
                error: Some(errors.join("; ")),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a `DetectedServer` (name + transport) into a lightweight
/// `McpServerResponse` for the detection endpoint.
///
/// Fields that require DB context (id, timestamps, status, etc.) are
/// populated with sensible defaults.
fn detected_to_response(detected: DetectedServer) -> aionui_api_types::McpServerResponse {
    aionui_api_types::McpServerResponse {
        id: format!("detected_{}", detected.name),
        name: detected.name,
        description: None,
        enabled: false,
        transport: detected.transport.into(),
        tools: None,
        status: aionui_common::McpServerStatus::Disconnected,
        last_connected: None,
        original_json: None,
        builtin: false,
        created_at: 0,
        updated_at: 0,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::McpAgentAdapter;
    use crate::types::McpServerTransport;
    use aionui_common::{McpServerStatus, TimestampMs};
    use aionui_db::models::McpServerRow;
    use aionui_db::{CreateMcpServerParams, DbError, UpdateMcpServerParams};
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    // -- Mock adapter --------------------------------------------------------

    struct MockAdapter {
        source: McpSource,
        installed: bool,
        servers: Arc<StdMutex<Vec<DetectedServer>>>,
        install_fail: bool,
        remove_fail: bool,
    }

    impl MockAdapter {
        fn new(source: McpSource, installed: bool) -> Self {
            Self {
                source,
                installed,
                servers: Arc::new(StdMutex::new(Vec::new())),
                install_fail: false,
                remove_fail: false,
            }
        }

        fn with_existing(mut self, servers: Vec<DetectedServer>) -> Self {
            self.servers = Arc::new(StdMutex::new(servers));
            self
        }

        fn with_install_fail(mut self) -> Self {
            self.install_fail = true;
            self
        }

        fn with_remove_fail(mut self) -> Self {
            self.remove_fail = true;
            self
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

        async fn install_server(&self, name: &str, transport: &McpServerTransport) -> Result<(), McpError> {
            if self.install_fail {
                return Err(McpError::AgentOperationFailed("mock install failure".into()));
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
            if self.remove_fail {
                return Err(McpError::AgentOperationFailed("mock remove failure".into()));
            }
            let mut servers = self.servers.lock().unwrap();
            servers.retain(|s| s.name != name);
            Ok(())
        }
    }

    // -- Mock repository -----------------------------------------------------

    #[derive(Debug)]
    struct MockRepo {
        servers: StdMutex<Vec<McpServerRow>>,
    }

    impl MockRepo {
        fn new(servers: Vec<McpServerRow>) -> Self {
            Self {
                servers: StdMutex::new(servers),
            }
        }
    }

    fn make_row(id: &str, name: &str, transport_type: &str, config: &str) -> McpServerRow {
        McpServerRow {
            id: id.to_owned(),
            name: name.to_owned(),
            description: None,
            enabled: true,
            transport_type: transport_type.to_owned(),
            transport_config: config.to_owned(),
            tools: None,
            status: "disconnected".to_owned(),
            last_connected: None,
            original_json: None,
            builtin: false,
            created_at: 1000,
            updated_at: 1000,
        }
    }

    #[async_trait::async_trait]
    impl IMcpServerRepository for MockRepo {
        async fn list(&self) -> Result<Vec<McpServerRow>, DbError> {
            Ok(self.servers.lock().unwrap().clone())
        }

        async fn find_by_id(&self, id: &str) -> Result<Option<McpServerRow>, DbError> {
            let servers = self.servers.lock().unwrap();
            Ok(servers.iter().find(|s| s.id == id).cloned())
        }

        async fn find_by_name(&self, name: &str) -> Result<Option<McpServerRow>, DbError> {
            let servers = self.servers.lock().unwrap();
            Ok(servers.iter().find(|s| s.name == name).cloned())
        }

        async fn create(&self, _params: CreateMcpServerParams<'_>) -> Result<McpServerRow, DbError> {
            unimplemented!("not needed for sync tests")
        }

        async fn update(&self, _id: &str, _params: UpdateMcpServerParams<'_>) -> Result<McpServerRow, DbError> {
            unimplemented!("not needed for sync tests")
        }

        async fn delete(&self, _id: &str) -> Result<(), DbError> {
            unimplemented!("not needed for sync tests")
        }

        async fn batch_upsert(&self, _params_list: &[CreateMcpServerParams<'_>]) -> Result<Vec<McpServerRow>, DbError> {
            unimplemented!("not needed for sync tests")
        }

        async fn update_status(
            &self,
            _id: &str,
            _status: &str,
            _last_connected: Option<TimestampMs>,
        ) -> Result<(), DbError> {
            unimplemented!("not needed for sync tests")
        }

        async fn update_tools(&self, _id: &str, _tools: Option<&str>) -> Result<(), DbError> {
            unimplemented!("not needed for sync tests")
        }
    }

    // -- Helper factories ----------------------------------------------------

    fn stdio_transport() -> McpServerTransport {
        McpServerTransport::Stdio {
            command: "npx".into(),
            args: vec!["-y".into(), "@test/server".into()],
            env: HashMap::new(),
        }
    }

    fn http_transport() -> McpServerTransport {
        McpServerTransport::Http {
            url: "https://example.com/mcp".into(),
            headers: HashMap::new(),
        }
    }

    fn make_service(rows: Vec<McpServerRow>, adapters: Vec<Arc<dyn McpAgentAdapter>>) -> McpSyncService {
        let repo = Arc::new(MockRepo::new(rows));
        McpSyncService::new(repo, adapters)
    }

    // -- get_agent_configs ---------------------------------------------------

    #[tokio::test]
    async fn get_agent_configs_returns_installed_only() {
        let adapter_a = Arc::new(
            MockAdapter::new(McpSource::Claude, true).with_existing(vec![DetectedServer {
                name: "srv1".into(),
                transport: stdio_transport(),
            }]),
        );
        let adapter_b = Arc::new(MockAdapter::new(McpSource::Gemini, false));
        let adapter_c = Arc::new(MockAdapter::new(McpSource::Qwen, true).with_existing(vec![]));

        let svc = make_service(vec![], vec![adapter_a, adapter_b, adapter_c]);
        let configs = svc.get_agent_configs().await.unwrap();

        // Gemini not installed → skipped
        // Qwen installed but empty → still listed
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].source, McpSource::Claude);
        assert_eq!(configs[0].servers.len(), 1);
        assert_eq!(configs[0].servers[0].name, "srv1");
        assert_eq!(configs[1].source, McpSource::Qwen);
        assert!(configs[1].servers.is_empty());
    }

    #[tokio::test]
    async fn get_agent_configs_no_adapters() {
        let svc = make_service(vec![], vec![]);
        let configs = svc.get_agent_configs().await.unwrap();
        assert!(configs.is_empty());
    }

    // -- sync_to_agents ------------------------------------------------------

    #[tokio::test]
    async fn sync_installs_new_server() {
        let adapter: Arc<MockAdapter> = Arc::new(MockAdapter::new(McpSource::Claude, true));

        let row = make_row(
            "mcp_1",
            "test-srv",
            "stdio",
            r#"{"command":"npx","args":["-y","@test/server"],"env":{}}"#,
        );

        let svc = make_service(vec![row], vec![adapter.clone() as Arc<dyn McpAgentAdapter>]);
        let result = svc.sync_to_agents(&["mcp_1".into()]).await.unwrap();

        assert!(result.success);
        assert_eq!(result.results.len(), 1);
        assert!(result.results[0].success);

        let current = adapter.current_servers();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].name, "test-srv");
    }

    #[tokio::test]
    async fn sync_skips_identical_server() {
        let transport = stdio_transport();
        let adapter: Arc<MockAdapter> =
            Arc::new(
                MockAdapter::new(McpSource::Claude, true).with_existing(vec![DetectedServer {
                    name: "test-srv".into(),
                    transport: transport.clone(),
                }]),
            );

        let row = make_row(
            "mcp_1",
            "test-srv",
            "stdio",
            r#"{"command":"npx","args":["-y","@test/server"],"env":{}}"#,
        );

        let svc = make_service(vec![row], vec![adapter.clone() as Arc<dyn McpAgentAdapter>]);
        let result = svc.sync_to_agents(&["mcp_1".into()]).await.unwrap();

        assert!(result.success);
        // Server should still be there (not duplicated)
        let current = adapter.current_servers();
        assert_eq!(current.len(), 1);
    }

    #[tokio::test]
    async fn sync_replaces_changed_transport() {
        let old_transport = stdio_transport();
        let adapter: Arc<MockAdapter> =
            Arc::new(
                MockAdapter::new(McpSource::Claude, true).with_existing(vec![DetectedServer {
                    name: "test-srv".into(),
                    transport: old_transport,
                }]),
            );

        // Same name but now HTTP transport
        let row = make_row(
            "mcp_1",
            "test-srv",
            "http",
            r#"{"url":"https://example.com/mcp","headers":{}}"#,
        );

        let svc = make_service(vec![row], vec![adapter.clone() as Arc<dyn McpAgentAdapter>]);
        let result = svc.sync_to_agents(&["mcp_1".into()]).await.unwrap();

        assert!(result.success);
        let current = adapter.current_servers();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].transport, http_transport());
    }

    #[tokio::test]
    async fn sync_skips_not_installed_agent() {
        let adapter: Arc<MockAdapter> = Arc::new(MockAdapter::new(McpSource::Gemini, false));

        let row = make_row("mcp_1", "test-srv", "stdio", r#"{"command":"npx","args":[],"env":{}}"#);

        let svc = make_service(vec![row], vec![adapter as Arc<dyn McpAgentAdapter>]);
        let result = svc.sync_to_agents(&["mcp_1".into()]).await.unwrap();

        assert!(result.success);
        assert!(result.results[0].success);
    }

    #[tokio::test]
    async fn sync_reports_partial_failure() {
        let adapter: Arc<MockAdapter> = Arc::new(MockAdapter::new(McpSource::Claude, true).with_install_fail());

        let row = make_row("mcp_1", "fail-srv", "stdio", r#"{"command":"npx","args":[],"env":{}}"#);

        let svc = make_service(vec![row], vec![adapter as Arc<dyn McpAgentAdapter>]);
        let result = svc.sync_to_agents(&["mcp_1".into()]).await.unwrap();

        assert!(!result.success);
        assert!(!result.results[0].success);
        assert!(result.results[0].error.is_some());
    }

    #[tokio::test]
    async fn sync_empty_server_list() {
        let adapter: Arc<dyn McpAgentAdapter> = Arc::new(MockAdapter::new(McpSource::Claude, true));

        let svc = make_service(vec![], vec![adapter]);
        let result = svc.sync_to_agents(&[]).await.unwrap();

        assert!(result.success);
    }

    #[tokio::test]
    async fn sync_skips_unknown_id() {
        let adapter: Arc<dyn McpAgentAdapter> = Arc::new(MockAdapter::new(McpSource::Claude, true));

        let svc = make_service(vec![], vec![adapter]);
        let result = svc.sync_to_agents(&["nonexistent_id".into()]).await.unwrap();

        // No servers loaded → nothing to sync → success
        assert!(result.success);
    }

    // -- remove_from_agents --------------------------------------------------

    #[tokio::test]
    async fn remove_succeeds() {
        let adapter: Arc<MockAdapter> =
            Arc::new(
                MockAdapter::new(McpSource::Claude, true).with_existing(vec![DetectedServer {
                    name: "srv1".into(),
                    transport: stdio_transport(),
                }]),
            );

        let svc = make_service(vec![], vec![adapter.clone() as Arc<dyn McpAgentAdapter>]);
        let result = svc.remove_from_agents(&["srv1".into()]).await.unwrap();

        assert!(result.success);
        assert!(adapter.current_servers().is_empty());
    }

    #[tokio::test]
    async fn remove_nonexistent_is_ok() {
        let adapter: Arc<dyn McpAgentAdapter> = Arc::new(MockAdapter::new(McpSource::Claude, true));

        let svc = make_service(vec![], vec![adapter]);
        let result = svc.remove_from_agents(&["no-such-srv".into()]).await.unwrap();

        assert!(result.success);
    }

    #[tokio::test]
    async fn remove_reports_failure() {
        let adapter: Arc<dyn McpAgentAdapter> = Arc::new(MockAdapter::new(McpSource::Claude, true).with_remove_fail());

        let svc = make_service(vec![], vec![adapter]);
        let result = svc.remove_from_agents(&["some-srv".into()]).await.unwrap();

        assert!(!result.success);
        assert!(result.results[0].error.is_some());
    }

    #[tokio::test]
    async fn remove_skips_not_installed() {
        let adapter: Arc<dyn McpAgentAdapter> = Arc::new(MockAdapter::new(McpSource::Gemini, false));

        let svc = make_service(vec![], vec![adapter]);
        let result = svc.remove_from_agents(&["srv".into()]).await.unwrap();

        assert!(result.success);
        assert!(result.results[0].success);
    }

    #[tokio::test]
    async fn remove_empty_names_list() {
        let adapter: Arc<dyn McpAgentAdapter> = Arc::new(MockAdapter::new(McpSource::Claude, true));

        let svc = make_service(vec![], vec![adapter]);
        let result = svc.remove_from_agents(&[]).await.unwrap();

        assert!(result.success);
    }

    // -- multi-adapter scenarios ---------------------------------------------

    #[tokio::test]
    async fn sync_multiple_adapters_mixed_results() {
        let ok_adapter: Arc<dyn McpAgentAdapter> = Arc::new(MockAdapter::new(McpSource::Claude, true));
        let fail_adapter: Arc<dyn McpAgentAdapter> =
            Arc::new(MockAdapter::new(McpSource::Gemini, true).with_install_fail());
        let skip_adapter: Arc<dyn McpAgentAdapter> = Arc::new(MockAdapter::new(McpSource::Qwen, false));

        let row = make_row("mcp_1", "srv", "stdio", r#"{"command":"npx","args":[],"env":{}}"#);

        let svc = make_service(vec![row], vec![ok_adapter, fail_adapter, skip_adapter]);
        let result = svc.sync_to_agents(&["mcp_1".into()]).await.unwrap();

        assert!(!result.success); // overall failure
        assert_eq!(result.results.len(), 3);
        assert!(result.results[0].success); // Claude OK
        assert!(!result.results[1].success); // Gemini failed
        assert!(result.results[2].success); // Qwen skipped (not installed)
    }

    // -- detected_to_response ------------------------------------------------

    #[test]
    fn detected_to_response_fields() {
        let detected = DetectedServer {
            name: "my-srv".into(),
            transport: stdio_transport(),
        };
        let resp = detected_to_response(detected);
        assert_eq!(resp.name, "my-srv");
        assert_eq!(resp.id, "detected_my-srv");
        assert!(!resp.enabled);
        assert_eq!(resp.status, McpServerStatus::Disconnected);
    }
}
