//! Atomic extraction of the compressed embedded node blob to the cache dir.
//!
//! Flow:
//! 1. Acquire inter-process advisory file lock (so parallel starts don't race).
//! 2. Re-check stamp: another process may have finished while we waited.
//! 3. Wipe any stale `<dir>.tmp/` from a crashed previous run.
//! 4. Streaming zstd → tar → unpack into `<dir>.tmp/`.
//! 5. Sanity-check `bin/node[.exe]` + `npm-cli.js` exist inside `.tmp/`.
//! 6. Atomic dir rename `<dir>.tmp/` → `<dir>/`.
//! 7. Write `node.stamp`.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("serde_json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Stamp file written next to the extracted node directory.
#[derive(Debug, Serialize, Deserialize)]
pub struct NodeStamp {
    pub sha256: String,
    pub version: String,
    pub extracted_at: String,
}

/// Returns true when `<dir>/node.stamp` matches `expected_sha` + `expected_version`
/// AND the `bin/node[.exe]` + `lib/node_modules/npm/bin/npm-cli.js` invariants hold.
pub fn is_node_fresh(dir: &Path, expected_sha: &str, expected_version: &str) -> bool {
    let node_bin = dir.join(if cfg!(windows) { "bin/node.exe" } else { "bin/node" });
    let npm_cli = dir.join("lib/node_modules/npm/bin/npm-cli.js");
    if !node_bin.is_file() || !npm_cli.is_file() {
        return false;
    }
    let stamp_path = dir.join("node.stamp");
    let Ok(bytes) = fs::read(&stamp_path) else {
        return false;
    };
    let Ok(stamp): Result<NodeStamp, _> = serde_json::from_slice(&bytes) else {
        return false;
    };
    stamp.sha256 == expected_sha && stamp.version == expected_version
}

/// Extract a zstd+tar `blob` (the bundled node directory contents) into
/// `dir`. Idempotent and crash-recoverable.
///
/// Pipeline:
/// 1. Acquire advisory lock on `<dir>/../runtime.lock`.
/// 2. Re-check `is_node_fresh` after lock.
/// 3. Wipe any stale `<dir>.tmp/` from a crashed previous run.
/// 4. Streaming zstd → tar → unpack into `<dir>.tmp/`.
/// 5. Sanity-check `bin/node[.exe]` + `npm-cli.js` exist inside `.tmp/`.
/// 6. Atomic dir rename `<dir>.tmp/` → `<dir>/`.
/// 7. Write `node.stamp`.
pub fn extract_node_into(dir: &Path, blob: &[u8], expected_sha: &str, version: &str) -> Result<(), ExtractError> {
    let parent = dir.parent().ok_or_else(|| {
        ExtractError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "dir has no parent",
        ))
    })?;
    fs::create_dir_all(parent)?;

    let lock_path = parent.join("runtime.lock");
    let lock_file = File::create(&lock_path)?;
    lock_file.lock_exclusive()?;

    let result = (|| -> Result<(), ExtractError> {
        if is_node_fresh(dir, expected_sha, version) {
            return Ok(());
        }

        let tmp_dir = parent.join(format!(
            "{}.tmp",
            dir.file_name().and_then(|n| n.to_str()).unwrap_or("node")
        ));
        if tmp_dir.exists() {
            fs::remove_dir_all(&tmp_dir)?;
        }
        fs::create_dir_all(&tmp_dir)?;

        let decoder = zstd::stream::read::Decoder::new(std::io::Cursor::new(blob))?;
        let mut archive = tar::Archive::new(decoder);
        archive.set_preserve_permissions(true);
        archive.unpack(&tmp_dir)?;

        // Sanity check.
        let node_bin = tmp_dir.join(if cfg!(windows) { "bin/node.exe" } else { "bin/node" });
        let npm_cli = tmp_dir.join("lib/node_modules/npm/bin/npm-cli.js");
        if !node_bin.is_file() || !npm_cli.is_file() {
            let _ = fs::remove_dir_all(&tmp_dir);
            return Err(ExtractError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "node tarball missing bin/node or npm-cli.js",
            )));
        }

        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        fs::rename(&tmp_dir, dir)?;

        let stamp = NodeStamp {
            sha256: expected_sha.into(),
            version: version.into(),
            extracted_at: chrono_utc_now(),
        };
        let stamp_bytes = serde_json::to_vec_pretty(&stamp)?;
        let stamp_tmp = dir.join("node.stamp.tmp");
        {
            let mut f = File::create(&stamp_tmp)?;
            f.write_all(&stamp_bytes)?;
            f.sync_all()?;
        }
        fs::rename(&stamp_tmp, dir.join("node.stamp"))?;
        Ok(())
    })();

    let _ = FileExt::unlock(&lock_file);
    result
}

