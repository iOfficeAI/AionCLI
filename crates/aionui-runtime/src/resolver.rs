//! Public API for the bundled node runtime.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::cache;
use crate::extract::{self, ExtractError};

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("node not found")]
    NotFound,
    #[error("failed to extract embedded node: {0}")]
    Extract(#[from] std::io::Error),
    #[error("embedded node checksum mismatch")]
    ChecksumMismatch,
    #[error("serde_json: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<ExtractError> for ResolveError {
    fn from(err: ExtractError) -> Self {
        match err {
            ExtractError::Io(e) => ResolveError::Extract(e),
            ExtractError::ChecksumMismatch { .. } => ResolveError::ChecksumMismatch,
            ExtractError::Json(e) => ResolveError::Json(e),
        }
    }
}

static RESOLVED_NODE: OnceLock<PathBuf> = OnceLock::new();
static NODE_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Returns the path to the bundled `node` executable.
///
/// Priority: `AIONUI_NODE_PATH` env override > embedded blob (extract on first call) > error.
/// Offline-first + version pinning are explicit goals; there is no `which("node")` fallback
/// when an embed is present.
pub fn resolve_node() -> Result<PathBuf, ResolveError> {
    if let Some(path) = RESOLVED_NODE.get() {
        return Ok(path.clone());
    }
    let resolved = resolve_node_with(&crate::embed::ProductionEmbedNode)?;
    let _ = RESOLVED_NODE.set(resolved.clone());
    Ok(resolved)
}

/// Returns the directory containing the bundled `node` (i.e. `<dir>/bin`).
/// `None` when no embed and no env override.
pub fn node_bin_dir() -> Option<PathBuf> {
    NODE_DIR
        .get_or_init(|| {
            resolve_node_with(&crate::embed::ProductionEmbedNode)
                .ok()
                .and_then(|p| p.parent().map(PathBuf::from))
        })
        .clone()
}

/// Returns the path to the bundled `npm-cli.js`. Errors if `resolve_node`
/// errors, or if the expected file is missing under the node dir.
pub fn resolve_npm_cli_js() -> Result<PathBuf, ResolveError> {
    let node = resolve_node()?;
    // node lives at <root>/bin/node[.exe] → npm-cli.js at <root>/lib/...
    let root = node.parent().and_then(|p| p.parent()).ok_or(ResolveError::NotFound)?;
    let cli = root.join("lib/node_modules/npm/bin/npm-cli.js");
    if cli.is_file() {
        Ok(cli)
    } else {
        Err(ResolveError::NotFound)
    }
}

fn resolve_node_with<E: crate::embed::EmbeddedNode>(embed: &E) -> Result<PathBuf, ResolveError> {
    if let Some(p) = node_env_override() {
        return Ok(p);
    }
    if !embed.has() {
        return which::which("node").map_err(|_| ResolveError::NotFound);
    }
    let dir = cache::node_dir(embed.version(), embed.sha256()).ok_or(ResolveError::NotFound)?;
    let node_bin = dir.join(if cfg!(windows) { "bin/node.exe" } else { "bin/node" });

    if extract::is_node_fresh(&dir, embed.sha256(), embed.version()) && node_bin.is_file() {
        return Ok(node_bin);
    }
    extract::extract_node_into(&dir, embed.blob(), embed.sha256(), embed.version())?;
    Ok(node_bin)
}

fn node_env_override() -> Option<PathBuf> {
    let raw = std::env::var("AIONUI_NODE_PATH").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let p = PathBuf::from(trimmed);
    if p.is_file() {
        Some(p)
    } else {
        tracing::warn!(path = %p.display(), "AIONUI_NODE_PATH does not point to a file; ignoring");
        None
    }
}

/// Resolve a command name to an absolute path.
///
/// For known bundled commands we go through `aionui_runtime` so the bundled
/// runtime is used when present; everything else falls back to the user's
/// `$PATH` via `which::which`.
///
/// On Windows, if a bare name lookup fails we retry with the common shim
/// suffixes (`.cmd`, `.ps1`, `.bat`). Tools installed via npm global / pnpm /
/// yarn typically ship as `name.cmd`, and a user with a trimmed `PATHEXT`
/// would otherwise see them as missing.
pub fn resolve_command_path(cmd: &str) -> Option<PathBuf> {
    which::which(cmd).ok().or_else(|| windows_shim_fallback(cmd))
}

#[cfg(windows)]
fn windows_shim_fallback(cmd: &str) -> Option<PathBuf> {
    // If the caller already passed an extension, no point retrying.
    if Path::new(cmd).extension().is_some() {
        return None;
    }
    for ext in ["cmd", "ps1", "bat"] {
        if let Ok(p) = which::which(format!("{cmd}.{ext}")) {
            return Some(p);
        }
    }
    None
}

