use std::collections::HashMap;

use aionui_common::{McpServerStatus, McpSource, TimestampMs};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// A. Transport types
// ---------------------------------------------------------------------------

/// MCP server transport configuration (tagged union).
///
/// `http` represents Streamable HTTP (the MCP standard); `sse` is legacy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpTransport {
    #[serde(rename = "stdio")]
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env: HashMap<String, String>,
    },
    #[serde(rename = "sse")]
    Sse {
        url: String,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        headers: HashMap<String, String>,
    },
    #[serde(rename = "http")]
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        headers: HashMap<String, String>,
    },
}

// ---------------------------------------------------------------------------
// B. Tool description
// ---------------------------------------------------------------------------

/// MCP tool description returned from connection tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResponse {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// C. Server CRUD — Request DTOs
// ---------------------------------------------------------------------------

/// Request body for `POST /api/mcp/servers` — create (or upsert by name).
#[derive(Debug, Deserialize)]
pub struct CreateMcpServerRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub transport: McpTransport,
    #[serde(default)]
    pub original_json: Option<String>,
    #[serde(default)]
    pub builtin: bool,
}

/// Request item for `POST /api/mcp/servers/import`.
///
/// Import can preserve the enabled state from a legacy source. Plain create
/// intentionally does not accept `enabled`, because enabling must also sync
/// the server to agent configs.
#[derive(Debug, Deserialize)]
pub struct ImportMcpServerRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub transport: McpTransport,
    #[serde(default)]
    pub original_json: Option<String>,
    #[serde(default)]
    pub builtin: bool,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Request body for `PUT /api/mcp/servers/:id` — partial update.
#[derive(Debug, Deserialize)]
pub struct UpdateMcpServerRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub transport: Option<McpTransport>,
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub original_json: Option<Option<String>>,
}

/// Request body for `POST /api/mcp/servers/import` — batch import.
#[derive(Debug, Deserialize)]
pub struct BatchImportMcpServersRequest {
    pub servers: Vec<ImportMcpServerRequest>,
}

// ---------------------------------------------------------------------------
// D. Server CRUD — Response DTOs
// ---------------------------------------------------------------------------

/// Full MCP server configuration response.
#[derive(Debug, Clone, Serialize)]
pub struct McpServerResponse {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub enabled: bool,
    pub transport: McpTransport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<McpToolResponse>>,
    pub status: McpServerStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_connected: Option<TimestampMs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_json: Option<String>,
    pub builtin: bool,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

// ---------------------------------------------------------------------------
// E. Agent sync
// ---------------------------------------------------------------------------

/// Request body for `POST /api/mcp/sync-to-agents`.
#[derive(Debug, Deserialize)]
pub struct SyncToAgentsRequest {
    pub servers: Vec<String>,
}

/// Request body for `POST /api/mcp/remove-from-agents`.
#[derive(Debug, Deserialize)]
pub struct RemoveFromAgentsRequest {
    pub server_names: Vec<String>,
}

/// Per-agent sync result.
#[derive(Debug, Clone, Serialize)]
pub struct McpAgentSyncResult {
    pub agent: McpSource,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Aggregated sync result across all agents.
#[derive(Debug, Clone, Serialize)]
pub struct McpSyncResult {
    pub success: bool,
    pub results: Vec<McpAgentSyncResult>,
}

/// Detected MCP servers for a single agent.
#[derive(Debug, Clone, Serialize)]
pub struct DetectedMcpServerResponse {
    pub source: McpSource,
    pub servers: Vec<McpServerResponse>,
}

// ---------------------------------------------------------------------------
// F. Connection test
// ---------------------------------------------------------------------------

/// Request body for `POST /api/mcp/test-connection`.
#[derive(Debug, Deserialize)]
pub struct TestMcpConnectionRequest {
    pub name: String,
    pub transport: McpTransport,
}

/// Authentication method detected during connection test.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpAuthMethod {
    Oauth,
    Basic,
}

/// Result of an MCP server connection test.
#[derive(Debug, Clone, Serialize)]
pub struct McpConnectionTestResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<McpToolResponse>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_auth: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<McpAuthMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub www_authenticate: Option<String>,
}

// ---------------------------------------------------------------------------
// G. OAuth
// ---------------------------------------------------------------------------

/// Request body for `POST /api/mcp/oauth/check-status`.
#[derive(Debug, Deserialize)]
pub struct OAuthCheckStatusRequest {
    pub server_url: String,
}

/// Response for OAuth status check.
#[derive(Debug, Serialize)]
pub struct OAuthStatusResponse {
    pub authenticated: bool,
}

/// Request body for `POST /api/mcp/oauth/login`.
#[derive(Debug, Deserialize)]
pub struct OAuthLoginRequest {
    pub server_url: String,
}

