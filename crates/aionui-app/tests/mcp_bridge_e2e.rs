//! D6 integration tests for the `aionui-backend mcp-bridge` subcommand.
//!
//! Spawn the production binary with `mcp-bridge` argv; drive its stdin/stdout
//! as the ACP agent CLI would, and verify it transparently bridges to a
//! length-prefixed JSON-RPC TCP peer.

use std::process::Stdio;
use std::time::Duration;

use aionui_team::mcp::protocol::{read_frame, write_frame};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::time::timeout;

const BRIDGE_BIN: &str = env!("CARGO_BIN_EXE_aionui-backend");

async fn write_stdio_message<W: AsyncWriteExt + Unpin>(writer: &mut W, value: &Value) {
    let body = serde_json::to_vec(value).unwrap();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await.unwrap();
    writer.write_all(&body).await.unwrap();
}

async fn read_stdio_message<R: AsyncBufReadExt + AsyncReadExt + Unpin>(reader: &mut R) -> Value {
    let mut content_length = None;
    let mut header_line = String::new();
    loop {
        header_line.clear();
        let n = reader.read_line(&mut header_line).await.unwrap();
        assert!(n > 0, "unexpected EOF while reading stdio headers");
        let trimmed = header_line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(len) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(len.trim().parse::<usize>().unwrap());
        }
    }

    let mut body = vec![0; content_length.expect("missing Content-Length header")];
    reader.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// 1) spawn bridge → mock TCP server accepts → stdin "tools/list" round-trip.
///    Also verifies the bridge injects `auth_token` + `slot_id` into the
///    very first `initialize` request (per interface-contracts §8).
#[tokio::test]
async fn bridge_forwards_initialize_and_tools_list() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // Mock TCP server: accept one connection, answer initialize + tools/list.
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (mut rd, mut wr) = tokio::io::split(stream);

        // --- initialize ---
        let f1 = read_frame(&mut rd).await.unwrap();
        let req1: Value = serde_json::from_slice(&f1).unwrap();
        assert_eq!(req1["method"], "initialize");
        assert_eq!(
            req1["params"]["auth_token"], "secret-tok",
            "bridge must inject env TEAM_MCP_TOKEN into initialize.params"
        );
        assert_eq!(
            req1["params"]["slot_id"], "slot-42",
            "bridge must inject env TEAM_AGENT_SLOT_ID into initialize.params"
        );
        let resp1 = json!({
            "jsonrpc":"2.0",
            "id": req1["id"],
            "result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"mock","version":"0"}}
        });
        write_frame(&mut wr, &serde_json::to_vec(&resp1).unwrap())
            .await
            .unwrap();

        // --- tools/list ---
        let f2 = read_frame(&mut rd).await.unwrap();
        let req2: Value = serde_json::from_slice(&f2).unwrap();
        assert_eq!(req2["method"], "tools/list");
        let resp2 = json!({
            "jsonrpc":"2.0",
            "id": req2["id"],
            "result":{"tools":[{"name":"team_members","description":"fake"}]}
        });
        write_frame(&mut wr, &serde_json::to_vec(&resp2).unwrap())
            .await
            .unwrap();
    });

    let mut child = Command::new(BRIDGE_BIN)
        .arg("mcp-bridge")
        .env("TEAM_MCP_PORT", port.to_string())
        .env("TEAM_MCP_TOKEN", "secret-tok")
        .env("TEAM_AGENT_SLOT_ID", "slot-42")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bridge");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut stdout_reader = BufReader::new(stdout);

    write_stdio_message(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{"protocolVersion":"2024-11-05"}
        }),
    )
    .await;
    write_stdio_message(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/list"
        }),
    )
    .await;
    stdin.flush().await.unwrap();

    let v1 = timeout(Duration::from_secs(5), read_stdio_message(&mut stdout_reader))
        .await
        .expect("initialize response timeout");
    assert_eq!(v1["id"], 1);
    assert_eq!(v1["result"]["protocolVersion"], "2024-11-05");

    let v2 = timeout(Duration::from_secs(5), read_stdio_message(&mut stdout_reader))
        .await
        .expect("tools/list response timeout");
    assert_eq!(v2["id"], 2);
    assert_eq!(v2["result"]["tools"][0]["name"], "team_members");

    // Closing stdin triggers orderly shutdown.
    drop(stdin);
    let _ = timeout(Duration::from_secs(5), child.wait()).await;
    let _ = child.kill().await;
    server.await.unwrap();
}

/// 2) No TCP server listening → bridge must exit non-zero within 1s.
#[tokio::test]
async fn bridge_exits_nonzero_when_tcp_unreachable() {
    // Pick port 1 on loopback: it is privileged for *bind* (so nothing on a
    // normal dev machine is listening there), but `connect` needs no
    // privilege and just gets ECONNREFUSED, which is exactly the failure
    // mode we want to exercise. Avoids the macOS "drop(listener) → port
    // may still accept" race seen with port-0 bind-then-drop.
    let port: u16 = 1;

    let mut child = Command::new(BRIDGE_BIN)
        .arg("mcp-bridge")
        .env("TEAM_MCP_PORT", port.to_string())
        .env("TEAM_MCP_TOKEN", "tok")
        .env("TEAM_AGENT_SLOT_ID", "slot")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bridge");

    // The spec says "bridge must exit within 1s of TCP connect failure".
    // We give 3s of budget because debug-build binary startup (tokio
    // runtime + large deps linking) adds ~1-2s on macOS dev machines; the
    // actual connect-fail → exit delay is tens of milliseconds. What we
    // really assert is "no hung main loop on connect failure", not
    // wall-clock < 1s from spawn.
    let status = timeout(Duration::from_secs(3), child.wait())
        .await
        .expect("bridge did not exit within 3s after TCP connect failure")
        .expect("wait failed");

    assert!(
        !status.success(),
        "bridge must exit non-zero when TCP connect fails, got {status:?}"
    );
}
