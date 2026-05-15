//! Opinionated wrapper around [`tokio::process::Command`] that centralises
//! cross-cutting concerns of child-process spawning across the workspace.
//!
//! Two construction flavours are provided:
//!
//! * [`Builder::new`] — for long-running agent CLIs whose stdio is owned
//!   by the caller (e.g. ACP SDK). Defaults to inherited stdio. Callers
//!   typically override to `piped()` to capture the streams.
//!
//! * [`Builder::clean_cli`] — for short-lived CLI tools whose output we
//!   capture and parse. Defaults to piped stdio plus `NO_COLOR=1` and
//!   `TERM=dumb` so ANSI escape codes do not leak into the captured
//!   output.
//!
//! Both flavours:
//! * set `kill_on_drop(true)` so a panicking / erroring caller cannot
//!   leave orphaned children;
//! * remove `NODE_OPTIONS`, `NODE_INSPECT`, `NODE_DEBUG`, `CLAUDECODE`
//!   so the child doesn't inherit debug/agent state that belongs to the
//!   parent (matches v1 `acpConnectors.ts::getCleanAgentEnv`).
//!
//! Enhanced `PATH` (including the bundled bun directory) is handled
//! once at process startup by [`crate::enhance_process_path`]; Builder
//! does not re-inject it.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;
use std::process::Stdio;

use tokio::process::{Child, Command};

use crate::resolver::resolve_command_path;

/// Construction mode — determines default stdio + env extras.
#[derive(Debug, Clone, Copy)]
enum Mode {
    Default,
    CleanCli,
}

pub struct Builder {
    inner: Command,
    mode: Mode,
}

impl std::fmt::Debug for Builder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Builder")
            .field("mode", &self.mode)
            .field("command", self.inner.as_std())
            .finish()
    }
}

impl Builder {
    /// Builder for long-running agent subprocesses (ACP SDK, legacy CLI).
    ///
    /// Defaults:
    /// - stdio: inherit (callers typically override with `.stdin(piped())`
    ///   etc. when they need to own the streams)
    /// - `kill_on_drop(true)`
    /// - removes `NODE_OPTIONS`, `NODE_INSPECT`, `NODE_DEBUG`, `CLAUDECODE`
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        let mut inner = Command::new(resolve_program(program.as_ref()));
        inner.kill_on_drop(true);
        strip_pollution(&mut inner);
        Self {
            inner,
            mode: Mode::Default,
        }
    }

    /// Builder for short-lived CLI tools whose output we capture.
    ///
    /// Defaults:
    /// - stdio: all piped
    /// - `kill_on_drop(true)`
    /// - removes `NODE_OPTIONS`, `NODE_INSPECT`, `NODE_DEBUG`, `CLAUDECODE`
    /// - sets `NO_COLOR=1`, `TERM=dumb`
    pub fn clean_cli<S: AsRef<OsStr>>(program: S) -> Self {
        let mut inner = Command::new(resolve_program(program.as_ref()));
        inner
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("NO_COLOR", "1")
            .env("TERM", "dumb");
        strip_pollution(&mut inner);
        Self {
            inner,
            mode: Mode::CleanCli,
        }
    }

    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        self.inner.arg(arg);
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.inner.args(args);
        self
    }

    pub fn env<K, V>(&mut self, key: K, val: V) -> &mut Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.inner.env(key, val);
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.inner.envs(vars);
        self
    }

    pub fn env_remove<K: AsRef<OsStr>>(&mut self, key: K) -> &mut Self {
        self.inner.env_remove(key);
        self
    }

    pub fn current_dir<P: AsRef<Path>>(&mut self, dir: P) -> &mut Self {
        self.inner.current_dir(dir);
        self
    }

    pub fn stdin<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.inner.stdin(cfg);
        self
    }

    pub fn stdout<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.inner.stdout(cfg);
        self
    }

    pub fn stderr<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.inner.stderr(cfg);
        self
    }

    /// Spawn the process and return the standard `tokio::process::Child`.
    pub fn spawn(mut self) -> io::Result<Child> {
        self.inner.spawn()
    }

    /// Run to completion and collect stdout/stderr.
    pub async fn output(mut self) -> io::Result<std::process::Output> {
        self.inner.output().await
    }
}

