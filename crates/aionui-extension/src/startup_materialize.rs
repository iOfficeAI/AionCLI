//! Startup-time materialization of the embedded builtin skills corpus to
//! `{data_dir}/builtin-skills/`. Gated on a `.version` file so repeat
//! starts with the same binary skip the rewrite.
//!
//! Algorithm:
//!   staging = data_dir/.builtin-skills.tmp (fresh each call)
//!   write all BUILTIN_SKILLS entries into staging
//!   write staging/.version ← binary version
//!   atomic rename(target → .builtin-skills.old, staging → target)
//!   best-effort remove .builtin-skills.old
//!
//! The atomic rename guarantees that concurrent backend processes, or a
//! crash mid-write, never observe a half-populated target — the old tree
//! stays in place until staging is fully ready.

use std::path::{Path, PathBuf};

use include_dir::Dir;
use tracing::{info, warn};

use crate::error::ExtensionError;

const VERSION_FILE: &str = ".version";
const STAGING_DIR_NAME: &str = ".builtin-skills.tmp";
const OLD_DIR_NAME: &str = ".builtin-skills.old";

/// Decide whether to materialize based on the `.version` file, then do it.
/// Returns `true` if a write happened, `false` if the gate said "skip".
///
/// When `BUILTIN_SKILLS_ENV_VAR` is set and non-empty, the caller has
/// already routed `builtin_skills_dir` at the env-var path — this
/// function still runs but the gate will see whatever version the dev
/// tree has on disk (or missing, and materialize into that dev path,
/// which is wrong). Callers MUST check the env var before calling.
pub async fn materialize_if_needed(
    data_dir: &Path,
    corpus: &Dir<'static>,
    binary_version: &str,
) -> Result<bool, ExtensionError> {
    let target = data_dir.join(crate::constants::BUILTIN_SKILLS_DIR_NAME);

    if version_file_matches(&target, binary_version).await {
        info!(
            target = %target.display(),
            version = binary_version,
            "builtin skills up to date; skipping materialize"
        );
        return Ok(false);
    }

    info!(
        target = %target.display(),
        version = binary_version,
        "materializing embedded builtin skills"
    );
    materialize_embedded_builtin_skills(data_dir, corpus, binary_version).await?;
    Ok(true)
}

/// Read `.version` and compare against the provided `binary_version`.
/// Returns `true` only on exact match. Missing file / IO error /
/// mismatch all return `false`.
async fn version_file_matches(target: &Path, binary_version: &str) -> bool {
    let version_path = target.join(VERSION_FILE);
    match tokio::fs::read_to_string(&version_path).await {
        Ok(s) => s == binary_version,
        Err(_) => false,
    }
}

/// Unconditional materialize: stage, write each file, atomic rename.
/// Exposed separately for tests that want to bypass the gate.
pub async fn materialize_embedded_builtin_skills(
    data_dir: &Path,
    corpus: &Dir<'static>,
    binary_version: &str,
) -> Result<(), ExtensionError> {
    let target = data_dir.join(crate::constants::BUILTIN_SKILLS_DIR_NAME);
    let staging = data_dir.join(STAGING_DIR_NAME);
    let old = data_dir.join(OLD_DIR_NAME);

    // Ensure data_dir itself exists before we try to write into it.
    tokio::fs::create_dir_all(data_dir).await?;

    // Clean any leftover staging from a previous crashed run.
    if staging.exists() {
        tokio::fs::remove_dir_all(&staging).await.map_err(|e| {
            ExtensionError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to clean staging dir {}: {e}", staging.display()),
            ))
        })?;
    }
    tokio::fs::create_dir_all(&staging).await?;

    write_dir_recursive(corpus, &staging).await?;

    let version_path = staging.join(VERSION_FILE);
    tokio::fs::write(&version_path, binary_version).await?;

    // Move existing target out of the way, then move staging in.
    if target.exists() {
        if old.exists() {
            // Tolerate leftover .old from a crashed rename sequence.
            let _ = tokio::fs::remove_dir_all(&old).await;
        }
        tokio::fs::rename(&target, &old).await?;
    }

    if let Err(e) = tokio::fs::rename(&staging, &target).await {
        // Try to restore the original target so we don't leave the user
        // with no builtin skills.
        if old.exists() {
            let _ = tokio::fs::rename(&old, &target).await;
        }
        return Err(ExtensionError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "atomic rename staging→target failed ({} → {}): {e}",
                staging.display(),
                target.display()
            ),
        )));
    }

    // Best-effort cleanup of the superseded tree.
    if old.exists()
        && let Err(e) = tokio::fs::remove_dir_all(&old).await
    {
        warn!(
            old = %old.display(),
            error = %e,
            "failed to remove superseded builtin skills tree (leaving behind)"
        );
    }

    Ok(())
}

/// Recursively copy every file in an `include_dir::Dir` tree into `dest`.
/// Directories are created as needed. Files overwrite silently.
async fn write_dir_recursive(dir: &Dir<'static>, dest: &Path) -> Result<(), ExtensionError> {
    // The include_dir API is synchronous; we flatten into a Vec then
    // feed the writes through tokio::fs to stay off the reactor's thread
    // for big IO bursts.
    let mut stack: Vec<(&Dir<'static>, PathBuf)> = vec![(dir, dest.to_path_buf())];
    while let Some((d, prefix)) = stack.pop() {
        for file in d.files() {
            let rel = file.path();
            let out_path = prefix.join(rel.strip_prefix(d.path()).unwrap_or(rel));
            if let Some(parent) = out_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&out_path, file.contents()).await?;
        }
        for sub in d.dirs() {
            let sub_rel = sub.path();
            let sub_dest = prefix.join(sub_rel.strip_prefix(d.path()).unwrap_or(sub_rel));
            tokio::fs::create_dir_all(&sub_dest).await?;
            stack.push((sub, sub_dest));
        }
    }
    Ok(())
}