/// Response for OAuth login initiation.
#[derive(Debug, Serialize)]
pub struct OAuthLoginResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Request body for `POST /api/mcp/oauth/logout`.
#[derive(Debug, Deserialize)]
pub struct OAuthLogoutRequest {
    pub server_url: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Deserialize `Option<Option<T>>`:
/// - JSON field absent → `None` (keep current value)
/// - JSON `null` → `Some(None)` (clear the value)
/// - JSON value → `Some(Some(value))` (set new value)
fn deserialize_optional_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let value: Option<T> = Option::deserialize(deserializer)?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- McpTransport serde --------------------------------------------------

    #[test]
    fn test_stdio_transport_serde() {
        let t = McpTransport::Stdio {
            command: "npx".into(),
            args: vec!["-y".into(), "test-server".into()],
            env: HashMap::from([("KEY".into(), "value".into())]),
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["type"], "stdio");
        assert_eq!(json["command"], "npx");
        assert_eq!(json["args"], serde_json::json!(["-y", "test-server"]));
        assert_eq!(json["env"]["KEY"], "value");

        let parsed: McpTransport = serde_json::from_value(json).unwrap();
        match parsed {
            McpTransport::Stdio { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(args, vec!["-y", "test-server"]);
                assert_eq!(env.get("KEY").unwrap(), "value");
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[test]
    fn test_http_transport_serde() {
        let t = McpTransport::Http {
            url: "https://example.com/mcp".into(),
            headers: HashMap::new(),
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["type"], "http");
        assert_eq!(json["url"], "https://example.com/mcp");
        assert!(json.get("headers").is_none()); // empty map skipped
    }

    #[test]
    fn test_sse_transport_with_headers() {
        let t = McpTransport::Sse {
            url: "https://example.com/sse".into(),
            headers: HashMap::from([("Authorization".into(), "Bearer xxx".into())]),
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["type"], "sse");
        assert_eq!(json["headers"]["Authorization"], "Bearer xxx");
    }

    #[test]
    fn test_stdio_transport_minimal() {
        let json = serde_json::json!({
            "type": "stdio",
            "command": "node"
        });
        let t: McpTransport = serde_json::from_value(json).unwrap();
        match t {
            McpTransport::Stdio { command, args, env } => {
                assert_eq!(command, "node");
                assert!(args.is_empty());
                assert!(env.is_empty());
            }
            _ => panic!("expected Stdio"),
        }
    }

    // -- CreateMcpServerRequest -----------------------------------------------

    #[test]
    fn test_create_request_deserialization() {
        let json = serde_json::json!({
            "name": "test-mcp",
            "description": "A test server",
            "transport": {
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "@test/server"]
            }
        });
        let req: CreateMcpServerRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.name, "test-mcp");
        assert_eq!(req.description.as_deref(), Some("A test server"));
        assert!(!req.builtin);
    }

    #[test]
    fn test_create_request_missing_name() {
        let json = serde_json::json!({
            "transport": { "type": "stdio", "command": "node" }
        });
        let result = serde_json::from_value::<CreateMcpServerRequest>(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_request_missing_transport() {
        let json = serde_json::json!({ "name": "test" });
        let result = serde_json::from_value::<CreateMcpServerRequest>(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_request_invalid_transport_type() {
        let json = serde_json::json!({
            "name": "test",
            "transport": { "type": "invalid", "command": "x" }
        });
        let result = serde_json::from_value::<CreateMcpServerRequest>(json);
        assert!(result.is_err());
    }

    // -- UpdateMcpServerRequest -----------------------------------------------

    #[test]
    fn test_update_request_partial() {
        let json = serde_json::json!({ "name": "new-name" });
        let req: UpdateMcpServerRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.name.as_deref(), Some("new-name"));
        assert!(req.description.is_none()); // absent → keep
        assert!(req.transport.is_none());
    }

    #[test]
    fn test_update_request_null_description() {
        let json = serde_json::json!({ "description": null });
        let req: UpdateMcpServerRequest = serde_json::from_value(json).unwrap();
        // null → Some(None) → clear
        assert_eq!(req.description, Some(None));
    }

    #[test]
    fn test_update_request_set_description() {
        let json = serde_json::json!({ "description": "new desc" });
        let req: UpdateMcpServerRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.description, Some(Some("new desc".into())));
    }

    // -- McpServerResponse ----------------------------------------------------

    #[test]
    fn test_server_response_serialization() {
        let resp = McpServerResponse {
            id: "mcp_123".into(),
            name: "test".into(),
            description: None,
            enabled: true,
            transport: McpTransport::Stdio {
                command: "npx".into(),
                args: vec![],
                env: HashMap::new(),
            },
            tools: None,
            status: McpServerStatus::Disconnected,
            last_connected: None,
            original_json: None,
            builtin: false,
            created_at: 1000,
            updated_at: 2000,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["id"], "mcp_123");
        assert_eq!(json["enabled"], true);
        assert_eq!(json["status"], "disconnected");
        assert!(json.get("description").is_none()); // None skipped
        assert!(json.get("tools").is_none());
    }

    // -- BatchImportMcpServersRequest -----------------------------------------

    #[test]
    fn test_batch_import_request() {
        let json = serde_json::json!({
            "servers": [
                { "name": "a", "transport": { "type": "stdio", "command": "a" } },
                { "name": "b", "transport": { "type": "http", "url": "http://b" } }
            ]
        });
        let req: BatchImportMcpServersRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.servers.len(), 2);
    }

    #[test]
    fn test_batch_import_empty() {
        let json = serde_json::json!({ "servers": [] });
        let req: BatchImportMcpServersRequest = serde_json::from_value(json).unwrap();
        assert!(req.servers.is_empty());
    }

    // -- McpSyncResult --------------------------------------------------------

    #[test]
    fn test_sync_result_serialization() {
        let result = McpSyncResult {
            success: false,
            results: vec![
                McpAgentSyncResult {
                    agent: McpSource::Claude,
                    success: true,
                    error: None,
                },
                McpAgentSyncResult {
                    agent: McpSource::Gemini,
                    success: false,
                    error: Some("CLI not found".into()),
                },
            ],
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["success"], false);
        assert_eq!(json["results"][0]["agent"], "claude");
        assert_eq!(json["results"][0]["success"], true);
        assert!(json["results"][0].get("error").is_none());
        assert_eq!(json["results"][1]["error"], "CLI not found");
    }

    // -- McpConnectionTestResult ----------------------------------------------

    #[test]
    fn test_connection_test_success() {
        let result = McpConnectionTestResult {
            success: true,
            tools: Some(vec![McpToolResponse {
                name: "read_file".into(),
                description: Some("Read a file".into()),
                input_schema: None,
            }]),
            error: None,
            needs_auth: None,
            auth_method: None,
            www_authenticate: None,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["tools"][0]["name"], "read_file");
        assert!(json.get("error").is_none());
        assert!(json.get("needs_auth").is_none());
    }

    #[test]
    fn test_connection_test_needs_auth() {
        let result = McpConnectionTestResult {
            success: false,
            tools: None,
            error: None,
            needs_auth: Some(true),
            auth_method: Some(McpAuthMethod::Oauth),
            www_authenticate: Some("Bearer realm=\"mcp\"".into()),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["needs_auth"], true);
        assert_eq!(json["auth_method"], "oauth");
        assert_eq!(json["www_authenticate"], "Bearer realm=\"mcp\"");
    }

    // -- TestMcpConnectionRequest ---------------------------------------------

    #[test]
    fn test_connection_request_deserialization() {
        let json = serde_json::json!({
            "name": "test-server",
            "transport": { "type": "http", "url": "https://example.com/mcp" }
        });
        let req: TestMcpConnectionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.name, "test-server");
        match req.transport {
            McpTransport::Http { ref url, .. } => {
                assert_eq!(url, "https://example.com/mcp");
            }
            _ => panic!("expected Http"),
        }
    }

    // -- OAuth DTOs -----------------------------------------------------------

    #[test]
    fn test_oauth_status_response() {
        let resp = OAuthStatusResponse { authenticated: true };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["authenticated"], true);
    }

    #[test]
    fn test_oauth_login_response() {
        let resp = OAuthLoginResponse {
            success: false,
            error: Some("discovery failed".into()),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["success"], false);
        assert_eq!(json["error"], "discovery failed");
    }

    // -- DetectedMcpServerResponse --------------------------------------------

    #[test]
    fn test_detected_server_response() {
        let resp = DetectedMcpServerResponse {
            source: McpSource::Claude,
            servers: vec![],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["source"], "claude");
        assert_eq!(json["servers"], serde_json::json!([]));
    }

    // -- SyncToAgentsRequest / RemoveFromAgentsRequest -------------------------

    #[test]
    fn test_sync_to_agents_request() {
        let json = serde_json::json!({ "servers": ["mcp_1", "mcp_2"] });
        let req: SyncToAgentsRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.servers, vec!["mcp_1", "mcp_2"]);
    }

    #[test]
    fn test_remove_from_agents_request() {
        let json = serde_json::json!({ "server_names": ["test-mcp"] });
        let req: RemoveFromAgentsRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.server_names, vec!["test-mcp"]);
    }
}
