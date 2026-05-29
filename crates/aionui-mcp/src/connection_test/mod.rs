mod protocol;

use std::collections::HashMap;
use std::time::Duration;

use aionui_api_types::McpConnectionTestResult;
use aionui_runtime::Builder as CmdBuilder;
use serde::Serialize;
use tokio::sync::mpsc;
use tracing::debug;

use crate::types::McpServerTransport;
use protocol::{
    JsonRpcRequest, JsonRpcResponse, SseEvent, build_http_headers, build_initialize_request,
    build_initialized_notification, build_tools_list_request, error_result, read_sse_events, rpc_error_result,
    run_stdio_protocol, spawn_error_result, success_result, timeout_result, wait_for_endpoint,
    wait_for_jsonrpc_response,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// McpConnectionTestService
// ---------------------------------------------------------------------------

/// Service for testing MCP server connectivity.
///
/// Creates a temporary MCP client, performs the protocol handshake
/// (initialize -> initialized -> tools/list), and returns the tool list
/// or an error.  Supports stdio, HTTP (Streamable HTTP), and SSE transports.
#[derive(Clone)]
pub struct McpConnectionTestService {
    http_client: reqwest::Client,
    timeout: Duration,
}

impl McpConnectionTestService {
    pub fn new(http_client: reqwest::Client) -> Self {
        Self {
            http_client,
            timeout: CONNECTION_TIMEOUT,
        }
    }

    /// Override the connection test timeout (default: 30s).
    pub fn with_timeout(self, timeout: Duration) -> Self {
        Self { timeout, ..self }
    }

    /// Test connectivity to an MCP server.
    ///
    /// Dispatches to the appropriate transport handler.  Always returns
    /// a result (never errors) -- failures are encoded in the struct.
    pub async fn test_connection(&self, name: &str, transport: &McpServerTransport) -> McpConnectionTestResult {
        debug!(name, ?transport, "starting MCP connection test");
        match transport {
            McpServerTransport::Stdio { command, args, env } => self.test_stdio(command, args, env).await,
            McpServerTransport::Http { url, headers } => self.test_http(url, headers).await,
            McpServerTransport::Sse { url, headers } => self.test_sse(url, headers).await,
        }
    }

    // -- Stdio transport --------------------------------------------------

    async fn test_stdio(
        &self,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> McpConnectionTestResult {
        match tokio::time::timeout(self.timeout, self.test_stdio_inner(command, args, env)).await {
            Ok(r) => r,
            Err(_) => timeout_result(self.timeout),
        }
    }

    async fn test_stdio_inner(
        &self,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> McpConnectionTestResult {
        let mut cmd = CmdBuilder::new(command);
        cmd.args(args)
            .envs(env.iter())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return spawn_error_result(command, &e),
        };

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let result = run_stdio_protocol(stdin, stdout).await;
        kill_child_tree(&mut child).await;
        result
    }

    // -- HTTP (Streamable HTTP) transport ---------------------------------

    async fn test_http(&self, url: &str, headers: &HashMap<String, String>) -> McpConnectionTestResult {
        match tokio::time::timeout(self.timeout, self.test_http_inner(url, headers)).await {
            Ok(r) => r,
            Err(_) => timeout_result(self.timeout),
        }
    }

    async fn test_http_inner(&self, url: &str, headers: &HashMap<String, String>) -> McpConnectionTestResult {
        let mut req_headers = build_http_headers(headers);
        req_headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().expect("valid header"),
        );
        req_headers.insert(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream".parse().expect("valid header"),
        );

        // 1. initialize
        let init_resp = match self
            .http_post_mcp(url, &req_headers, &build_initialize_request(1))
            .await
        {
            Ok(r) => r,
            Err(result) => return result,
        };
        if let Some(err) = init_resp.rpc.error {
            return rpc_error_result("initialize", &err);
        }

        // Extract session ID for subsequent requests
        if let Some(sid) = init_resp.session_id
            && let Ok(val) = reqwest::header::HeaderValue::from_str(&sid)
        {
            req_headers.insert("mcp-session-id", val);
        }

        // 2. initialized notification (fire-and-forget)
        let _ = self
            .http_client
            .post(url)
            .headers(req_headers.clone())
            .json(&build_initialized_notification())
            .send()
            .await;

        // 3. tools/list
        let tools_resp = match self
            .http_post_mcp(url, &req_headers, &build_tools_list_request(2))
            .await
        {
            Ok(r) => r,
            Err(result) => return result,
        };
        if let Some(err) = tools_resp.rpc.error {
            return rpc_error_result("tools/list", &err);
        }

        success_result(tools_resp.rpc.result)
    }

    /// POST a JSON-RPC message and parse the response.
    ///
    /// Returns `Err(McpConnectionTestResult)` for HTTP-level failures
    /// (connection error, 401, non-success status).
    async fn http_post_mcp(
        &self,
        url: &str,
        headers: &reqwest::header::HeaderMap,
        body: &JsonRpcRequest,
    ) -> Result<HttpMcpResponse, McpConnectionTestResult> {
        let resp = self
            .http_client
            .post(url)
            .headers(headers.clone())
            .json(body)
            .send()
            .await
            .map_err(|e| error_result(format!("Connection failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(protocol::auth_result(resp.headers()));
        }
        if !resp.status().is_success() {
            return Err(error_result(format!("HTTP {} from server", resp.status())));
        }

        let session_id = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let rpc = protocol::parse_http_response(resp).await.map_err(error_result)?;

        Ok(HttpMcpResponse { rpc, session_id })
    }

    // -- SSE transport ----------------------------------------------------

    async fn test_sse(&self, url: &str, headers: &HashMap<String, String>) -> McpConnectionTestResult {
        match tokio::time::timeout(self.timeout, self.test_sse_inner(url, headers)).await {
            Ok(r) => r,
            Err(_) => timeout_result(self.timeout),
        }
    }

    async fn test_sse_inner(&self, url: &str, headers: &HashMap<String, String>) -> McpConnectionTestResult {
        let mut req_headers = build_http_headers(headers);

        // 1. Open SSE connection
        let resp = match self
            .http_client
            .get(url)
            .headers(req_headers.clone())
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return error_result(format!("Connection failed: {e}")),
        };
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return protocol::auth_result(resp.headers());
        }
        if !resp.status().is_success() {
            return error_result(format!("HTTP {} from server", resp.status()));
        }

        // 2. Start SSE reader task
        let (event_tx, mut event_rx) = mpsc::channel::<SseEvent>(16);
        let reader_handle = tokio::spawn(read_sse_events(resp, event_tx));

        req_headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().expect("valid header"),
        );

        let result = self.run_sse_protocol(url, &req_headers, &mut event_rx).await;
        reader_handle.abort();
        result
    }

    async fn run_sse_protocol(
        &self,
        base_url: &str,
        headers: &reqwest::header::HeaderMap,
        event_rx: &mut mpsc::Receiver<SseEvent>,
    ) -> McpConnectionTestResult {
        // 3. Wait for endpoint event
        let endpoint = match wait_for_endpoint(event_rx, base_url).await {
            Ok(ep) => ep,
            Err(e) => return error_result(e),
        };

        // 4. initialize
        if let Err(e) = self.sse_post(&endpoint, headers, &build_initialize_request(1)).await {
            return error_result(format!("Failed to send initialize: {e}"));
        }
        let init_resp = match wait_for_jsonrpc_response(event_rx).await {
            Ok(r) => r,
            Err(e) => return error_result(format!("initialize response: {e}")),
        };
        if let Some(err) = init_resp.error {
            return rpc_error_result("initialize", &err);
        }

        // 5. initialized notification
        let _ = self
            .sse_post(&endpoint, headers, &build_initialized_notification())
            .await;

        // 6. tools/list
        if let Err(e) = self.sse_post(&endpoint, headers, &build_tools_list_request(2)).await {
            return error_result(format!("Failed to send tools/list: {e}"));
        }
        let tools_resp = match wait_for_jsonrpc_response(event_rx).await {
            Ok(r) => r,
            Err(e) => return error_result(format!("tools/list response: {e}")),
        };
        if let Some(err) = tools_resp.error {
            return rpc_error_result("tools/list", &err);
        }

        success_result(tools_resp.result)
    }

    /// POST a JSON-RPC message to an SSE endpoint (fire-and-forget semantics).
    async fn sse_post<T: Serialize>(
        &self,
        endpoint: &str,
        headers: &reqwest::header::HeaderMap,
        body: &T,
    ) -> Result<(), String> {
        self.http_client
            .post(endpoint)
            .headers(headers.clone())
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Intermediate struct for HTTP transport response parsing.
struct HttpMcpResponse {
    rpc: JsonRpcResponse,
    session_id: Option<String>,
}

async fn kill_child_tree(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        #[cfg(unix)]
        {
            let process_group = format!("-{pid}");
            let _ = tokio::process::Command::new("kill")
                .args(["-KILL", &process_group])
                .status()
                .await;
        }

        #[cfg(windows)]
        {
            let _ = tokio::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .status()
                .await;
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = child.kill().await;
        }
    } else {
        let _ = child.kill().await;
    }

    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_clone() {
        let svc = McpConnectionTestService::new(reqwest::Client::new());
        let _cloned = svc.clone();
    }

    #[test]
    fn service_with_timeout() {
        let svc = McpConnectionTestService::new(reqwest::Client::new()).with_timeout(Duration::from_secs(5));
        assert_eq!(svc.timeout, Duration::from_secs(5));
    }
}
