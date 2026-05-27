use std::sync::Arc;

use aionui_api_types::{
    BatchImportMcpServersRequest, CreateMcpServerRequest, McpServerResponse, UpdateMcpServerRequest,
};
use aionui_db::{CreateMcpServerParams, IMcpServerRepository, UpdateMcpServerParams};
use tracing::info;

use crate::error::McpError;
use crate::types::{McpServer, McpServerTransport};

// ---------------------------------------------------------------------------
// McpConfigService
// ---------------------------------------------------------------------------

/// MCP server configuration CRUD service.
///
/// Handles create/read/update/delete operations on MCP server configs,
/// delegating persistence to `IMcpServerRepository`. Business rules:
///
/// - **add**: upsert by name (existing → update, new → create)
/// - **delete**: if enabled, caller should trigger remove-from-agents
/// - **toggle**: flips enabled state
/// - **batch_import**: sequential upsert by name
#[derive(Clone)]
pub struct McpConfigService {
    repo: Arc<dyn IMcpServerRepository>,
}

impl McpConfigService {
    pub fn new(repo: Arc<dyn IMcpServerRepository>) -> Self {
        Self { repo }
    }

    /// List all MCP servers.
    pub async fn list_servers(&self) -> Result<Vec<McpServerResponse>, McpError> {
        let rows = self.repo.list().await?;
        rows.into_iter()
            .map(|row| McpServer::from_row(row).map(McpServer::into_response))
            .collect()
    }

    /// Get a single MCP server by ID.
    pub async fn get_server(&self, id: &str) -> Result<McpServerResponse, McpError> {
        let row = self
            .repo
            .find_by_id(id)
            .await?
            .ok_or_else(|| McpError::NotFound(id.to_owned()))?;
        let server = McpServer::from_row(row)?;
        Ok(server.into_response())
    }

    /// Add (or upsert) an MCP server.
    ///
    /// If a server with the same name already exists, it is updated
    /// (transport, description, original_json) rather than creating a duplicate.
    pub async fn add_server(&self, req: CreateMcpServerRequest) -> Result<McpServerResponse, McpError> {
        let transport = McpServerTransport::from(req.transport);
        let config_json = transport.to_config_json()?;

        // Upsert: if a server with this name exists, update it
        if let Some(existing) = self.repo.find_by_name(&req.name).await? {
            let params = UpdateMcpServerParams {
                description: Some(req.description.as_deref()),
                transport_type: Some(transport.transport_type()),
                transport_config: Some(&config_json),
                original_json: Some(req.original_json.as_deref()),
                ..Default::default()
            };
            let updated = self.repo.update(&existing.id, params).await?;
            let server = McpServer::from_row(updated)?;
            return Ok(server.into_response());
        }

        let params = CreateMcpServerParams {
            name: &req.name,
            description: req.description.as_deref(),
            enabled: false,
            transport_type: transport.transport_type(),
            transport_config: &config_json,
            tools: None,
            original_json: req.original_json.as_deref(),
            builtin: req.builtin,
        };
        let row = self.repo.create(params).await?;
        let server = McpServer::from_row(row)?;
        Ok(server.into_response())
    }

    /// Edit an existing MCP server (partial update).
    pub async fn edit_server(&self, id: &str, req: UpdateMcpServerRequest) -> Result<McpServerResponse, McpError> {
        // Verify the server exists
        self.repo
            .find_by_id(id)
            .await?
            .ok_or_else(|| McpError::NotFound(id.to_owned()))?;

        // Check name uniqueness if renaming
        if let Some(ref new_name) = req.name
            && let Some(existing) = self.repo.find_by_name(new_name).await?
            && existing.id != id
        {
            return Err(McpError::Conflict(new_name.clone()));
        }

        // Build transport fields if provided
        let transport = req.transport.map(McpServerTransport::from);
        let config_json = transport.as_ref().map(McpServerTransport::to_config_json).transpose()?;

        let params = UpdateMcpServerParams {
            name: req.name.as_deref(),
            description: req.description.as_ref().map(|opt| opt.as_deref()),
            transport_type: transport.as_ref().map(McpServerTransport::transport_type),
            transport_config: config_json.as_deref(),
            original_json: req.original_json.as_ref().map(|opt| opt.as_deref()),
            ..Default::default()
        };

        let row = self.repo.update(id, params).await?;
        let server = McpServer::from_row(row)?;
        Ok(server.into_response())
    }

