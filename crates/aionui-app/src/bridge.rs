//! `aionui-backend mcp-bridge` subcommand: stdio ↔ TCP bridge for the team MCP server.
//!
//! Spawned by the ACP agent CLI as an MCP server with command `aionui-backend mcp-bridge`.
//! stdio side speaks newline-delimited JSON-RPC 2.0 (the MCP stdio transport);
//! TCP side speaks 4-byte big-endian length-prefixed JSON frames against
//! `127.0.0.1:<TEAM_MCP_PORT>` (reusing `aionui_team::mcp::protocol`).
//!
//! On the first `initialize` request from the CLI, the bridge injects
//! `auth_token` and `slot_id` (read from env) into `params` before forwarding
//! to the TCP server; subsequent messages are transparently proxied in both
//! directions. Any unrecoverable error exits non-zero so the ACP CLI marks
//! the MCP server as broken (see docs/teams/mcp.md §4.4 / §4.6).

use std::io::{self, IsTerminal};
use std::process::ExitCode;

use aionui_api_types::TeamMcpStdioConfig;
use aionui_team::mcp::protocol::{read_frame, write_frame};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const CONNECT_ADDR_HOST: &str = "127.0.0.1";

/// Entry point for `aionui-backend mcp-bridge`. Returns an [`ExitCode`] so the
/// binary surfaces non-zero on any failure (ACP CLI uses that to mark the MCP
/// server as broken).
pub async fn run_mcp_bridge() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // stderr, not tracing: the parent agent CLI captures stderr and
            // shows it to the user when the bridge dies on startup.
            eprintln!("mcp-bridge: {err}");
            ExitCode::from(1)
        }
    }
}

async fn run() -> Result<(), String> {
    let env = BridgeEnv::from_env()?;

    let tcp = TcpStream::connect((CONNECT_ADDR_HOST, env.port))
        .await
        .map_err(|e| format!("failed to connect 127.0.0.1:{}: {e}", env.port))?;
    tcp.set_nodelay(true).ok();
    let (tcp_reader, tcp_writer) = tcp.into_split();

    if std::io::stdin().is_terminal() {
        return Err("stdin is a TTY; mcp-bridge must be spawned by an MCP-capable agent CLI".into());
    }
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let env_for_stdin = env.clone();
    let stdin_task = tokio::spawn(async move { forward_stdin_to_tcp(stdin, tcp_writer, env_for_stdin).await });
    let tcp_task = tokio::spawn(async move { forward_tcp_to_stdout(tcp_reader, stdout).await });

    // First task to return decides the exit path; we treat clean EOF from
    // either side as "other side closed, we're done".
    tokio::select! {
        res = stdin_task => {
            res.map_err(|e| format!("stdin task panicked: {e}"))??;
        }
        res = tcp_task => {
            res.map_err(|e| format!("tcp task panicked: {e}"))??;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Env
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct BridgeEnv {
    port: u16,
    token: String,
    slot_id: String,
}

impl BridgeEnv {
    fn from_env() -> Result<Self, String> {
        let port_raw = std::env::var(TeamMcpStdioConfig::ENV_PORT)
            .map_err(|_| format!("missing env var {}", TeamMcpStdioConfig::ENV_PORT))?;
        let port: u16 = port_raw
            .parse()
            .map_err(|e| format!("invalid {} value {port_raw:?}: {e}", TeamMcpStdioConfig::ENV_PORT))?;
        let token = std::env::var(TeamMcpStdioConfig::ENV_TOKEN)
            .map_err(|_| format!("missing env var {}", TeamMcpStdioConfig::ENV_TOKEN))?;
        let slot_id = std::env::var(TeamMcpStdioConfig::ENV_SLOT_ID)
            .map_err(|_| format!("missing env var {}", TeamMcpStdioConfig::ENV_SLOT_ID))?;
        Ok(Self { port, token, slot_id })
    }
}

// ---------------------------------------------------------------------------
// stdin → TCP: read newline-delimited JSON, inject auth on `initialize`, frame to TCP
// ---------------------------------------------------------------------------

async fn forward_stdin_to_tcp<R, W>(stdin: R, mut tcp_writer: W, env: BridgeEnv) -> Result<(), String>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(stdin);
    eprintln!("[mcp-bridge] stdin loop started (Content-Length framing)");
    loop {
        let body = match read_mcp_stdio_message(&mut reader).await {
            Ok(Some(b)) => b,
            Ok(None) => {
                eprintln!("[mcp-bridge] stdin EOF");
                return Ok(());
            }
            Err(e) => return Err(e),
        };

        eprintln!("[mcp-bridge] stdin received: {} bytes", body.len());
        let mut value: Value =
            serde_json::from_slice(&body).map_err(|e| format!("stdin payload is not valid JSON: {e}"))?;

        if value.get("method").and_then(Value::as_str) == Some("initialize") {
            inject_auth(&mut value, &env);
        }

        let bytes = serde_json::to_vec(&value).map_err(|e| format!("serialize outgoing frame: {e}"))?;
        write_frame(&mut tcp_writer, &bytes)
            .await
            .map_err(|e| format!("tcp write error: {e}"))?;
        eprintln!("[mcp-bridge] forwarded to TCP ({} bytes)", bytes.len());
    }
}

/// Read one MCP stdio message (Content-Length framing).
/// Returns None on clean EOF.
async fn read_mcp_stdio_message<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>, String> {
    let mut content_length: Option<usize> = None;
    let mut header_line = String::new();
    loop {
        header_line.clear();
        let n = reader
            .read_line(&mut header_line)
            .await
            .map_err(|e| format!("stdin header read error: {e}"))?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = header_line.trim();
        if trimmed.is_empty() {
            // Empty line = end of headers
            break;
        }
        if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(
                len_str
                    .trim()
                    .parse::<usize>()
                    .map_err(|e| format!("invalid Content-Length: {e}"))?,
            );
        }
        // Ignore other headers
    }
    let len = content_length.ok_or("missing Content-Length header")?;
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|e| format!("stdin body read error: {e}"))?;
    Ok(Some(body))
}

