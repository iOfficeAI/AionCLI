use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use aionui_common::{AppError, FileChangeOperation};
use dashmap::DashMap;
use git2::{
    IndexAddOption, Repository, Signature, Status, StatusOptions,
};
use tracing;

use crate::types::{CompareResult, FileChangeInfo, SnapshotInfo, SnapshotMode};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Prefix for temporary snapshot directories under the system temp dir.
const SNAPSHOT_DIR_PREFIX: &str = "aionui-snapshot-";

/// Exclude rules written to `<git-dir>/info/exclude` for snapshot mode.
/// These patterns prevent large/generated directories from being tracked.
const SNAPSHOT_EXCLUDE_RULES: &str = "\
node_modules/
dist/
build/
target/
.venv/
__pycache__/
.DS_Store
Thumbs.db
*.pyc
.env
.env.local
.next/
.nuxt/
.output/
";

/// Signature name used for snapshot commits.
const SNAPSHOT_SIG_NAME: &str = "aionui";
/// Signature email used for snapshot commits.
const SNAPSHOT_SIG_EMAIL: &str = "snapshot@aionui.local";
/// Commit message for the initial snapshot baseline.
const SNAPSHOT_INITIAL_MSG: &str = "Initial snapshot";

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

/// Tracked state for an initialized workspace.
#[derive(Clone, Debug)]
struct WorkspaceState {
    mode: SnapshotMode,
    /// Path to the git directory.
    /// - git-repo mode: the workspace path itself (contains `.git/`).
    /// - snapshot mode: `/tmp/aionui-snapshot-{hash}` (bare-style git dir).
    repo_path: PathBuf,
    /// Canonical path to the actual workspace directory.
    workspace_path: PathBuf,
}

// ---------------------------------------------------------------------------
// SnapshotService
// ---------------------------------------------------------------------------

/// Git-based workspace snapshot service.
///
/// Supports two modes:
/// - **git-repo**: directory already has `.git` — uses it directly.
/// - **snapshot**: no `.git` — creates a temporary git repo that tracks the
///   workspace via a separate worktree.
pub struct SnapshotService {
    workspaces: DashMap<String, WorkspaceState>,
}

impl Default for SnapshotService {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotService {
    pub fn new() -> Self {
        Self {
            workspaces: DashMap::new(),
        }
    }