    /// Delete an MCP server by ID.
    ///
    /// Returns whether the deleted server was enabled (caller should trigger
    /// remove-from-agents if `true`).
    pub async fn delete_server(&self, id: &str) -> Result<bool, McpError> {
        let row = self
            .repo
            .find_by_id(id)
            .await?
            .ok_or_else(|| McpError::NotFound(id.to_owned()))?;
        let was_enabled = row.enabled;
        self.repo.delete(id).await?;
        Ok(was_enabled)
    }

    /// Toggle the enabled state of an MCP server.
    ///
    /// Returns the updated server response.
    pub async fn toggle_server(&self, id: &str) -> Result<McpServerResponse, McpError> {
        let row = self
            .repo
            .find_by_id(id)
            .await?
            .ok_or_else(|| McpError::NotFound(id.to_owned()))?;

        let new_enabled = !row.enabled;
        let params = UpdateMcpServerParams {
            enabled: Some(new_enabled),
            ..Default::default()
        };
        let updated = self.repo.update(id, params).await?;
        let server = McpServer::from_row(updated)?;
        Ok(server.into_response())
    }

    /// Batch import MCP servers (upsert by name).
    ///
    /// Each server is processed individually: existing names are updated,
    /// new names are created.
    pub async fn batch_import(&self, req: BatchImportMcpServersRequest) -> Result<Vec<McpServerResponse>, McpError> {
        let mut params_data: Vec<(McpServerTransport, String)> = Vec::new();
        for server_req in &req.servers {
            let transport = McpServerTransport::from(server_req.transport.clone());
            let config_json = transport.to_config_json()?;
            params_data.push((transport, config_json));
        }

        let create_params: Vec<CreateMcpServerParams<'_>> = req
            .servers
            .iter()
            .zip(params_data.iter())
            .map(|(server_req, (transport, config_json))| CreateMcpServerParams {
                name: &server_req.name,
                description: server_req.description.as_deref(),
                enabled: server_req.enabled.unwrap_or(false),
                transport_type: transport.transport_type(),
                transport_config: config_json.as_str(),
                tools: None,
                original_json: server_req.original_json.as_deref(),
                builtin: server_req.builtin,
            })
            .collect();

        let rows = self.repo.batch_upsert(&create_params).await?;
        info!(
            requested_count = req.servers.len(),
            imported_count = rows.len(),
            enabled_count = rows.iter().filter(|row| row.enabled).count(),
            "batch imported MCP servers"
        );
        rows.into_iter()
            .map(|row| McpServer::from_row(row).map(McpServer::into_response))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_api_types::{ImportMcpServerRequest, McpTransport};
    use aionui_common::{McpServerStatus, TimestampMs};
    use aionui_db::models::McpServerRow;
    use aionui_db::{CreateMcpServerParams, DbError, UpdateMcpServerParams};
    use std::collections::HashMap;
    use std::sync::Mutex;

    // -- In-memory mock repository -------------------------------------------

    #[derive(Debug)]
    struct MockMcpServerRepo {
        servers: Mutex<Vec<McpServerRow>>,
        id_counter: Mutex<u32>,
    }

    impl MockMcpServerRepo {
        fn new() -> Self {
            Self {
                servers: Mutex::new(Vec::new()),
                id_counter: Mutex::new(0),
            }
        }

        fn next_id(&self) -> String {
            let mut counter = self.id_counter.lock().unwrap();
            *counter += 1;
            format!("mcp_{counter}")
        }

        fn now() -> TimestampMs {
            1000
        }
    }

    #[async_trait::async_trait]
    impl IMcpServerRepository for MockMcpServerRepo {
        async fn list(&self) -> Result<Vec<McpServerRow>, DbError> {
            let servers = self.servers.lock().unwrap();
            Ok(servers.clone())
        }

        async fn find_by_id(&self, id: &str) -> Result<Option<McpServerRow>, DbError> {
            let servers = self.servers.lock().unwrap();
            Ok(servers.iter().find(|s| s.id == id).cloned())
        }

        async fn find_by_name(&self, name: &str) -> Result<Option<McpServerRow>, DbError> {
            let servers = self.servers.lock().unwrap();
            Ok(servers.iter().find(|s| s.name == name).cloned())
        }

        async fn create(&self, params: CreateMcpServerParams<'_>) -> Result<McpServerRow, DbError> {
            let mut servers = self.servers.lock().unwrap();
            if servers.iter().any(|s| s.name == params.name) {
                return Err(DbError::Conflict(format!(
                    "MCP server name '{}' already exists",
                    params.name
                )));
            }
            let row = McpServerRow {
                id: self.next_id(),
                name: params.name.to_owned(),
                description: params.description.map(String::from),
                enabled: params.enabled,
                transport_type: params.transport_type.to_owned(),
                transport_config: params.transport_config.to_owned(),
                tools: params.tools.map(String::from),
                status: "disconnected".to_owned(),
                last_connected: None,
                original_json: params.original_json.map(String::from),
                builtin: params.builtin,
                created_at: Self::now(),
                updated_at: Self::now(),
            };
            servers.push(row.clone());
            Ok(row)
        }

        async fn update(&self, id: &str, params: UpdateMcpServerParams<'_>) -> Result<McpServerRow, DbError> {
            let mut servers = self.servers.lock().unwrap();
            let idx = servers
                .iter()
                .position(|s| s.id == id)
                .ok_or_else(|| DbError::NotFound(format!("MCP server {id}")))?;

            // Check name conflict
            if let Some(new_name) = params.name {
                if servers.iter().any(|s| s.name == new_name && s.id != id) {
                    return Err(DbError::Conflict(format!(
                        "MCP server name '{new_name}' already exists"
                    )));
                }
                servers[idx].name = new_name.to_owned();
            }
            if let Some(desc) = params.description {
                servers[idx].description = desc.map(String::from);
            }
            if let Some(enabled) = params.enabled {
                servers[idx].enabled = enabled;
            }
            if let Some(tt) = params.transport_type {
                servers[idx].transport_type = tt.to_owned();
            }
            if let Some(tc) = params.transport_config {
                servers[idx].transport_config = tc.to_owned();
            }
            if let Some(tools) = params.tools {
                servers[idx].tools = tools.map(String::from);
            }
            if let Some(oj) = params.original_json {
                servers[idx].original_json = oj.map(String::from);
            }
            if let Some(b) = params.builtin {
                servers[idx].builtin = b;
            }
            servers[idx].updated_at = Self::now();
            Ok(servers[idx].clone())
        }

        async fn delete(&self, id: &str) -> Result<(), DbError> {
            let mut servers = self.servers.lock().unwrap();
            let idx = servers
                .iter()
                .position(|s| s.id == id)
                .ok_or_else(|| DbError::NotFound(format!("MCP server {id}")))?;
            servers.remove(idx);
            Ok(())
        }

        async fn batch_upsert(&self, params_list: &[CreateMcpServerParams<'_>]) -> Result<Vec<McpServerRow>, DbError> {
            let mut results = Vec::new();
            for params in params_list {
                let mut servers = self.servers.lock().unwrap();
                if let Some(idx) = servers.iter().position(|s| s.name == params.name) {
                    // Update existing
                    servers[idx].description = params.description.map(String::from);
                    servers[idx].transport_type = params.transport_type.to_owned();
                    servers[idx].transport_config = params.transport_config.to_owned();
                    servers[idx].original_json = params.original_json.map(String::from);
                    servers[idx].updated_at = Self::now();
                    results.push(servers[idx].clone());
                } else {
                    // Create new
                    let row = McpServerRow {
                        id: self.next_id(),
                        name: params.name.to_owned(),
                        description: params.description.map(String::from),
                        enabled: params.enabled,
                        transport_type: params.transport_type.to_owned(),
                        transport_config: params.transport_config.to_owned(),
                        tools: params.tools.map(String::from),
                        status: "disconnected".to_owned(),
                        last_connected: None,
                        original_json: params.original_json.map(String::from),
                        builtin: params.builtin,
                        created_at: Self::now(),
                        updated_at: Self::now(),
                    };
                    servers.push(row.clone());
                    results.push(row);
                }
            }
            Ok(results)
        }

        async fn update_status(
            &self,
            id: &str,
            status: &str,
            last_connected: Option<TimestampMs>,
        ) -> Result<(), DbError> {
            let mut servers = self.servers.lock().unwrap();
            let idx = servers
                .iter()
                .position(|s| s.id == id)
                .ok_or_else(|| DbError::NotFound(format!("MCP server {id}")))?;
            servers[idx].status = status.to_owned();
            if let Some(lc) = last_connected {
                servers[idx].last_connected = Some(lc);
            }
            Ok(())
        }

        async fn update_tools(&self, id: &str, tools: Option<&str>) -> Result<(), DbError> {
            let mut servers = self.servers.lock().unwrap();
            let idx = servers
                .iter()
                .position(|s| s.id == id)
                .ok_or_else(|| DbError::NotFound(format!("MCP server {id}")))?;
            servers[idx].tools = tools.map(String::from);
            Ok(())
        }
    }

    fn make_service() -> McpConfigService {
        McpConfigService::new(Arc::new(MockMcpServerRepo::new()))
    }

    fn stdio_create_req(name: &str) -> CreateMcpServerRequest {
        CreateMcpServerRequest {
            name: name.to_owned(),
            description: Some("test server".to_owned()),
            transport: McpTransport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "@test/server".into()],
                env: HashMap::new(),
            },
            original_json: None,
            builtin: false,
        }
    }