fn inject_auth(value: &mut Value, env: &BridgeEnv) {
    let params = value.as_object_mut().and_then(|obj| {
        obj.entry("params")
            .or_insert(Value::Object(Default::default()))
            .as_object_mut()
    });
    if let Some(params) = params {
        params.insert("auth_token".into(), Value::String(env.token.clone()));
        params.insert("slot_id".into(), Value::String(env.slot_id.clone()));
    }
}

// ---------------------------------------------------------------------------
// TCP → stdout: read length-prefixed frames, write as newline-delimited JSON
// ---------------------------------------------------------------------------

async fn forward_tcp_to_stdout<R, W>(mut tcp_reader: R, mut stdout: W) -> Result<(), String>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    loop {
        let frame = match read_frame(&mut tcp_reader).await {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                eprintln!("[mcp-bridge] TCP EOF");
                return Ok(());
            }
            Err(e) => return Err(format!("tcp read error: {e}")),
        };
        eprintln!("[mcp-bridge] TCP→stdout: {} bytes", frame.len());
        // Content-Length framing for stdout (MCP stdio transport)
        let header = format!("Content-Length: {}\r\n\r\n", frame.len());
        stdout
            .write_all(header.as_bytes())
            .await
            .map_err(|e| format!("stdout write error: {e}"))?;
        stdout
            .write_all(&frame)
            .await
            .map_err(|e| format!("stdout write error: {e}"))?;
        stdout.flush().await.map_err(|e| format!("stdout flush error: {e}"))?;
    }
}

// ---------------------------------------------------------------------------
// Unit tests (integration tests live in tests/mcp_bridge.rs)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env() -> BridgeEnv {
        BridgeEnv {
            port: 1,
            token: "tok".into(),
            slot_id: "slot-a".into(),
        }
    }

    #[test]
    fn inject_auth_adds_fields_when_params_missing() {
        let mut v = json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
        inject_auth(&mut v, &env());
        assert_eq!(v["params"]["auth_token"], "tok");
        assert_eq!(v["params"]["slot_id"], "slot-a");
    }

    #[test]
    fn inject_auth_preserves_existing_params() {
        let mut v = json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params": {"protocolVersion":"2024-11-05","capabilities":{}}
        });
        inject_auth(&mut v, &env());
        assert_eq!(v["params"]["protocolVersion"], "2024-11-05");
        assert_eq!(v["params"]["auth_token"], "tok");
        assert_eq!(v["params"]["slot_id"], "slot-a");
    }

    #[test]
    fn inject_auth_overrides_client_supplied_credentials() {
        // The CLI cannot be trusted to know the bridge's token / slot id,
        // so whatever it sent gets replaced.
        let mut v = json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"auth_token":"stale","slot_id":"wrong"}
        });
        inject_auth(&mut v, &env());
        assert_eq!(v["params"]["auth_token"], "tok");
        assert_eq!(v["params"]["slot_id"], "slot-a");
    }

    #[tokio::test]
    async fn forward_stdin_injects_only_on_initialize() {
        let initialize = br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let tools_list = br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let input = format!(
            "Content-Length: {}\r\n\r\n{}Content-Length: {}\r\n\r\n{}",
            initialize.len(),
            std::str::from_utf8(initialize).unwrap(),
            tools_list.len(),
            std::str::from_utf8(tools_list).unwrap(),
        );
        let mut out = Vec::<u8>::new();
        forward_stdin_to_tcp(input.as_bytes(), &mut out, env()).await.unwrap();

        // Parse two frames back out.
        let mut cursor = std::io::Cursor::new(out);
        let f1 = read_frame(&mut cursor).await.unwrap();
        let f2 = read_frame(&mut cursor).await.unwrap();
        let v1: Value = serde_json::from_slice(&f1).unwrap();
        let v2: Value = serde_json::from_slice(&f2).unwrap();
        assert_eq!(v1["params"]["auth_token"], "tok");
        assert_eq!(v1["params"]["slot_id"], "slot-a");
        assert!(v2.get("params").is_none(), "tools/list params untouched");
    }

    #[tokio::test]
    async fn forward_tcp_writes_content_length_framed_stdout() {
        let payload = br#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
        let mut framed = Vec::new();
        write_frame(&mut framed, payload).await.unwrap();

        let mut out = Vec::<u8>::new();
        forward_tcp_to_stdout(&framed[..], &mut out).await.unwrap();

        let mut cursor = std::io::Cursor::new(out);
        let body = read_mcp_stdio_message(&mut cursor).await.unwrap().unwrap();
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["id"], 1);
    }
}