    /// Remove leftover `aionui-snapshot-*` directories from the system temp
    /// dir. Call once at application startup.
    pub fn cleanup_stale_snapshots() {
        let temp_dir = std::env::temp_dir();
        let entries = match std::fs::read_dir(&temp_dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to read temp dir for snapshot cleanup"
                );
                return;
            }
        };
        for entry in entries.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            if name.starts_with(SNAPSHOT_DIR_PREFIX) {
                let path = entry.path();
                if let Err(e) = std::fs::remove_dir_all(&path) {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "Failed to clean up stale snapshot directory"
                    );
                } else {
                    tracing::info!(
                        path = %path.display(),
                        "Cleaned up stale snapshot directory"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pure helper functions (no &self — usable inside spawn_blocking)
// ---------------------------------------------------------------------------

/// Compute a deterministic temp directory path for a workspace.
fn temp_repo_path(workspace: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    workspace.hash(&mut hasher);
    let hash = hasher.finish();
    std::env::temp_dir()
        .join(format!("{}{:016x}", SNAPSHOT_DIR_PREFIX, hash))
}

/// Open the git repository for a workspace state.
fn open_repo(state: &WorkspaceState) -> Result<Repository, AppError> {
    Repository::open(&state.repo_path).map_err(|e| {
        AppError::Internal(format!(
            "Failed to open git repo at {}: {}",
            state.repo_path.display(),
            e
        ))
    })
}

/// Initialize a snapshot-mode temp repository for a non-git workspace.
///
/// 1. Creates the temp directory with a standard `.git` layout.
/// 2. Sets `core.worktree` to point at the real workspace.
/// 3. Writes exclude rules to `.git/info/exclude`.
/// 4. Adds all workspace files and creates an initial commit as the baseline.
fn init_snapshot_repo(
    workspace: &Path,
    temp_dir: &Path,
) -> Result<(), AppError> {
    // Clean up any leftover directory from a previous run with the same hash
    if temp_dir.exists() {
        std::fs::remove_dir_all(temp_dir).map_err(|e| {
            AppError::Internal(format!(
                "Failed to clean up existing snapshot dir {}: {}",
                temp_dir.display(),
                e
            ))
        })?;
    }
    std::fs::create_dir_all(temp_dir).map_err(|e| {
        AppError::Internal(format!(
            "Failed to create snapshot dir {}: {}",
            temp_dir.display(),
            e
        ))
    })?;

    // Init a standard repo (creates .git/ inside temp_dir)
    let repo = Repository::init(temp_dir).map_err(|e| {
        AppError::Internal(format!(
            "Failed to init snapshot repo at {}: {}",
            temp_dir.display(),
            e
        ))
    })?;

    // Set workdir to the actual workspace (in-memory)
    repo.set_workdir(workspace, false).map_err(|e| {
        AppError::Internal(format!(
            "Failed to set workdir to {}: {}",
            workspace.display(),
            e
        ))
    })?;

    // Persist core.worktree in config so future opens resolve the workdir
    let mut config = repo.config().map_err(|e| {
        AppError::Internal(format!("Failed to open repo config: {}", e))
    })?;
    let ws_str = workspace.to_string_lossy();
    config.set_str("core.worktree", &ws_str).map_err(|e| {
        AppError::Internal(format!(
            "Failed to set core.worktree to {}: {}",
            ws_str, e
        ))
    })?;

    // Write exclude rules to .git/info/exclude (avoids polluting the workspace)
    let git_dir = repo.path(); // .git/ directory
    let info_dir = git_dir.join("info");
    std::fs::create_dir_all(&info_dir).map_err(|e| {
        AppError::Internal(format!(
            "Failed to create info dir {}: {}",
            info_dir.display(),
            e
        ))
    })?;
    std::fs::write(info_dir.join("exclude"), SNAPSHOT_EXCLUDE_RULES)
        .map_err(|e| {
            AppError::Internal(format!(
                "Failed to write exclude rules: {}",
                e
            ))
        })?;

    // Stage all workspace files
    let mut index = repo.index().map_err(|e| {
        AppError::Internal(format!("Failed to get index: {}", e))
    })?;
    index
        .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
        .map_err(|e| {
            AppError::Internal(format!(
                "Failed to add files to index: {}",
                e
            ))
        })?;
    index.write().map_err(|e| {
        AppError::Internal(format!("Failed to write index: {}", e))
    })?;

    // Create initial commit
    let tree_oid = index.write_tree().map_err(|e| {
        AppError::Internal(format!("Failed to write tree: {}", e))
    })?;
    let tree = repo.find_tree(tree_oid).map_err(|e| {
        AppError::Internal(format!("Failed to find tree: {}", e))
    })?;
    let sig =
        Signature::now(SNAPSHOT_SIG_NAME, SNAPSHOT_SIG_EMAIL).map_err(|e| {
            AppError::Internal(format!(
                "Failed to create signature: {}",
                e
            ))
        })?;
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        SNAPSHOT_INITIAL_MSG,
        &tree,
        &[],
    )
    .map_err(|e| {
        AppError::Internal(format!("Failed to create initial commit: {}", e))
    })?;

    Ok(())
}

/// Get the current branch name from a repository.
/// Returns `None` if HEAD is detached or the repo has no commits.
fn current_branch(repo: &Repository) -> Option<String> {
    repo.head()
        .ok()
        .and_then(|head| head.shorthand().map(String::from))
}

/// Build a `SnapshotInfo` from mode and repository.
fn build_info(
    mode: SnapshotMode,
    repo: &Repository,
) -> SnapshotInfo {
    let branch = match mode {
        SnapshotMode::GitRepo => current_branch(repo),
        SnapshotMode::Snapshot => None,
    };
    SnapshotInfo { mode, branch }
}

/// Map git2 status flags to `FileChangeOperation`.
fn index_operation(status: Status) -> Option<FileChangeOperation> {
    if status.intersects(Status::INDEX_NEW) {
        Some(FileChangeOperation::Create)
    } else if status.intersects(Status::INDEX_MODIFIED) {
        Some(FileChangeOperation::Modify)
    } else if status.intersects(Status::INDEX_DELETED) {
        Some(FileChangeOperation::Delete)
    } else {
        None
    }
}

/// Map git2 working-tree status flags to `FileChangeOperation`.
fn worktree_operation(status: Status) -> Option<FileChangeOperation> {
    if status.intersects(Status::WT_NEW) {
        Some(FileChangeOperation::Create)
    } else if status.intersects(Status::WT_MODIFIED) {
        Some(FileChangeOperation::Modify)
    } else if status.intersects(Status::WT_DELETED) {
        Some(FileChangeOperation::Delete)
    } else {
        None
    }
}

/// Parse git2 statuses into staged and unstaged change lists.
fn parse_statuses(
    repo: &Repository,
    workspace: &Path,
) -> Result<CompareResult, AppError> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);