fn strip_pollution(cmd: &mut Command) {
    cmd.env_remove("NODE_OPTIONS")
        .env_remove("NODE_INSPECT")
        .env_remove("NODE_DEBUG")
        .env_remove("CLAUDECODE");
}

/// Resolve `program` through `resolve_command_path` so callers don't have
/// to. If the input already contains a path separator (relative or
/// absolute) we leave it alone — only bare command names go through
/// the resolver, where the bundled-bun shim and Windows `.cmd / .ps1 /
/// .bat` fallbacks live.
fn resolve_program(program: &OsStr) -> OsString {
    if let Some(s) = program.to_str()
        && !s.is_empty()
        && !s.contains('/')
        && !s.contains('\\')
        && let Some(path) = resolve_command_path(s)
    {
        return path.into_os_string();
    }
    program.to_os_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clean_cli_captures_stdout_and_strips_env_pollution() {
        // Set pollution on parent — it must not leak into child.
        // SAFETY: single-threaded test. Rust 2024 requires unsafe.
        unsafe {
            std::env::set_var("NODE_OPTIONS", "--inspect=9229");
            std::env::set_var("CLAUDECODE", "1");
        }

        // Ask the child to print NODE_OPTIONS + CLAUDECODE; Builder must
        // have removed them.
        let mut b = Builder::clean_cli("sh");
        b.arg("-c")
            .arg("echo \"NO:${NODE_OPTIONS:-unset} CC:${CLAUDECODE:-unset}\"");
        let output = b.output().await.unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("NO:unset"), "got: {stdout}");
        assert!(stdout.contains("CC:unset"), "got: {stdout}");
        assert!(output.status.success());

        // SAFETY: single-threaded test cleanup.
        unsafe {
            std::env::remove_var("NODE_OPTIONS");
            std::env::remove_var("CLAUDECODE");
        }
    }

    #[tokio::test]
    async fn clean_cli_sets_no_color_and_term_dumb() {
        let mut b = Builder::clean_cli("sh");
        b.arg("-c").arg("echo \"NC:${NO_COLOR:-unset} TERM:${TERM:-unset}\"");
        let output = b.output().await.unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("NC:1"), "got: {stdout}");
        assert!(stdout.contains("TERM:dumb"), "got: {stdout}");
    }

    #[tokio::test]
    async fn agent_allows_stdio_override() {
        // agent() defaults to inherit. Override to piped, then verify
        // we can capture output.
        let mut b = Builder::new("sh");
        b.arg("-c").arg("echo hello").stdout(Stdio::piped());
        let output = b.output().await.unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(stdout.trim(), "hello");
    }

    #[tokio::test]
    async fn agent_strips_env_pollution() {
        // SAFETY: single-threaded test.
        unsafe {
            std::env::set_var("NODE_INSPECT", "9229");
            std::env::set_var("NODE_DEBUG", "*");
        }

        let mut b = Builder::new("sh");
        b.arg("-c")
            .arg("echo \"NI:${NODE_INSPECT:-unset} ND:${NODE_DEBUG:-unset}\"")
            .stdout(Stdio::piped());
        let output = b.output().await.unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("NI:unset"), "got: {stdout}");
        assert!(stdout.contains("ND:unset"), "got: {stdout}");

        // SAFETY: single-threaded cleanup.
        unsafe {
            std::env::remove_var("NODE_INSPECT");
            std::env::remove_var("NODE_DEBUG");
        }
    }

    #[tokio::test]
    async fn spawn_returns_child_with_pid() {
        let mut b = Builder::new("sh");
        b.arg("-c").arg("sleep 0.05");
        let mut child = b.spawn().unwrap();
        assert!(child.id().is_some());
        let status = child.wait().await.unwrap();
        assert!(status.success());
    }
}
