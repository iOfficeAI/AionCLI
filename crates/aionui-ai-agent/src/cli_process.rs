use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use aionui_common::{AppError, CommandSpec};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, broadcast, watch};
use tracing::{debug, error, trace, warn};

/// Wrapper to hold a pre-subscribed receiver from before background tasks start.
/// Ensures no events are lost between process spawn and consumer subscription.
type InitialReceiver = std::sync::Mutex<Option<broadcast::Receiver<serde_json::Value>>>;

/// Default broadcast channel capacity for stdout events.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Maximum stderr ring-buffer size in bytes.
const STDERR_BUFFER_MAX: usize = 8192;

/// Manages a CLI subprocess with optional JSON-over-stdin/stdout communication.
///
/// Supports two modes:
///
/// 1. **Legacy mode** (Gemini, OpenClaw, Nanobot): stdout is read as line-delimited
///    JSON and broadcast via `subscribe()`. Messages are sent via `send()`.
///
/// 2. **SDK mode** (ACP): call [`take_stdio`](Self::take_stdio) to hand raw
///    stdin/stdout to the ACP SDK transport. After this, `send()` and `subscribe()`
///    are no longer available.
pub struct CliAgentProcess {
    /// Stdin writer, wrapped in Mutex for concurrent send safety.
    /// Set to `None` once stdin is closed, taken, or process exited.
    stdin: Mutex<Option<ChildStdin>>,
    /// Raw stdout handle. Only available before background tasks start or
    /// in SDK mode (taken by `take_stdio`). `None` once consumed.
    stdout: Mutex<Option<ChildStdout>>,
    /// OS-level process ID.
    pid: u32,
    /// Broadcast sender for parsed stdout events (legacy mode only).
    event_tx: broadcast::Sender<serde_json::Value>,
    /// Watch channel that transitions from `None` → `Some(ExitStatus)` on exit.
    exit_rx: watch::Receiver<Option<ExitStatus>>,
    /// Pre-subscribed receiver created before background tasks start (legacy mode).
    /// Take this via [`take_initial_receiver`] to guarantee no events are lost.
    initial_rx: InitialReceiver,
    /// Stderr ring buffer for diagnostics.
    stderr_buffer: Arc<Mutex<String>>,
    /// Handle to the stdout reader task (legacy mode, for cleanup).
    _stdout_handle: Option<Arc<tokio::task::JoinHandle<()>>>,
    /// Handle to the stderr reader task (for cleanup).
    _stderr_handle: Arc<tokio::task::JoinHandle<()>>,
    /// Handle to the exit monitor task (for cleanup).
    _exit_handle: Arc<tokio::task::JoinHandle<()>>,
}