    let statuses = repo.statuses(Some(&mut opts)).map_err(|e| {
        AppError::Internal(format!("Failed to get git status: {}", e))
    })?;

    let ws_str = workspace.to_string_lossy();
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();

    for entry in statuses.iter() {
        let status = entry.status();
        let rel_path = match entry.path() {
            Some(p) => p.to_string(),
            None => continue,
        };
        let full_path = format!(
            "{}/{}",
            ws_str.trim_end_matches('/'),
            &rel_path
        );

        if let Some(op) = index_operation(status) {
            staged.push(FileChangeInfo {
                file_path: full_path.clone(),
                relative_path: rel_path.clone(),
                operation: op,
            });
        }
        if let Some(op) = worktree_operation(status) {
            unstaged.push(FileChangeInfo {
                file_path: full_path,
                relative_path: rel_path,
                operation: op,
            });
        }
    }

    Ok(CompareResult { staged, unstaged })
}

/// Read a file's content from HEAD.
/// Returns `None` if the file is not tracked or the repo has no commits.
fn read_baseline(
    repo: &Repository,
    rel_path: &str,
) -> Result<Option<String>, AppError> {
    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => return Ok(None),
    };
    let commit = head.peel_to_commit().map_err(|e| {
        AppError::Internal(format!("Failed to peel HEAD to commit: {}", e))
    })?;
    let tree = commit.tree().map_err(|e| {
        AppError::Internal(format!("Failed to get commit tree: {}", e))
    })?;

    let entry = match tree.get_path(Path::new(rel_path)) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };

    let blob = repo.find_blob(entry.id()).map_err(|e| {
        AppError::Internal(format!("Failed to read blob: {}", e))
    })?;

    match std::str::from_utf8(blob.content()) {
        Ok(s) => Ok(Some(s.to_string())),
        Err(_) => Ok(None), // Binary file — no text baseline
    }
}

/// Canonicalize a workspace path and validate it exists.
fn resolve_workspace(workspace: &str) -> Result<PathBuf, AppError> {
    let path = Path::new(workspace);
    if !path.exists() {
        return Err(AppError::NotFound(format!(
            "Workspace not found: {}",
            workspace
        )));
    }
    std::fs::canonicalize(path).map_err(|e| {
        AppError::Internal(format!(
            "Failed to canonicalize workspace path {}: {}",
            workspace, e
        ))
    })
}