    fn http_create_req(name: &str) -> CreateMcpServerRequest {
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

    fn stdio_import_req(name: &str) -> ImportMcpServerRequest {
        ImportMcpServerRequest {
            name: name.to_owned(),
            description: Some("test server".to_owned()),
            transport: McpTransport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "@test/server".into()],
                env: HashMap::new(),
            },
            original_json: None,
            builtin: false,
            enabled: None,
        }
    }

    fn http_import_req(name: &str) -> ImportMcpServerRequest {
        ImportMcpServerRequest {
            name: name.to_owned(),
            description: None,
            transport: McpTransport::Http {
                url: "https://example.com/mcp".into(),
                headers: HashMap::new(),
            },
            original_json: None,
            builtin: false,
            enabled: None,
        }
    }

    // -- list_servers --------------------------------------------------------

    #[tokio::test]
    async fn list_servers_empty() {
        let svc = make_service();
        let result = svc.list_servers().await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn list_servers_returns_all() {
        let svc = make_service();
        svc.add_server(stdio_create_req("a")).await.unwrap();
        svc.add_server(http_create_req("b")).await.unwrap();

        let result = svc.list_servers().await.unwrap();
        assert_eq!(result.len(), 2);
    }

    // -- get_server ----------------------------------------------------------

    #[tokio::test]
    async fn get_server_found() {
        let svc = make_service();
        let created = svc.add_server(stdio_create_req("test")).await.unwrap();
        let found = svc.get_server(&created.id).await.unwrap();
        assert_eq!(found.id, created.id);
        assert_eq!(found.name, "test");
    }

    #[tokio::test]
    async fn get_server_not_found() {
        let svc = make_service();
        let result = svc.get_server("nonexistent").await;
        assert!(matches!(result, Err(McpError::NotFound(_))));
    }

    // -- add_server ----------------------------------------------------------

    #[tokio::test]
    async fn add_server_creates_new() {
        let svc = make_service();
        let resp = svc.add_server(stdio_create_req("new-srv")).await.unwrap();
        assert_eq!(resp.name, "new-srv");
        assert!(!resp.enabled);
        assert_eq!(resp.status, McpServerStatus::Disconnected);
        assert_eq!(resp.description.as_deref(), Some("test server"));
    }

    #[tokio::test]
    async fn add_server_upserts_existing() {
        let svc = make_service();
        let first = svc.add_server(stdio_create_req("upsert-test")).await.unwrap();

        // Second add with same name updates existing
        let updated = svc.add_server(http_create_req("upsert-test")).await.unwrap();
        assert_eq!(updated.id, first.id);
        // Transport should be updated to http
        match updated.transport {
            McpTransport::Http { ref url, .. } => {
                assert_eq!(url, "https://example.com/mcp");
            }
            _ => panic!("expected Http transport after upsert"),
        }
    }

    #[tokio::test]
    async fn add_server_stdio_complete() {
        let svc = make_service();
        let resp = svc
            .add_server(CreateMcpServerRequest {
                name: "stdio-full".into(),
                description: Some("full stdio".into()),
                transport: McpTransport::Stdio {
                    command: "node".into(),
                    args: vec!["index.js".into()],
                    env: HashMap::from([("KEY".into(), "val".into())]),
                },
                original_json: Some(r#"{"name":"stdio-full"}"#.into()),
                builtin: true,
            })
            .await
            .unwrap();
        assert_eq!(resp.name, "stdio-full");
        assert!(resp.builtin);
        assert_eq!(resp.original_json.as_deref(), Some(r#"{"name":"stdio-full"}"#));
    }

    // -- edit_server ---------------------------------------------------------

    #[tokio::test]
    async fn edit_server_updates_name() {
        let svc = make_service();
        let created = svc.add_server(stdio_create_req("old-name")).await.unwrap();
        let updated = svc
            .edit_server(
                &created.id,
                UpdateMcpServerRequest {
                    name: Some("new-name".into()),
                    description: None,
                    transport: None,
                    original_json: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.name, "new-name");
    }

    #[tokio::test]
    async fn edit_server_updates_transport() {
        let svc = make_service();
        let created = svc.add_server(stdio_create_req("test")).await.unwrap();
        let updated = svc
            .edit_server(
                &created.id,
                UpdateMcpServerRequest {
                    name: None,
                    description: None,
                    transport: Some(McpTransport::Http {
                        url: "https://new.url".into(),
                        headers: HashMap::new(),
                    }),
                    original_json: None,
                },
            )
            .await
            .unwrap();
        match updated.transport {
            McpTransport::Http { ref url, .. } => assert_eq!(url, "https://new.url"),
            _ => panic!("expected Http"),
        }
    }

    #[tokio::test]
    async fn edit_server_clears_description() {
        let svc = make_service();
        let created = svc.add_server(stdio_create_req("test")).await.unwrap();
        assert!(created.description.is_some());

        let updated = svc
            .edit_server(
                &created.id,
                UpdateMcpServerRequest {
                    name: None,
                    description: Some(None), // clear
                    transport: None,
                    original_json: None,
                },
            )
            .await
            .unwrap();
        assert!(updated.description.is_none());
    }

    #[tokio::test]
    async fn edit_server_not_found() {
        let svc = make_service();
        let result = svc
            .edit_server(
                "nonexistent",
                UpdateMcpServerRequest {
                    name: Some("x".into()),
                    description: None,
                    transport: None,
                    original_json: None,
                },
            )
            .await;
        assert!(matches!(result, Err(McpError::NotFound(_))));
    }

    #[tokio::test]
    async fn edit_server_name_conflict() {
        let svc = make_service();
        svc.add_server(stdio_create_req("server-a")).await.unwrap();
        let b = svc.add_server(stdio_create_req("server-b")).await.unwrap();

        let result = svc
            .edit_server(
                &b.id,
                UpdateMcpServerRequest {
                    name: Some("server-a".into()), // conflict
                    description: None,
                    transport: None,
                    original_json: None,
                },
            )
            .await;
        assert!(matches!(result, Err(McpError::Conflict(_))));
    }

    #[tokio::test]
    async fn edit_server_rename_to_same_name() {
        let svc = make_service();
        let a = svc.add_server(stdio_create_req("server-a")).await.unwrap();

        // Renaming to the same name should succeed
        let result = svc
            .edit_server(
                &a.id,
                UpdateMcpServerRequest {
                    name: Some("server-a".into()),
                    description: None,
                    transport: None,
                    original_json: None,
                },
            )
            .await;
        assert!(result.is_ok());
    }

    // -- delete_server -------------------------------------------------------

    #[tokio::test]
    async fn delete_server_removes_and_returns_enabled_status() {
        let svc = make_service();
        let created = svc.add_server(stdio_create_req("test")).await.unwrap();

        // Not enabled
        let was_enabled = svc.delete_server(&created.id).await.unwrap();
        assert!(!was_enabled);

        // Should be gone
        let result = svc.get_server(&created.id).await;
        assert!(matches!(result, Err(McpError::NotFound(_))));
    }

    #[tokio::test]
    async fn delete_enabled_server_returns_true() {
        let svc = make_service();
        let created = svc.add_server(stdio_create_req("test")).await.unwrap();
        svc.toggle_server(&created.id).await.unwrap(); // enable

        let was_enabled = svc.delete_server(&created.id).await.unwrap();
        assert!(was_enabled);
    }

    #[tokio::test]
    async fn delete_server_not_found() {
        let svc = make_service();
        let result = svc.delete_server("nonexistent").await;
        assert!(matches!(result, Err(McpError::NotFound(_))));
    }

    // -- toggle_server -------------------------------------------------------

    #[tokio::test]
    async fn toggle_server_enables_then_disables() {
        let svc = make_service();
        let created = svc.add_server(stdio_create_req("toggle")).await.unwrap();
        assert!(!created.enabled);

        let toggled = svc.toggle_server(&created.id).await.unwrap();
        assert!(toggled.enabled);

        let toggled_back = svc.toggle_server(&created.id).await.unwrap();
        assert!(!toggled_back.enabled);
    }

    #[tokio::test]
    async fn toggle_server_not_found() {
        let svc = make_service();
        let result = svc.toggle_server("nonexistent").await;
        assert!(matches!(result, Err(McpError::NotFound(_))));
    }

    // -- batch_import --------------------------------------------------------

    #[tokio::test]
    async fn batch_import_creates_new_servers() {
        let svc = make_service();
        let req = BatchImportMcpServersRequest {
            servers: vec![stdio_import_req("a"), http_import_req("b")],
        };
        let results = svc.batch_import(req).await.unwrap();
        assert_eq!(results.len(), 2);

        let all = svc.list_servers().await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn batch_import_upserts_existing() {
        let svc = make_service();
        svc.add_server(stdio_create_req("existing")).await.unwrap();

        let req = BatchImportMcpServersRequest {
            servers: vec![
                http_import_req("existing"),   // update
                stdio_import_req("brand-new"), // create
            ],
        };
        let results = svc.batch_import(req).await.unwrap();
        assert_eq!(results.len(), 2);

        let all = svc.list_servers().await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn batch_import_preserves_enabled_state() {
        let svc = make_service();
        let mut req = stdio_import_req("enabled-mcp");
        req.enabled = Some(true);
        let result = svc
            .batch_import(BatchImportMcpServersRequest { servers: vec![req] })
            .await
            .unwrap();

        assert_eq!(result[0].name, "enabled-mcp");
        assert!(result[0].enabled);
    }

    #[tokio::test]
    async fn batch_import_empty_list() {
        let svc = make_service();
        let req = BatchImportMcpServersRequest { servers: vec![] };
        let results = svc.batch_import(req).await.unwrap();
        assert!(results.is_empty());
    }
}