impl CliAgentProcess {
    /// Spawn a new CLI subprocess in **legacy mode**.
    ///
    /// The child process is started with stdin, stdout, and stderr piped.
    /// Background tasks are spawned to:
    /// - Read stdout line-by-line and parse each line as JSON
    /// - Read stderr and buffer the last [`STDERR_BUFFER_MAX`] bytes
    /// - Monitor process exit
    ///
    /// This is used by Gemini, OpenClaw, Nanobot agents.
    pub async fn spawn(config: CommandSpec) -> Result<Self, AppError> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .envs(config.env.iter().map(|e| (&e.name, &e.value)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        if let Some(ref cwd) = config.cwd {
            cmd.current_dir(cwd);
        }

        let mut child: Child = cmd.spawn().map_err(|e| {
            error!(command = %config.command.display(), error = %e, "Failed to spawn CLI process");
            AppError::Internal(format!(
                "Failed to spawn CLI process '{}': {}",
                config.command.display(), e
            ))
        })?;

        let pid = child.id().ok_or_else(|| {
            AppError::Internal("Failed to obtain PID from spawned process".into())
        })?;
        debug!(pid, command = %config.command.display(), "CLI process spawned");

        let stdout = child.stdout.take().ok_or_else(|| {
            AppError::Internal("Failed to capture stdout from child process".into())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AppError::Internal("Failed to capture stderr from child process".into())
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            AppError::Internal("Failed to capture stdin for child process".into())
        })?;

        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        // Pre-subscribe before spawning background tasks to guarantee no events are lost
        let initial_rx = event_tx.subscribe();
        let (exit_tx, exit_rx) = watch::channel(None);

        // Background task: read stdout line-by-line → parse JSON → broadcast
        let stdout_tx = event_tx.clone();
        let stdout_handle = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                match serde_json::from_str::<serde_json::Value>(trimmed) {
                    Ok(value) => {
                        // Ignore send errors — no active subscribers is fine
                        let _ = stdout_tx.send(value);
                    }
                    Err(e) => {
                        trace!(line = trimmed, error = %e, "Non-JSON line from stdout, skipping");
                    }
                }
            }

            debug!(pid, "Stdout reader finished");
        });

        // Background task: read stderr → ring buffer + log
        let stderr_buffer = Arc::new(Mutex::new(String::new()));
        let stderr_buf_clone = Arc::clone(&stderr_buffer);
        let stderr_handle = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    warn!(pid, stderr = trimmed, "CLI process stderr");
                }
                let mut buf = stderr_buf_clone.lock().await;
                buf.push_str(&line);
                buf.push('\n');
                if buf.len() > STDERR_BUFFER_MAX {
                    let cut = buf.len() - STDERR_BUFFER_MAX;
                    buf.drain(..cut);
                }
            }

            debug!(pid, "Stderr reader finished");
        });

        // Background task: monitor process exit
        let exit_handle = tokio::spawn(async move {
            match child.wait().await {
                Ok(status) => {
                    debug!(pid, ?status, "CLI process exited");
                    let _ = exit_tx.send(Some(status));
                }
                Err(e) => {
                    error!(pid, error = %e, "Failed to wait on CLI process");
                    // Signal exit even on error so callers don't hang
                    let _ = exit_tx.send(None);
                }
            }
        });

        Ok(Self {
            stdin: Mutex::new(Some(stdin)),
            stdout: Mutex::new(None), // stdout consumed by reader task
            pid,
            event_tx,
            exit_rx,
            initial_rx: std::sync::Mutex::new(Some(initial_rx)),
            stderr_buffer,
            _stdout_handle: Some(Arc::new(stdout_handle)),
            _stderr_handle: Arc::new(stderr_handle),
            _exit_handle: Arc::new(exit_handle),
        })
    }

    /// Spawn a new CLI subprocess in **SDK mode**.
    ///
    /// Unlike [`spawn`](Self::spawn), this does NOT start a stdout reader task.
    /// Instead, the raw stdin/stdout handles are available via [`take_stdio`](Self::take_stdio)
    /// for the ACP SDK transport to own.
    ///
    /// Background tasks are still spawned for:
    /// - stderr buffering
    /// - Process exit monitoring
    pub async fn spawn_for_sdk(config: CommandSpec) -> Result<Self, AppError> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .envs(config.env.iter().map(|e| (&e.name, &e.value)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        if let Some(ref cwd) = config.cwd {
            cmd.current_dir(cwd);
        }

        let mut child: Child = cmd.spawn().map_err(|e| {
            error!(command = %config.command.display(), error = %e, "Failed to spawn CLI process");
            AppError::Internal(format!(
                "Failed to spawn CLI process '{}': {}",
                config.command.display(), e
            ))
        })?;

        let pid = child.id().ok_or_else(|| {
            AppError::Internal("Failed to obtain PID from spawned process".into())
        })?;
        debug!(pid, command = %config.command.display(), "CLI process spawned (SDK mode)");

        let stdout = child.stdout.take().ok_or_else(|| {
            AppError::Internal("Failed to capture stdout from child process".into())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AppError::Internal("Failed to capture stderr from child process".into())
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            AppError::Internal("Failed to capture stdin for child process".into())
        })?;

        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (exit_tx, exit_rx) = watch::channel(None);

        // Background task: read stderr → ring buffer + log
        let stderr_buffer = Arc::new(Mutex::new(String::new()));
        let stderr_buf_clone = Arc::clone(&stderr_buffer);
        let stderr_handle = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    warn!(pid, stderr = trimmed, "CLI process stderr");
                }
                let mut buf = stderr_buf_clone.lock().await;
                buf.push_str(&line);
                buf.push('\n');
                if buf.len() > STDERR_BUFFER_MAX {
                    let cut = buf.len() - STDERR_BUFFER_MAX;
                    buf.drain(..cut);
                }
            }

            debug!(pid, "Stderr reader finished");
        });

        // Background task: monitor process exit
        let exit_handle = tokio::spawn(async move {
            match child.wait().await {
                Ok(status) => {
                    debug!(pid, ?status, "CLI process exited");
                    let _ = exit_tx.send(Some(status));
                }
                Err(e) => {
                    error!(pid, error = %e, "Failed to wait on CLI process");
                    let _ = exit_tx.send(None);
                }
            }
        });

        Ok(Self {
            stdin: Mutex::new(Some(stdin)),
            stdout: Mutex::new(Some(stdout)),
            pid,
            event_tx,
            exit_rx,
            initial_rx: std::sync::Mutex::new(None),
            stderr_buffer,
            _stdout_handle: None,
            _stderr_handle: Arc::new(stderr_handle),
            _exit_handle: Arc::new(exit_handle),
        })
    }

    /// Take ownership of stdin and stdout for the SDK transport.
    ///
    /// Only available in SDK mode (after [`spawn_for_sdk`](Self::spawn_for_sdk)).
    /// Can only be called once. Returns `None` on subsequent calls or if
    /// spawned in legacy mode.
    pub async fn take_stdio(&self) -> Option<(ChildStdin, ChildStdout)> {
        let stdin = self.stdin.lock().await.take()?;
        let stdout = self.stdout.lock().await.take()?;
        Some((stdin, stdout))
    }

    /// Send a JSON message to the subprocess via stdin (legacy mode).
    ///
    /// The message is serialized as a single line followed by a newline.
    /// Returns an error if stdin has been closed (process exited) or taken
    /// by [`take_stdio`](Self::take_stdio).
    pub async fn send(&self, message: &serde_json::Value) -> Result<(), AppError> {
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| {
            AppError::Internal("Cannot send: stdin is closed (process exited or taken)".into())
        })?;

        let mut buf = serde_json::to_vec(message)
            .map_err(|e| AppError::Internal(format!("Failed to serialize message: {e}")))?;
        buf.push(b'\n');

        stdin.write_all(&buf).await.map_err(|e| {
            error!(pid = self.pid, error = %e, "Failed to write to stdin");
            AppError::Internal(format!("Failed to write to stdin: {e}"))
        })?;

        stdin.flush().await.map_err(|e| {
            error!(pid = self.pid, error = %e, "Failed to flush stdin");
            AppError::Internal(format!("Failed to flush stdin: {e}"))
        })?;

        Ok(())
    }

    /// Subscribe to the event stream from stdout (legacy mode).
    ///
    /// Returns a broadcast receiver that yields raw `serde_json::Value` events
    /// as they are parsed from the subprocess stdout.
    pub fn subscribe(&self) -> broadcast::Receiver<serde_json::Value> {
        self.event_tx.subscribe()
    }

    /// Take the pre-subscribed receiver created before background tasks started
    /// (legacy mode).
    ///
    /// This receiver captures all events from the very first output line.
    /// Can only be called once; subsequent calls return `None`.
    pub fn take_initial_receiver(&self) -> Option<broadcast::Receiver<serde_json::Value>> {
        self.initial_rx.lock().unwrap().take()
    }

    /// Close stdin, signaling the subprocess that no more input will arrive.
    pub async fn close_stdin(&self) {
        let mut guard = self.stdin.lock().await;
        if guard.take().is_some() {
            debug!(pid = self.pid, "Stdin closed");
        }
    }

    /// Gracefully terminate the subprocess.
    ///
    /// 1. Close stdin
    /// 2. Wait up to `grace_period` for the process to exit on its own
    /// 3. If still running after grace period, send SIGKILL
    pub async fn kill(&self, grace_period: Duration) -> Result<(), AppError> {
        // Close stdin first to signal the child
        self.close_stdin().await;

        // Wait for graceful exit within the grace period
        let mut rx = self.exit_rx.clone();
        let exited = tokio::time::timeout(grace_period, async {
            // If already exited, return immediately
            if rx.borrow().is_some() {
                return;
            }
            // Wait for state change
            let _ = rx.changed().await;
        })
        .await;

        if exited.is_ok() && self.exit_rx.borrow().is_some() {
            debug!(pid = self.pid, "CLI process exited gracefully");
            return Ok(());
        }

        // Force kill
        warn!(pid = self.pid, "Grace period expired, sending SIGKILL");
        force_kill(self.pid)
    }

    /// Check whether the subprocess is still running.
    pub fn is_running(&self) -> bool {
        self.exit_rx.borrow().is_none()
    }

    /// Get the exit status if the process has exited.
    pub fn exit_status(&self) -> Option<ExitStatus> {
        *self.exit_rx.borrow()
    }

    /// Get the OS process ID.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Wait for the process to exit (blocks until exit or cancellation).
    pub async fn wait_for_exit(&self) -> Option<ExitStatus> {
        let mut rx = self.exit_rx.clone();
        // If already exited, return immediately
        if let Some(status) = *rx.borrow() {
            return Some(status);
        }
        // Wait for state change
        let _ = rx.changed().await;
        *rx.borrow()
    }

    /// Take the buffered stderr content (consuming).
    ///
    /// Returns the last [`STDERR_BUFFER_MAX`] bytes of stderr output.
    /// Used for error diagnostics in `AcpError::StartupCrash` and
    /// `AcpError::Disconnected`.
    pub async fn take_stderr(&self) -> String {
        let mut buf = self.stderr_buffer.lock().await;
        std::mem::take(&mut *buf)
    }
}