/// A cheap RFC3339-ish timestamp that avoids pulling chrono into this crate.
pub(crate) fn chrono_utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch-{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node_tar_zstd_blob() -> Vec<u8> {
        // Build a minimal in-memory tar containing the two files
        // `extract_node_into` checks for, then zstd-compress.
        let mut tar_bytes = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_bytes);
            // bin/node
            let mut header = tar::Header::new_gnu();
            let payload = b"#!/bin/sh\necho fake-node\n";
            header.set_path("bin/node").unwrap();
            header.set_size(payload.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            b.append(&header, &payload[..]).unwrap();
            // lib/node_modules/npm/bin/npm-cli.js
            let mut header = tar::Header::new_gnu();
            let payload = b"#!/usr/bin/env node\n";
            header.set_path("lib/node_modules/npm/bin/npm-cli.js").unwrap();
            header.set_size(payload.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            b.append(&header, &payload[..]).unwrap();
            b.finish().unwrap();
        }
        let mut out = Vec::new();
        let mut enc = zstd::stream::write::Encoder::new(&mut out, 0).unwrap();
        std::io::copy(&mut tar_bytes.as_slice(), &mut enc).unwrap();
        enc.finish().unwrap();
        out
    }

    #[test]
    fn extract_node_creates_bin_node_and_npm_cli_js() {
        let blob = make_node_tar_zstd_blob();
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("node-22.11.0-aaaaaaaaaaaa");

        extract_node_into(&dir, &blob, "fakesha", "22.11.0").unwrap();

        let node_bin = if cfg!(windows) {
            dir.join("bin").join("node.exe")
        } else {
            dir.join("bin").join("node")
        };
        let node_path_unix = dir.join("bin/node");
        assert!(
            node_path_unix.is_file() || node_bin.is_file(),
            "bin/node must exist at {} or {}",
            node_path_unix.display(),
            node_bin.display()
        );
        assert!(
            dir.join("lib/node_modules/npm/bin/npm-cli.js").is_file(),
            "npm-cli.js must exist"
        );
        assert!(dir.join("node.stamp").is_file(), "stamp must exist");
    }

    #[test]
    fn extract_node_is_idempotent_via_stamp() {
        let blob = make_node_tar_zstd_blob();
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("node-22.11.0-bbbbbbbbbbbb");

        extract_node_into(&dir, &blob, "sha-x", "22.11.0").unwrap();
        assert!(is_node_fresh(&dir, "sha-x", "22.11.0"));

        let sentinel = dir.join("sentinel");
        fs::write(&sentinel, b"keep").unwrap();

        extract_node_into(&dir, &blob, "sha-x", "22.11.0").unwrap();
        assert!(sentinel.is_file(), "second call must not re-extract");
    }

    #[test]
    fn extract_node_recovers_from_stale_tmp_dir() {
        let blob = make_node_tar_zstd_blob();
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("node-22.11.0-cccccccccccc");
        let stale_tmp = tmp.path().join("node-22.11.0-cccccccccccc.tmp");
        fs::create_dir_all(&stale_tmp).unwrap();
        fs::write(stale_tmp.join("garbage"), b"junk").unwrap();

        extract_node_into(&dir, &blob, "sha-y", "22.11.0").unwrap();

        assert!(!stale_tmp.exists(), ".tmp must be cleaned up");
        assert!(dir.join("bin/node").is_file());
    }
}
