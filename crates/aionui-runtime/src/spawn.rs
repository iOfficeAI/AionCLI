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
//! Enhanced `PATH` (including the bundled node directory) is handled
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

#[derive(Debug, thiserror::Error)]
pub enum ExpandError {
    #[error("unknown placeholder ${{{0}}}")]
    Unknown(String),
}

/// Expand `${NAME}` placeholders in a string against `env`. Strict — an
/// unknown name returns `ExpandError::Unknown(name)`.
fn expand_str(input: &str, env: &std::collections::HashMap<String, String>) -> Result<String, ExpandError> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let end = after_open
            .find('}')
            .ok_or_else(|| ExpandError::Unknown(after_open.into()))?;
        let name = &after_open[..end];
        let value = env.get(name).ok_or_else(|| ExpandError::Unknown(name.to_string()))?;
        out.push_str(value);
        rest = &after_open[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
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

/// Renders the configured spawn as a shell-style preview (`cd … && env -u
/// X K=V <prog> <args>…`) suitable for logs and error messages. Format
/// comes for free from `std::process::Command`'s `Debug` impl.
impl std::fmt::Display for Builder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.inner.as_std(), f)
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
        let resolved_program = resolve_program(program.as_ref());
        // Logical "npm" → spawn `<bundled-node> <npm-cli.js> ...` instead of
        // letting cmd.exe / shell shim layers handle the .cmd shim. Keeps
        // spawn behaviour identical across OS.
        if program.as_ref().to_string_lossy() == "npm"
            && let (Ok(node), Ok(cli)) = (crate::resolver::resolve_node(), crate::resolver::resolve_npm_cli_js())
        {
            let mut inner = Command::new(node);
            inner.arg(cli);
            inner.kill_on_drop(true);
            strip_pollution(&mut inner);
            return Self {
                inner,
                mode: Mode::Default,
            };
        }
        let mut inner = Command::new(resolved_program);
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

    /// Walk over args + env values currently configured on this builder,
    /// rewriting every `${NAME}` token using `env`. An unknown placeholder
    /// produces an error rather than silently substituting empty.
    ///
    /// Note: only the std::process::Command's args + env values are
    /// rewritten — not the program path. Program-path placeholders are
    /// disallowed by spec. Stdio config does NOT survive the rebuild —
    /// callers set it after expand, or this is called pre-stdio in
    /// spawn_sdk.
    pub fn expand_placeholders(
        &mut self,
        env: &std::collections::HashMap<String, String>,
    ) -> Result<&mut Self, ExpandError> {
        let cmd = self.inner.as_std();
        let program = cmd.get_program().to_os_string();
        let cwd = cmd.get_current_dir().map(|p| p.to_path_buf());

        // Collect & rewrite args.
        let mut new_args: Vec<OsString> = Vec::new();
        for raw in cmd.get_args() {
            let s = raw.to_string_lossy();
            let rewritten = expand_str(&s, env)?;
            new_args.push(OsString::from(rewritten));
        }

        // Collect & rewrite env entries (only Some values; removals stay).
        let envs: Vec<(OsString, Option<OsString>)> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_os_string(), v.map(|v| v.to_os_string())))
            .collect();

        // Rebuild Command with rewritten content.
        let mut new_inner = tokio::process::Command::new(program);
        new_inner.kill_on_drop(true);
        for arg in &new_args {
            new_inner.arg(arg);
        }
        if let Some(d) = cwd {
            new_inner.current_dir(d);
        }
        for (k, v) in envs {
            match v {
                Some(value) => {
                    let s = value.to_string_lossy();
                    let rewritten = expand_str(&s, env)?;
                    new_inner.env(&k, OsString::from(rewritten));
                }
                None => {
                    new_inner.env_remove(&k);
                }
            }
        }
        self.inner = new_inner;
        Ok(self)
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
/// the resolver, where Windows `.cmd / .ps1 / .bat` fallbacks live.
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

    #[test]
    fn display_renders_shell_style_command() {
        let mut b = Builder::new("/usr/local/bin/bun");
        b.current_dir("/tmp/work dir")
            .env("FOO", "bar baz")
            .args(["x", "--flag", "with space"]);

        let preview = format!("{b}");
        // Format inherited from std Command Debug: `cd "..." && env -u X K=V "prog" "args"...`
        assert!(
            preview.starts_with(r#"cd "/tmp/work dir" &&"#),
            "missing cwd prefix: {preview}"
        );
        assert!(preview.contains("env "), "expected env section: {preview}");
        assert!(preview.contains(r#"FOO="bar baz""#), "FOO missing: {preview}");
        // strip_pollution unsets these
        assert!(
            preview.contains("-u NODE_OPTIONS"),
            "missing -u NODE_OPTIONS: {preview}"
        );
        assert!(preview.contains("-u CLAUDECODE"), "missing -u CLAUDECODE: {preview}");
        assert!(
            preview.contains(r#""/usr/local/bin/bun""#),
            "program missing: {preview}"
        );
        assert!(preview.contains(r#""--flag""#), "arg --flag missing: {preview}");
        assert!(preview.contains(r#""with space""#), "arg with space missing: {preview}");
    }

    #[test]
    fn expand_placeholders_substitutes_known_vars() {
        let mut b = Builder::new("sh");
        b.arg("--prefix=${AGENT_PREFIX}").arg("--cache=${AGENT_NPM_CACHE}");
        let env = std::collections::HashMap::from([
            ("AGENT_PREFIX".to_string(), "/data/agents/npx/claude-1".to_string()),
            ("AGENT_NPM_CACHE".to_string(), "/data/agents/npx/_npm_cache".to_string()),
        ]);
        b.expand_placeholders(&env).expect("must succeed");

        let preview = format!("{b}");
        assert!(
            preview.contains("--prefix=/data/agents/npx/claude-1"),
            "expanded prefix missing: {preview}"
        );
        assert!(
            preview.contains("--cache=/data/agents/npx/_npm_cache"),
            "expanded cache missing: {preview}"
        );
    }

    #[test]
    fn expand_placeholders_errors_on_unknown_var() {
        let mut b = Builder::new("sh");
        b.arg("--config=${WHO_ARE_YOU}");
        let env = std::collections::HashMap::new();
        let err = b
            .expand_placeholders(&env)
            .expect_err("must fail on unknown placeholder");
        let msg = err.to_string();
        assert!(msg.contains("WHO_ARE_YOU"), "must name the missing key; got {msg}");
    }

    #[test]
    fn expand_placeholders_leaves_literal_when_no_dollar_brace() {
        let mut b = Builder::new("sh");
        b.arg("plain-arg");
        let env = std::collections::HashMap::new();
        b.expand_placeholders(&env).unwrap();
        assert!(format!("{b}").contains("plain-arg"));
    }

    #[test]
    #[cfg(unix)]
    fn builder_for_npm_uses_node_plus_npm_cli_js_when_resolved() {
        // We can't guarantee a bundled node here, but we can guarantee
        // that when AIONUI_NODE_PATH is set + npm-cli.js exists nearby,
        // the resulting program token points to node and arg[0] points
        // to npm-cli.js. Skip when host has no node.
        let Ok(host_node) = which::which("node") else {
            return;
        };
        let host_root = host_node.parent().and_then(|p| p.parent());
        let Some(root) = host_root else {
            return;
        };
        let cli = root.join("lib/node_modules/npm/bin/npm-cli.js");
        if !cli.is_file() {
            return;
        }
        unsafe {
            std::env::set_var("AIONUI_NODE_PATH", &host_node);
        }
        let b = Builder::new("npm");
        let preview = format!("{b}");
        assert!(
            preview.contains("node"),
            "program should be node when bundled is available: {preview}"
        );
        assert!(
            preview.contains("npm-cli.js"),
            "args should start with npm-cli.js: {preview}"
        );
        unsafe {
            std::env::remove_var("AIONUI_NODE_PATH");
        }
    }
}