// ---------------------------------------------------------------------------
// ISnapshotService implementation
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl crate::traits::ISnapshotService for SnapshotService {
    async fn init(&self, workspace: &str) -> Result<SnapshotInfo, AppError> {
        let ws = workspace.to_owned();

        // Check if already initialized
        if let Some(state) = self.workspaces.get(&ws) {
            let st = state.clone();
            return tokio::task::spawn_blocking(move || {
                let repo = open_repo(&st)?;
                Ok(build_info(st.mode, &repo))
            })
            .await
            .map_err(|e| {
                AppError::Internal(format!("Blocking task failed: {}", e))
            })?;
        }

        let ws_clone = ws.clone();
        let result = tokio::task::spawn_blocking(move || {
            let canonical = resolve_workspace(&ws_clone)?;
            let canonical_str =
                canonical.to_string_lossy().to_string();

            let git_dir = canonical.join(".git");
            let (mode, repo_path) = if git_dir.exists() {
                // git-repo mode
                (SnapshotMode::GitRepo, canonical.clone())
            } else {
                // snapshot mode
                let temp = temp_repo_path(&canonical_str);
                init_snapshot_repo(&canonical, &temp)?;
                (SnapshotMode::Snapshot, temp)
            };

            let state = WorkspaceState {
                mode,
                repo_path: repo_path.clone(),
                workspace_path: canonical,
            };

            let repo = Repository::open(&repo_path).map_err(|e| {
                AppError::Internal(format!(
                    "Failed to open repo after init: {}",
                    e
                ))
            })?;
            let info = build_info(mode, &repo);

            Ok::<(WorkspaceState, SnapshotInfo), AppError>((state, info))
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!("Blocking task failed: {}", e))
        })??;

        let (state, info) = result;
        self.workspaces.insert(ws, state);
        Ok(info)
    }

    async fn get_info(
        &self,
        workspace: &str,
    ) -> Result<SnapshotInfo, AppError> {
        let state = self
            .workspaces
            .get(workspace)
            .map(|r| r.clone())
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Workspace not initialized: {}",
                    workspace
                ))
            })?;

        tokio::task::spawn_blocking(move || {
            let repo = open_repo(&state)?;
            Ok(build_info(state.mode, &repo))
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!("Blocking task failed: {}", e))
        })?
    }

    async fn compare(
        &self,
        workspace: &str,
    ) -> Result<CompareResult, AppError> {
        let state = self
            .workspaces
            .get(workspace)
            .map(|r| r.clone())
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Workspace not initialized: {}",
                    workspace
                ))
            })?;

        tokio::task::spawn_blocking(move || {
            let repo = open_repo(&state)?;
            parse_statuses(&repo, &state.workspace_path)
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!("Blocking task failed: {}", e))
        })?
    }

    async fn get_baseline_content(
        &self,
        workspace: &str,
        file_path: &str,
    ) -> Result<Option<String>, AppError> {
        let state = self
            .workspaces
            .get(workspace)
            .map(|r| r.clone())
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Workspace not initialized: {}",
                    workspace
                ))
            })?;

        let rel = file_path.to_owned();
        tokio::task::spawn_blocking(move || {
            let repo = open_repo(&state)?;
            read_baseline(&repo, &rel)
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!("Blocking task failed: {}", e))
        })?
    }

    // -----------------------------------------------------------------------
    // The following methods will be fully implemented in task 7.10.
    // For now they return appropriate errors to satisfy the trait.
    // -----------------------------------------------------------------------

    async fn stage_file(
        &self,
        workspace: &str,
        file_path: &str,
    ) -> Result<(), AppError> {
        let state = self
            .workspaces
            .get(workspace)
            .map(|r| r.clone())
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Workspace not initialized: {}",
                    workspace
                ))
            })?;

        let fp = file_path.to_owned();
        tokio::task::spawn_blocking(move || {
            let repo = open_repo(&state)?;
            let mut index = repo.index().map_err(|e| {
                AppError::Internal(format!(
                    "Failed to get index: {}",
                    e
                ))
            })?;
            index
                .add_path(Path::new(&fp))
                .map_err(|e| {
                    AppError::Internal(format!(
                        "Failed to stage file {}: {}",
                        fp, e
                    ))
                })?;
            index.write().map_err(|e| {
                AppError::Internal(format!(
                    "Failed to write index: {}",
                    e
                ))
            })?;
            Ok(())
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!("Blocking task failed: {}", e))
        })?
    }

    async fn stage_all(&self, workspace: &str) -> Result<(), AppError> {
        let state = self
            .workspaces
            .get(workspace)
            .map(|r| r.clone())
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Workspace not initialized: {}",
                    workspace
                ))
            })?;

        tokio::task::spawn_blocking(move || {
            let repo = open_repo(&state)?;
            let mut index = repo.index().map_err(|e| {
                AppError::Internal(format!(
                    "Failed to get index: {}",
                    e
                ))
            })?;
            index
                .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
                .map_err(|e| {
                    AppError::Internal(format!(
                        "Failed to stage all files: {}",
                        e
                    ))
                })?;
            index.write().map_err(|e| {
                AppError::Internal(format!(
                    "Failed to write index: {}",
                    e
                ))
            })?;
            Ok(())
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!("Blocking task failed: {}", e))
        })?
    }

    async fn unstage_file(
        &self,
        workspace: &str,
        file_path: &str,
    ) -> Result<(), AppError> {
        let state = self
            .workspaces
            .get(workspace)
            .map(|r| r.clone())
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Workspace not initialized: {}",
                    workspace
                ))
            })?;

        let fp = file_path.to_owned();
        tokio::task::spawn_blocking(move || {
            let repo = open_repo(&state)?;
            let head = repo.head().map_err(|e| {
                AppError::Internal(format!(
                    "Failed to get HEAD: {}",
                    e
                ))
            })?;
            let commit = head.peel_to_commit().map_err(|e| {
                AppError::Internal(format!(
                    "Failed to peel HEAD: {}",
                    e
                ))
            })?;
            let tree = commit.tree().map_err(|e| {
                AppError::Internal(format!(
                    "Failed to get tree: {}",
                    e
                ))
            })?;
            repo.reset_default(Some(tree.as_object()), [fp.as_str()])
                .map_err(|e| {
                    AppError::Internal(format!(
                        "Failed to unstage file {}: {}",
                        fp, e
                    ))
                })?;
            Ok(())
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!("Blocking task failed: {}", e))
        })?
    }

    async fn unstage_all(&self, workspace: &str) -> Result<(), AppError> {
        let state = self
            .workspaces
            .get(workspace)
            .map(|r| r.clone())
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Workspace not initialized: {}",
                    workspace
                ))
            })?;

        tokio::task::spawn_blocking(move || {
            let repo = open_repo(&state)?;
            let head = repo.head().map_err(|e| {
                AppError::Internal(format!(
                    "Failed to get HEAD: {}",
                    e
                ))
            })?;
            let commit = head.peel_to_commit().map_err(|e| {
                AppError::Internal(format!(
                    "Failed to peel HEAD: {}",
                    e
                ))
            })?;
            repo.reset(
                commit.as_object(),
                git2::ResetType::Mixed,
                None,
            )
            .map_err(|e| {
                AppError::Internal(format!(
                    "Failed to unstage all: {}",
                    e
                ))
            })?;
            Ok(())
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!("Blocking task failed: {}", e))
        })?
    }

    async fn discard_file(
        &self,
        _workspace: &str,
        _file_path: &str,
        _operation: FileChangeOperation,
    ) -> Result<(), AppError> {
        Err(AppError::Internal(
            "discard_file not yet implemented (planned for 7.10)".into(),
        ))
    }

    async fn reset_file(
        &self,
        _workspace: &str,
        _file_path: &str,
        _operation: FileChangeOperation,
    ) -> Result<(), AppError> {
        Err(AppError::Internal(
            "reset_file not yet implemented (planned for 7.10)".into(),
        ))
    }

    async fn get_branches(
        &self,
        _workspace: &str,
    ) -> Result<Vec<String>, AppError> {
        Err(AppError::Internal(
            "get_branches not yet implemented (planned for 7.10)".into(),
        ))
    }

    async fn dispose(&self, _workspace: &str) -> Result<(), AppError> {
        Err(AppError::Internal(
            "dispose not yet implemented (planned for 7.10)".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- temp_repo_path --

    #[test]
    fn temp_repo_path_deterministic() {
        let a = temp_repo_path("/home/user/project");
        let b = temp_repo_path("/home/user/project");
        assert_eq!(a, b);
    }

    #[test]
    fn temp_repo_path_different_for_different_workspaces() {
        let a = temp_repo_path("/home/user/project-a");
        let b = temp_repo_path("/home/user/project-b");
        assert_ne!(a, b);
    }

    #[test]
    fn temp_repo_path_has_prefix() {
        let p = temp_repo_path("/ws");
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with(SNAPSHOT_DIR_PREFIX));
    }

    // -- index_operation / worktree_operation --

    #[test]
    fn index_operation_new() {
        assert_eq!(
            index_operation(Status::INDEX_NEW),
            Some(FileChangeOperation::Create)
        );
    }

    #[test]
    fn index_operation_modified() {
        assert_eq!(
            index_operation(Status::INDEX_MODIFIED),
            Some(FileChangeOperation::Modify)
        );
    }

    #[test]
    fn index_operation_deleted() {
        assert_eq!(
            index_operation(Status::INDEX_DELETED),
            Some(FileChangeOperation::Delete)
        );
    }

    #[test]
    fn index_operation_none_for_wt() {
        assert_eq!(index_operation(Status::WT_NEW), None);
    }

    #[test]
    fn worktree_operation_new() {
        assert_eq!(
            worktree_operation(Status::WT_NEW),
            Some(FileChangeOperation::Create)
        );
    }

    #[test]
    fn worktree_operation_modified() {
        assert_eq!(
            worktree_operation(Status::WT_MODIFIED),
            Some(FileChangeOperation::Modify)
        );
    }

    #[test]
    fn worktree_operation_deleted() {
        assert_eq!(
            worktree_operation(Status::WT_DELETED),
            Some(FileChangeOperation::Delete)
        );
    }

    #[test]
    fn worktree_operation_none_for_index() {
        assert_eq!(worktree_operation(Status::INDEX_NEW), None);
    }

    // -- resolve_workspace --

    #[test]
    fn resolve_workspace_not_found() {
        let err =
            resolve_workspace("/nonexistent/path/xyz123").unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[test]
    fn resolve_workspace_success() {
        let tmp = std::env::temp_dir();
        let result = resolve_workspace(tmp.to_str().unwrap());
        assert!(result.is_ok());
    }

    // -- current_branch --

    #[test]
    fn current_branch_of_fresh_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        // Fresh repo with no commits — HEAD is unborn
        assert!(current_branch(&repo).is_none());

        // Create an initial commit so HEAD points to a branch
        let mut index = repo.index().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let branch = current_branch(&repo);
        assert!(branch.is_some());
        // Default branch varies by git config, but it should be non-empty
        assert!(!branch.unwrap().is_empty());
    }

    // -- build_info --

    #[test]
    fn build_info_git_repo_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        // Create a commit on "main"
        let mut index = repo.index().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let info = build_info(SnapshotMode::GitRepo, &repo);
        assert_eq!(info.mode, SnapshotMode::GitRepo);
        assert!(info.branch.is_some());
    }

    #[test]
    fn build_info_snapshot_mode_returns_no_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        // Even with a commit, snapshot mode should report no branch
        let mut index = repo.index().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let info = build_info(SnapshotMode::Snapshot, &repo);
        assert_eq!(info.mode, SnapshotMode::Snapshot);
        assert!(info.branch.is_none());
    }

    // -- read_baseline --

    #[test]
    fn read_baseline_no_commits() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();
        let result = read_baseline(&repo, "any.txt").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_baseline_tracked_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        // Write a file and commit it
        std::fs::write(tmp.path().join("hello.txt"), "Hello, world!")
            .unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("hello.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "add hello", &tree, &[])
            .unwrap();

        let content = read_baseline(&repo, "hello.txt").unwrap();
        assert_eq!(content.as_deref(), Some("Hello, world!"));
    }

    #[test]
    fn read_baseline_untracked_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        // Create empty commit (no files tracked)
        let mut index = repo.index().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let content = read_baseline(&repo, "missing.txt").unwrap();
        assert!(content.is_none());
    }

    // -- parse_statuses --

    #[test]
    fn parse_statuses_clean_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        // Create a file, commit it, no changes after
        std::fs::write(tmp.path().join("a.txt"), "content").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let result = parse_statuses(&repo, tmp.path()).unwrap();
        assert!(result.staged.is_empty());
        assert!(result.unstaged.is_empty());
    }

    #[test]
    fn parse_statuses_new_untracked_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        // Initial commit with one file
        std::fs::write(tmp.path().join("a.txt"), "a").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // Add a new untracked file
        std::fs::write(tmp.path().join("b.txt"), "b").unwrap();

        let result = parse_statuses(&repo, tmp.path()).unwrap();
        assert!(result.staged.is_empty());
        assert_eq!(result.unstaged.len(), 1);
        assert_eq!(result.unstaged[0].relative_path, "b.txt");
        assert_eq!(
            result.unstaged[0].operation,
            FileChangeOperation::Create
        );
    }

    #[test]
    fn parse_statuses_modified_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        std::fs::write(tmp.path().join("a.txt"), "original").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // Modify the file
        std::fs::write(tmp.path().join("a.txt"), "modified").unwrap();

        let result = parse_statuses(&repo, tmp.path()).unwrap();
        assert!(result.staged.is_empty());
        assert_eq!(result.unstaged.len(), 1);
        assert_eq!(
            result.unstaged[0].operation,
            FileChangeOperation::Modify
        );
    }

    #[test]
    fn parse_statuses_deleted_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        std::fs::write(tmp.path().join("a.txt"), "content").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // Delete the file
        std::fs::remove_file(tmp.path().join("a.txt")).unwrap();

        let result = parse_statuses(&repo, tmp.path()).unwrap();
        assert!(result.staged.is_empty());
        assert_eq!(result.unstaged.len(), 1);
        assert_eq!(
            result.unstaged[0].operation,
            FileChangeOperation::Delete
        );
    }

    #[test]
    fn parse_statuses_staged_new_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        // Empty initial commit
        let mut index = repo.index().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // Stage a new file
        std::fs::write(tmp.path().join("new.txt"), "new content")
            .unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("new.txt")).unwrap();
        index.write().unwrap();

        let result = parse_statuses(&repo, tmp.path()).unwrap();
        assert_eq!(result.staged.len(), 1);
        assert_eq!(result.staged[0].relative_path, "new.txt");
        assert_eq!(
            result.staged[0].operation,
            FileChangeOperation::Create
        );
        assert!(result.unstaged.is_empty());
    }

    #[test]
    fn parse_statuses_staged_and_unstaged_mixed() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        // Commit a.txt
        std::fs::write(tmp.path().join("a.txt"), "original").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // Stage a modification to a.txt
        std::fs::write(tmp.path().join("a.txt"), "staged change")
            .unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();

        // Then modify a.txt again (unstaged on top of staged)
        std::fs::write(tmp.path().join("a.txt"), "unstaged change")
            .unwrap();

        let result = parse_statuses(&repo, tmp.path()).unwrap();
        // a.txt should appear in both staged and unstaged
        assert_eq!(result.staged.len(), 1);
        assert_eq!(
            result.staged[0].operation,
            FileChangeOperation::Modify
        );
        assert_eq!(result.unstaged.len(), 1);
        assert_eq!(
            result.unstaged[0].operation,
            FileChangeOperation::Modify
        );
    }
}
