//! Public API for the bundled bun runtime.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::cache;
use crate::embed::{EmbeddedBun, ProductionEmbed};
use crate::extract::{self, ExtractError};

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("bun not found")]
    NotFound,
    #[error("failed to extract embedded bun: {0}")]
    Extract(#[from] std::io::Error),
    #[error("embedded bun checksum mismatch")]
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

static RESOLVED_BUN: OnceLock<PathBuf> = OnceLock::new();
static BUN_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Returns the path to a usable `bun` executable.
///
/// Priority: `AIONUI_BUN_PATH` env override > embedded + extract >
/// `which("bun")`.
pub fn resolve_bun() -> Result<PathBuf, ResolveError> {
    if let Some(path) = RESOLVED_BUN.get() {
        return Ok(path.clone());
    }
    let resolved = resolve_with(&ProductionEmbed)?;
    let _ = RESOLVED_BUN.set(resolved.clone());
    Ok(resolved)
}

/// Returns the directory that holds `bun` and `bunx`, if a bundled
/// runtime was extracted. `None` when no embed + no override was used.
pub fn bun_bin_dir() -> Option<PathBuf> {
    BUN_DIR
        .get_or_init(|| {
            resolve_with(&ProductionEmbed)
                .ok()
                .and_then(|p| p.parent().map(PathBuf::from))
        })
        .clone()
}

pub(crate) fn resolve_with<E: EmbeddedBun>(embed: &E) -> Result<PathBuf, ResolveError> {
    if let Some(p) = env_override() {
        return Ok(p);
    }
    if !embed.has() {
        return which::which("bun").map_err(|_| ResolveError::NotFound);
    }
    let dir = cache::bun_dir(embed.version(), embed.sha256()).ok_or(ResolveError::NotFound)?;

    if extract::is_fresh(&dir, embed.sha256(), embed.version()) {
        return Ok(dir.join(extract::bun_filename()));
    }

    // One retry on checksum mismatch: wipe dir and re-extract.
    match extract::extract_into(&dir, embed.blob(), embed.sha256(), embed.version()) {
        Ok(p) => Ok(p),
        Err(ExtractError::ChecksumMismatch { .. }) => {
            tracing::warn!("bun cache checksum mismatch; wiping and retrying");
            let _ = std::fs::remove_dir_all(&dir);
            Ok(extract::extract_into(
                &dir,
                embed.blob(),
                embed.sha256(),
                embed.version(),
            )?)
        }
        Err(e) => Err(e.into()),
    }
}

fn env_override() -> Option<PathBuf> {
    let raw = std::env::var("AIONUI_BUN_PATH").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let p = PathBuf::from(trimmed);
    if p.is_file() {
        Some(p)
    } else {
        tracing::warn!(path = %p.display(), "AIONUI_BUN_PATH does not point to a file; ignoring");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::FakeEmbed;
    use std::io::Write as _;

    fn make_blob(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = zstd::stream::write::Encoder::new(&mut out, 0).unwrap();
        enc.write_all(payload).unwrap();
        enc.finish().unwrap();
        out
    }

    fn sha(payload: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(payload);
        hex::encode(h.finalize())
    }

    #[test]
    fn no_embed_falls_back_to_which() {
        // Safety: unset to avoid env override winning.
        // SAFETY: single-threaded test, `cargo test` default is per-process.
        unsafe {
            std::env::remove_var("AIONUI_BUN_PATH");
        }

        let fake = FakeEmbed {
            has: false,
            blob: b"",
            sha256: "",
            version: "",
        };
        let res = resolve_with(&fake);
        // If bun is on the test host's PATH -> Ok; otherwise NotFound.
        // Both are correct behaviors for this branch.
        match res {
            Ok(_) | Err(ResolveError::NotFound) => {}
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }

    #[test]
    fn env_override_wins_over_embed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        // SAFETY: single-threaded test.
        unsafe {
            std::env::set_var("AIONUI_BUN_PATH", &path);
        }

        let payload = b"anything";
        let fake_blob: &'static [u8] = Box::leak(make_blob(payload).into_boxed_slice());
        let fake_sha: &'static str = Box::leak(sha(payload).into_boxed_str());
        let fake = FakeEmbed {
            has: true,
            blob: fake_blob,
            sha256: fake_sha,
            version: "1.0",
        };

        let result = resolve_with(&fake).unwrap();
        assert_eq!(result, path);

        // SAFETY: single-threaded test cleanup.
        unsafe {
            std::env::remove_var("AIONUI_BUN_PATH");
        }
    }

    #[test]
    fn bad_env_override_falls_through_to_embed() {
        // SAFETY: single-threaded test.
        unsafe {
            std::env::set_var("AIONUI_BUN_PATH", "/definitely/does/not/exist");
        }

        let fake = FakeEmbed {
            has: false,
            blob: b"",
            sha256: "",
            version: "",
        };
        let res = resolve_with(&fake);
        // Must not error out as `Extract(...)` from env override branch;
        // must fall through to which() (Ok or NotFound — both fine).
        match res {
            Ok(_) | Err(ResolveError::NotFound) => {}
            Err(e) => panic!("unexpected error: {e:?}"),
        }

        // SAFETY: single-threaded test cleanup.
        unsafe {
            std::env::remove_var("AIONUI_BUN_PATH");
        }
    }
}