/// Send SIGKILL to a process by PID.
///
/// Uses the system `kill` command to avoid a `libc` dependency.
/// If the process has already exited, this is a no-op.
fn force_kill(pid: u32) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        let result = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output();

        match result {
            Ok(output) if output.status.success() => {
                debug!(pid, "SIGKILL sent successfully");
                Ok(())
            }
            Ok(_output) => {
                // Non-zero exit likely means process already exited — acceptable
                debug!(pid, "Process already exited before SIGKILL");
                Ok(())
            }
            Err(e) => {
                error!(pid, error = %e, "Failed to execute kill command");
                Err(AppError::Internal(format!(
                    "Failed to kill process {pid}: {e}"
                )))
            }
        }
    }
    #[cfg(not(unix))]
    {
        Err(AppError::Internal(format!(
            "Force kill not supported on this platform for pid {pid}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use aionui_common::EnvVar;

    use super::*;
    use serde_json::json;
    use tokio::time::timeout;

    fn echo_json_config(json_str: &str) -> CommandSpec {
        CommandSpec {
            command: "sh".into(),
            args: vec!["-c".into(), format!("echo '{json_str}'")],
            env: vec![],
            cwd: None,
        }
    }

    fn simple_script_config(script: &str) -> CommandSpec {
        CommandSpec {
            command: "sh".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![],
            cwd: None,
        }
    }

    // ── Legacy mode tests ────────────────────────────────────────────

    #[tokio::test]
    async fn spawn_and_receive_event() {
        let config = echo_json_config(r#"{"type":"text","data":{"content":"hello"}}"#);
        let proc = CliAgentProcess::spawn(config).await.unwrap();
        let mut rx = proc.subscribe();

        let event = timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Timed out waiting for event")
            .expect("Channel closed");

        assert_eq!(event["type"], "text");
        assert_eq!(event["data"]["content"], "hello");

        let status = timeout(Duration::from_secs(5), proc.wait_for_exit())
            .await
            .expect("Timed out waiting for exit");
        assert!(status.is_some());
    }

    #[tokio::test]
    async fn spawn_multiple_events() {
        let script = r#"echo '{"type":"start","data":{}}' && echo '{"type":"text","data":{"content":"line1"}}' && echo '{"type":"finish","data":{}}'  "#;
        let config = simple_script_config(script);
        let proc = CliAgentProcess::spawn(config).await.unwrap();
        let mut rx = proc.subscribe();

        let mut events = Vec::new();
        for _ in 0..3 {
            let event = timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("Timed out")
                .expect("Channel closed");
            events.push(event);
        }

        assert_eq!(events[0]["type"], "start");
        assert_eq!(events[1]["type"], "text");
        assert_eq!(events[2]["type"], "finish");
    }

    #[tokio::test]
    async fn non_json_lines_are_skipped() {
        let script = r#"echo 'not json' && echo '{"type":"ok","data":{}}' && echo 'also not json'"#;
        let config = simple_script_config(script);
        let proc = CliAgentProcess::spawn(config).await.unwrap();
        let mut rx = proc.subscribe();

        let event = timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Timed out")
            .expect("Channel closed");

        assert_eq!(event["type"], "ok");
        proc.wait_for_exit().await;
    }

    #[tokio::test]
    async fn empty_lines_are_skipped() {
        let script = "echo '' && echo '  ' && echo '{\"type\":\"data\",\"data\":{}}' && echo ''";
        let config = simple_script_config(script);
        let proc = CliAgentProcess::spawn(config).await.unwrap();
        let mut rx = proc.subscribe();

        let event = timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Timed out")
            .expect("Channel closed");
        assert_eq!(event["type"], "data");
    }

    #[tokio::test]
    async fn send_json_to_stdin() {
        let config = simple_script_config("read line && echo \"$line\"");
        let proc = CliAgentProcess::spawn(config).await.unwrap();
        let mut rx = proc.subscribe();

        let msg = json!({"type": "sendMessage", "data": {"content": "test"}});
        proc.send(&msg).await.unwrap();
        proc.close_stdin().await;

        let event = timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Timed out")
            .expect("Channel closed");
        assert_eq!(event["type"], "sendMessage");
        assert_eq!(event["data"]["content"], "test");
    }

    #[tokio::test]
    async fn send_after_exit_returns_error() {
        let config = simple_script_config("true");
        let proc = CliAgentProcess::spawn(config).await.unwrap();

        proc.wait_for_exit().await;
        proc.close_stdin().await;

        let result = proc.send(&json!({"type":"test"})).await;
        assert!(result.is_err());
    }

    // ── Lifecycle tests (apply to both modes) ────────────────────────

    #[tokio::test]
    async fn is_running_reflects_process_state() {
        let config = simple_script_config("sleep 10");
        let proc = CliAgentProcess::spawn(config).await.unwrap();

        assert!(proc.is_running());
        assert!(proc.exit_status().is_none());

        proc.kill(Duration::from_millis(100)).await.unwrap();

        timeout(Duration::from_secs(5), proc.wait_for_exit())
            .await
            .unwrap();
        assert!(!proc.is_running());
        assert!(proc.exit_status().is_some());
    }

    #[tokio::test]
    async fn kill_with_grace_period_exits_cleanly() {
        let config = simple_script_config("read line");
        let proc = CliAgentProcess::spawn(config).await.unwrap();
        assert!(proc.is_running());

        proc.kill(Duration::from_secs(5)).await.unwrap();
        assert!(!proc.is_running());
    }

    #[tokio::test]
    async fn kill_force_kills_after_grace_period() {
        let config = simple_script_config("trap '' TERM; while true; do sleep 1; done");
        let proc = CliAgentProcess::spawn(config).await.unwrap();
        assert!(proc.is_running());

        let result = proc.kill(Duration::from_millis(100)).await;
        assert!(result.is_ok());

        timeout(Duration::from_secs(5), proc.wait_for_exit())
            .await
            .unwrap();
        assert!(!proc.is_running());
    }

    #[tokio::test]
    async fn spawn_with_env_and_cwd() {
        let config = CommandSpec {
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "echo \"{\\\"val\\\":\\\"$MY_TEST_VAR\\\"}\"".into(),
            ],
            env: vec![EnvVar { name: "MY_TEST_VAR".into(), value: "hello_env".into() }],
            cwd: Some("/tmp".into()),
        };
        let proc = CliAgentProcess::spawn(config).await.unwrap();
        let mut rx = proc.subscribe();

        let event = timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Timed out")
            .expect("Channel closed");
        assert_eq!(event["val"], "hello_env");
    }

    #[tokio::test]
    async fn spawn_invalid_command_returns_error() {
        let config = CommandSpec {
            command: "/nonexistent/binary/that/does/not/exist".into(),
            args: vec![],
            env: vec![],
            cwd: None,
        };
        let result = CliAgentProcess::spawn(config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn pid_is_nonzero_for_valid_process() {
        let config = simple_script_config("sleep 10");
        let proc = CliAgentProcess::spawn(config).await.unwrap();
        assert!(proc.pid() > 0);
        proc.kill(Duration::from_millis(100)).await.unwrap();
    }

    #[tokio::test]
    async fn wait_for_exit_returns_immediately_if_already_exited() {
        let config = simple_script_config("true");
        let proc = CliAgentProcess::spawn(config).await.unwrap();

        let status1 = timeout(Duration::from_secs(5), proc.wait_for_exit())
            .await
            .expect("Timed out");
        assert!(status1.is_some());

        let status2 = timeout(Duration::from_millis(100), proc.wait_for_exit())
            .await
            .expect("Should return immediately");
        assert!(status2.is_some());
    }

    #[tokio::test]
    async fn multiple_subscribers_receive_same_events() {
        let config = echo_json_config(r#"{"type":"broadcast","data":{"msg":"all"}}"#);
        let proc = CliAgentProcess::spawn(config).await.unwrap();

        let mut rx1 = proc.subscribe();
        let mut rx2 = proc.subscribe();

        let e1 = timeout(Duration::from_secs(5), rx1.recv())
            .await
            .expect("Timed out")
            .expect("Channel closed");
        let e2 = timeout(Duration::from_secs(5), rx2.recv())
            .await
            .expect("Timed out")
            .expect("Channel closed");

        assert_eq!(e1, e2);
        assert_eq!(e1["type"], "broadcast");
    }

    // ── SDK mode tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn spawn_for_sdk_take_stdio() {
        let config = simple_script_config("read line && echo \"$line\"");
        let proc = CliAgentProcess::spawn_for_sdk(config).await.unwrap();

        let stdio = proc.take_stdio().await;
        assert!(stdio.is_some(), "First take_stdio should succeed");

        let stdio_again = proc.take_stdio().await;
        assert!(
            stdio_again.is_none(),
            "Second take_stdio should return None"
        );

        proc.kill(Duration::from_millis(100)).await.unwrap();
    }

    #[tokio::test]
    async fn stderr_captured_in_buffer() {
        let config = simple_script_config("echo 'error line 1' >&2 && echo 'error line 2' >&2");
        let proc = CliAgentProcess::spawn(config).await.unwrap();

        timeout(Duration::from_secs(5), proc.wait_for_exit())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let stderr = proc.take_stderr().await;
        assert!(stderr.contains("error line 1"), "stderr: {stderr}");
        assert!(stderr.contains("error line 2"), "stderr: {stderr}");
    }

    #[tokio::test]
    async fn take_stderr_is_consuming() {
        let config = simple_script_config("echo 'hello' >&2");
        let proc = CliAgentProcess::spawn(config).await.unwrap();

        timeout(Duration::from_secs(5), proc.wait_for_exit())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let first = proc.take_stderr().await;
        assert!(!first.is_empty());

        let second = proc.take_stderr().await;
        assert!(second.is_empty(), "Second take should be empty");
    }
}