#[cfg(not(windows))]
fn windows_shim_fallback(_cmd: &str) -> Option<PathBuf> {
    None
}

/// Resolve `cmd` to an absolute path **within `dir` only** — does not walk
/// `PATH`. Honours `PATHEXT` (so `widget.exe` is found on Windows), and on
/// Windows additionally tries `.cmd`, `.ps1`, `.bat` shim suffixes for
/// npm-/pnpm-installed CLIs whose extension `PATHEXT` may not list.
///
/// `dir` is wrapped via `std::env::join_paths` before being handed to
/// `which::which_in`, so a `dir` that itself contains the OS PATH
/// separator (`:` on Unix, `;` on Windows) cannot be misinterpreted as
/// two directories. If `dir` cannot be expressed as a single PATH
/// entry, we return `None` rather than searching a phantom location.
///
/// Returns `None` if the command cannot be resolved inside the directory.
pub fn resolve_command_in(cmd: &str, dir: &Path) -> Option<PathBuf> {
    let paths = std::env::join_paths([dir]).ok()?;
    if let Ok(p) = which::which_in(cmd, Some(&paths), dir) {
        return Some(p);
    }
    windows_shim_fallback_in(cmd, dir)
}

/// Try `cmd` plus the common Windows shim suffixes (`.cmd`, `.ps1`, `.bat`)
/// inside a single directory. Used by `resolve_command_in` for callers that
/// want a directory-scoped lookup (the global `windows_shim_fallback` above
/// goes through `which::which`, which walks the entire `PATH`).
#[cfg(windows)]
fn windows_shim_fallback_in(cmd: &str, dir: &Path) -> Option<PathBuf> {
    if Path::new(cmd).extension().is_some() {
        return None;
    }
    for ext in ["cmd", "ps1", "bat"] {
        let candidate = dir.join(format!("{cmd}.{ext}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(windows))]
fn windows_shim_fallback_in(_cmd: &str, _dir: &Path) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn resolve_command_in_finds_executable_in_dir() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("widget");
        std::fs::write(&bin, b"#!/bin/sh\necho hi\n").unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();

        let found = resolve_command_in("widget", tmp.path()).expect("must find");
        assert_eq!(found, bin);
    }

    #[test]
    fn resolve_command_in_returns_none_for_missing_command() {
        let tmp = tempfile::TempDir::new().unwrap();
        let found = resolve_command_in("definitely-not-here", tmp.path());
        assert!(found.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_command_in_handles_dir_with_colon_safely() {
        // A path containing `:` is a separator-collision hazard for the
        // PATH string `which_in` consumes. We must NOT internally split
        // and search a wrong second segment — return None instead.
        let tmp = tempfile::TempDir::new().unwrap();
        let weird = tmp.path().join("with:colon");
        std::fs::create_dir(&weird).unwrap();
        // No `widget` file is created anywhere — the only way this could
        // return Some is if the function wrongly split `with:colon` and
        // found something in another segment.
        let found = resolve_command_in("widget", &weird);
        assert!(found.is_none(), "must not split on `:` inside dir; got {:?}", found);
    }

    #[cfg(windows)]
    #[test]
    fn resolve_command_in_falls_back_to_cmd_shim_on_windows() {
        // Simulate an npm-installed CLI: only `widget.cmd` exists, not `widget.exe`.
        let tmp = tempfile::TempDir::new().unwrap();
        let shim = tmp.path().join("widget.cmd");
        std::fs::write(&shim, b"@echo off\r\necho hi\r\n").unwrap();

        let found = resolve_command_in("widget", tmp.path()).expect("must find shim");
        assert!(
            found.to_string_lossy().to_lowercase().ends_with("widget.cmd"),
            "expected the .cmd shim; got {}",
            found.display()
        );
    }

    #[test]
    fn resolve_node_with_no_embed_returns_not_found_or_falls_back() {
        // No embed + no override → either NotFound, or which-resolves to a
        // host node. Both correct.
        unsafe {
            std::env::remove_var("AIONUI_NODE_PATH");
        }
        let fake = crate::embed::FakeEmbedNode {
            has: false,
            blob: b"",
            sha256: "",
            version: "",
            dir_name: "",
        };
        match resolve_node_with(&fake) {
            Ok(_) | Err(ResolveError::NotFound) => {}
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }

    #[test]
    fn resolve_node_env_override_wins_over_embed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        unsafe {
            std::env::set_var("AIONUI_NODE_PATH", &path);
        }
        let fake = crate::embed::FakeEmbedNode {
            has: true,
            blob: b"x",
            sha256: "0",
            version: "0",
            dir_name: "node-0-0",
        };
        let result = resolve_node_with(&fake).unwrap();
        assert_eq!(result, path);
        unsafe {
            std::env::remove_var("AIONUI_NODE_PATH");
        }
    }
}
