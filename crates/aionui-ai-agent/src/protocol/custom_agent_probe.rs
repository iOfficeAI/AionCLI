//! Two-step probe for custom ACP agents.
//!
//! Step 1: `which`/`where` — resolve the first token of `command` on
//!         `$PATH`. Bounded by `execFileSync`-equivalent 5 s timeout.
//! Step 2: Spawn the CLI via `CliAgentProcess::spawn_for_sdk`, connect
//!         an `AcpProtocol` (which owns the ACP `initialize` handshake
//!         with a built-in 30 s timeout), then shut down cleanly.
//!
//! The same function is called by:
//!   - `POST /api/agents/custom/try-connect`  (manual "test connection" button)
//!   - `AgentService::create/update_custom_agent`   (test-on-save)
//!
//! Both paths produce identical outcomes / error text.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use aionui_api_types::TryConnectCustomAgentResponse;
use aionui_common::{CommandSpec, EnvVar};
use tokio::sync::{broadcast, mpsc};
use tracing::debug;

use crate::capability::cli_process::CliAgentProcess;
use crate::protocol::acp::AcpProtocol;

/// Step 2 overall timeout. Belt-and-suspenders: `AcpProtocol::connect`
/// already caps the initialize RPC at 30 s, but a CLI that hangs
/// before writing any ACP frame at all is covered by this outer cap.
const STEP2_TIMEOUT: Duration = Duration::from_secs(35);

/// Probe a custom ACP agent.
///
/// Returns `Success` only if both `which` and the ACP `initialize`
/// handshake succeed. Any failure short-circuits into the
/// corresponding variant.
pub async fn try_connect_custom_agent(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> TryConnectCustomAgentResponse {
    // ── Step 1 — which check ────────────────────────────────────────
    let Some(resolved) = step1_resolve_command(command) else {
        return TryConnectCustomAgentResponse::FailCli {
            error: format!("Command '{}' was not found on PATH", first_token(command)),
        };
    };
    debug!(?resolved, "probe step 1 ok");

    // ── Step 2 — spawn + ACP initialize ─────────────────────────────
    match tokio::time::timeout(STEP2_TIMEOUT, step2_acp_initialize(resolved, args, env)).await {
        Ok(Ok(())) => TryConnectCustomAgentResponse::Success,
        Ok(Err(msg)) => TryConnectCustomAgentResponse::FailAcp { error: msg },
        Err(_) => TryConnectCustomAgentResponse::FailAcp {
            error: format!("ACP initialize did not complete within {}s", STEP2_TIMEOUT.as_secs()),
        },
    }
}

fn first_token(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or(command)
}

fn step1_resolve_command(command: &str) -> Option<PathBuf> {
    let head = first_token(command);
    // Reuse the same bun/bunx resolver builtin probing uses so that
    // `bun` commands work even with the bundled runtime.
    match head {
        "bun" => aionui_runtime::resolve_bun().ok(),
        "bunx" => {
            let bunx_name = if cfg!(windows) { "bunx.exe" } else { "bunx" };
            if let Some(dir) = aionui_runtime::bun_bin_dir() {
                let p = dir.join(bunx_name);
                if p.exists() {
                    return Some(p);
                }
            }
            which::which("bunx").ok()
        }
        other => which::which(other).ok(),
    }
}

async fn step2_acp_initialize(
    resolved: PathBuf,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<(), String> {
    let spec = CommandSpec {
        command: resolved,
        args: args.to_vec(),
        env: env
            .iter()
            .map(|(name, value)| EnvVar {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
        cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
    };

    let proc = CliAgentProcess::spawn_for_sdk(spec)
        .await
        .map_err(|e| format!("spawn failed: {e}"))?;

    let (stdin, stdout) = proc
        .take_stdio()
        .await
        .ok_or_else(|| "stdio not available after spawn_for_sdk".to_string())?;

    // Throwaway channels — we only care about init handshake succeeding.
    let (event_tx, _event_rx) = broadcast::channel(16);
    let (permission_tx, _permission_rx) = mpsc::channel(4);
    let (notification_tx, _notification_rx) = mpsc::channel(4);

    let protocol = AcpProtocol::connect(stdin, stdout, event_tx, permission_tx, notification_tx)
        .await
        .map_err(|e| format!("ACP initialize failed: {e}"))?;

    // Dropping `protocol` fires the shutdown oneshot; the child process
    // was spawned with `kill_on_drop(true)` via `aionui_runtime::Builder`
    // so CPU stays clean.
    drop(protocol);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn probe_returns_fail_cli_when_command_missing() {
        let resp = try_connect_custom_agent(
            "aionui-definitely-does-not-exist-xyz",
            &[],
            &HashMap::new(),
        )
        .await;
        match resp {
            TryConnectCustomAgentResponse::FailCli { error } => {
                let lower = error.to_lowercase();
                assert!(
                    lower.contains("not found") || lower.contains("no such") || lower.contains("was not found"),
                    "expected 'not found' style message, got: {error}"
                );
            }
            other => panic!("expected FailCli, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_returns_fail_acp_when_command_is_noop() {
        // `true` exits 0 immediately — Step 1 passes (on PATH), but the
        // process dies before ACP initialize completes, so Step 2 maps
        // to FailAcp.
        if cfg!(windows) {
            // `true` is a cmd builtin on Windows, not a standalone exe.
            return;
        }
        let resp = try_connect_custom_agent("true", &[], &HashMap::new()).await;
        assert!(
            matches!(resp, TryConnectCustomAgentResponse::FailAcp { .. }),
            "expected FailAcp, got {resp:?}"
        );
    }
}
