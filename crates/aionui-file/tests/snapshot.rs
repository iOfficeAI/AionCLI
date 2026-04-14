//! Integration tests for workspace snapshot (task 7.9).
//!
//! Tests exercise `ISnapshotService` through `SnapshotService`, verifying
//! both git-repo and snapshot modes for init, get_info, compare, and
//! get_baseline_content.

use std::path::Path;

use aionui_common::FileChangeOperation;
use aionui_file::{ISnapshotService, SnapshotMode, SnapshotService};
use git2::{Repository, Signature};

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

/// Create a git repo at `path` with an initial commit containing no files.
fn init_empty_repo(path: &Path) {
    let repo = Repository::init(path).expect("init repo");
    let mut index = repo.index().expect("index");
    let tree_oid = index.write_tree().expect("write tree");
    let tree = repo.find_tree(tree_oid).expect("find tree");
    let sig = Signature::now("test", "test@test.com").expect("sig");
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .expect("commit");
}

/// Create a git repo at `path` with an initial commit that tracks a file.
/// Parent directories for nested filenames are created automatically.
fn init_repo_with_file(path: &Path, filename: &str, content: &str) {
    let file_path = path.join(filename);
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dirs");
    }
    std::fs::write(&file_path, content).expect("write file");
    let repo = Repository::init(path).expect("init repo");
    let mut index = repo.index().expect("index");
    index.add_path(Path::new(filename)).expect("add path");
    index.write().expect("write index");
    let tree_oid = index.write_tree().expect("write tree");
    let tree = repo.find_tree(tree_oid).expect("find tree");
    let sig = Signature::now("test", "test@test.com").expect("sig");
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .expect("commit");
}

// =======================================================================
// Git-repo mode tests
// =======================================================================

#[tokio::test]
async fn git_repo_init_detects_mode() {
    let tmp = tempfile::tempdir().unwrap();
    init_empty_repo(tmp.path());

    let svc = SnapshotService::new();
    let info = svc.init(tmp.path().to_str().unwrap()).await.unwrap();

    assert_eq!(info.mode, SnapshotMode::GitRepo);
    assert!(info.branch.is_some());
}

#[tokio::test]
async fn git_repo_init_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    init_empty_repo(tmp.path());

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    let info1 = svc.init(ws).await.unwrap();
    let info2 = svc.init(ws).await.unwrap();

    assert_eq!(info1.mode, info2.mode);
    assert_eq!(info1.branch, info2.branch);
}

#[tokio::test]
async fn git_repo_get_info_after_init() {
    let tmp = tempfile::tempdir().unwrap();
    init_empty_repo(tmp.path());

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();
    let info = svc.get_info(ws).await.unwrap();

    assert_eq!(info.mode, SnapshotMode::GitRepo);
    assert!(info.branch.is_some());
}

#[tokio::test]
async fn git_repo_get_info_without_init_errors() {
    let svc = SnapshotService::new();
    let result = svc.get_info("/some/random/path").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn git_repo_compare_clean_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_file(tmp.path(), "a.txt", "hello");

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();
    let result = svc.compare(ws).await.unwrap();

    assert!(result.staged.is_empty());
    assert!(result.unstaged.is_empty());
}

#[tokio::test]
async fn git_repo_compare_unstaged_modify() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_file(tmp.path(), "a.txt", "original");

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // Modify tracked file
    std::fs::write(tmp.path().join("a.txt"), "modified").unwrap();

    let result = svc.compare(ws).await.unwrap();
    assert!(result.staged.is_empty());
    assert_eq!(result.unstaged.len(), 1);
    assert_eq!(result.unstaged[0].relative_path, "a.txt");
    assert_eq!(result.unstaged[0].operation, FileChangeOperation::Modify);
}

#[tokio::test]
async fn git_repo_compare_unstaged_create() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_file(tmp.path(), "a.txt", "content");

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // Create new untracked file
    std::fs::write(tmp.path().join("new.txt"), "new content").unwrap();

    let result = svc.compare(ws).await.unwrap();
    assert!(result.staged.is_empty());
    assert_eq!(result.unstaged.len(), 1);
    assert_eq!(result.unstaged[0].relative_path, "new.txt");
    assert_eq!(result.unstaged[0].operation, FileChangeOperation::Create);
}

#[tokio::test]
async fn git_repo_compare_unstaged_delete() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_file(tmp.path(), "a.txt", "content");

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // Delete tracked file
    std::fs::remove_file(tmp.path().join("a.txt")).unwrap();

    let result = svc.compare(ws).await.unwrap();
    assert!(result.staged.is_empty());
    assert_eq!(result.unstaged.len(), 1);
    assert_eq!(result.unstaged[0].relative_path, "a.txt");
    assert_eq!(result.unstaged[0].operation, FileChangeOperation::Delete);
}

#[tokio::test]
async fn git_repo_compare_staged_changes() {
    let tmp = tempfile::tempdir().unwrap();
    init_empty_repo(tmp.path());

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // Create a file and stage it
    std::fs::write(tmp.path().join("staged.txt"), "staged").unwrap();
    let repo = Repository::open(tmp.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("staged.txt")).unwrap();
    index.write().unwrap();
    drop(repo);

    let result = svc.compare(ws).await.unwrap();
    assert_eq!(result.staged.len(), 1);
    assert_eq!(result.staged[0].relative_path, "staged.txt");
    assert_eq!(result.staged[0].operation, FileChangeOperation::Create);
    assert!(result.unstaged.is_empty());
}

#[tokio::test]
async fn git_repo_compare_mixed_staged_and_unstaged() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_file(tmp.path(), "a.txt", "original");

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // Stage a modification
    std::fs::write(tmp.path().join("a.txt"), "staged").unwrap();
    let repo = Repository::open(tmp.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.txt")).unwrap();
    index.write().unwrap();
    drop(repo);

    // Modify again (unstaged on top of staged)
    std::fs::write(tmp.path().join("a.txt"), "unstaged on top").unwrap();

    let result = svc.compare(ws).await.unwrap();
    assert_eq!(result.staged.len(), 1);
    assert_eq!(result.staged[0].operation, FileChangeOperation::Modify);
    assert_eq!(result.unstaged.len(), 1);
    assert_eq!(result.unstaged[0].operation, FileChangeOperation::Modify);
}

#[tokio::test]
async fn git_repo_baseline_content_tracked_file() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_file(tmp.path(), "readme.txt", "Hello from baseline");

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    let content = svc
        .get_baseline_content(ws, "readme.txt")
        .await
        .unwrap();
    assert_eq!(content.as_deref(), Some("Hello from baseline"));
}

#[tokio::test]
async fn git_repo_baseline_content_untracked_file() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_file(tmp.path(), "a.txt", "tracked");

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // Create a new file that isn't committed
    std::fs::write(tmp.path().join("new.txt"), "new").unwrap();

    let content = svc.get_baseline_content(ws, "new.txt").await.unwrap();
    assert!(content.is_none());
}

#[tokio::test]
async fn git_repo_baseline_content_missing_file() {
    let tmp = tempfile::tempdir().unwrap();
    init_empty_repo(tmp.path());

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    let content = svc
        .get_baseline_content(ws, "nonexistent.txt")
        .await
        .unwrap();
    assert!(content.is_none());
}

// =======================================================================
// Snapshot mode tests
// =======================================================================

#[tokio::test]
async fn snapshot_init_detects_mode() {
    let tmp = tempfile::tempdir().unwrap();
    // Plain directory, no .git
    std::fs::write(tmp.path().join("hello.txt"), "hello").unwrap();

    let svc = SnapshotService::new();
    let info = svc.init(tmp.path().to_str().unwrap()).await.unwrap();

    assert_eq!(info.mode, SnapshotMode::Snapshot);
    assert!(info.branch.is_none());
}

#[tokio::test]
async fn snapshot_init_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "data").unwrap();

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    let info1 = svc.init(ws).await.unwrap();
    let info2 = svc.init(ws).await.unwrap();

    assert_eq!(info1.mode, info2.mode);
}

#[tokio::test]
async fn snapshot_get_info_after_init() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "data").unwrap();

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();
    let info = svc.get_info(ws).await.unwrap();

    assert_eq!(info.mode, SnapshotMode::Snapshot);
    assert!(info.branch.is_none());
}

#[tokio::test]
async fn snapshot_compare_clean_after_init() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("hello.txt"), "hello").unwrap();

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // Right after init, the baseline matches the workspace — no changes
    let result = svc.compare(ws).await.unwrap();
    assert!(result.staged.is_empty());
    assert!(result.unstaged.is_empty());
}

#[tokio::test]
async fn snapshot_compare_detects_new_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("original.txt"), "original").unwrap();

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // Add a new file after baseline
    std::fs::write(tmp.path().join("added.txt"), "new content").unwrap();

    let result = svc.compare(ws).await.unwrap();
    assert!(result.staged.is_empty());
    assert_eq!(result.unstaged.len(), 1);
    assert_eq!(result.unstaged[0].relative_path, "added.txt");
    assert_eq!(result.unstaged[0].operation, FileChangeOperation::Create);
}

#[tokio::test]
async fn snapshot_compare_detects_modified_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("data.txt"), "original").unwrap();

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // Modify after baseline
    std::fs::write(tmp.path().join("data.txt"), "modified content").unwrap();

    let result = svc.compare(ws).await.unwrap();
    assert!(result.staged.is_empty());
    assert_eq!(result.unstaged.len(), 1);
    assert_eq!(result.unstaged[0].relative_path, "data.txt");
    assert_eq!(result.unstaged[0].operation, FileChangeOperation::Modify);
}

#[tokio::test]
async fn snapshot_compare_detects_deleted_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("to_delete.txt"), "content").unwrap();

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // Delete after baseline
    std::fs::remove_file(tmp.path().join("to_delete.txt")).unwrap();

    let result = svc.compare(ws).await.unwrap();
    assert!(result.staged.is_empty());
    assert_eq!(result.unstaged.len(), 1);
    assert_eq!(result.unstaged[0].relative_path, "to_delete.txt");
    assert_eq!(result.unstaged[0].operation, FileChangeOperation::Delete);
}

#[tokio::test]
async fn snapshot_baseline_content_returns_initial_content() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("doc.txt"), "initial content").unwrap();

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // Modify the file
    std::fs::write(tmp.path().join("doc.txt"), "changed content").unwrap();

    // Baseline should still return the initial version
    let content = svc
        .get_baseline_content(ws, "doc.txt")
        .await
        .unwrap();
    assert_eq!(content.as_deref(), Some("initial content"));
}

#[tokio::test]
async fn snapshot_baseline_content_new_file_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("existing.txt"), "exists").unwrap();

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // Add a new file after baseline
    std::fs::write(tmp.path().join("new.txt"), "new").unwrap();

    let content = svc.get_baseline_content(ws, "new.txt").await.unwrap();
    assert!(content.is_none());
}

#[tokio::test]
async fn snapshot_excludes_node_modules() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("app.js"), "console.log('hi')").unwrap();
    std::fs::create_dir(tmp.path().join("node_modules")).unwrap();
    std::fs::write(
        tmp.path().join("node_modules/dep.js"),
        "module.exports = {}",
    )
    .unwrap();

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // After init, workspace is clean (node_modules excluded from tracking)
    let result = svc.compare(ws).await.unwrap();
    assert!(result.staged.is_empty());
    assert!(result.unstaged.is_empty());

    // Adding a file to node_modules should not show up as a change
    std::fs::write(
        tmp.path().join("node_modules/new_dep.js"),
        "new dep",
    )
    .unwrap();

    let result = svc.compare(ws).await.unwrap();
    assert!(result.staged.is_empty());
    assert!(result.unstaged.is_empty());
}

// =======================================================================
// Error cases
// =======================================================================

#[tokio::test]
async fn init_nonexistent_workspace_errors() {
    let svc = SnapshotService::new();
    let result = svc.init("/nonexistent/path/xyz123abc").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn compare_without_init_errors() {
    let svc = SnapshotService::new();
    let result = svc.compare("/some/workspace").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn baseline_without_init_errors() {
    let svc = SnapshotService::new();
    let result = svc.get_baseline_content("/some/ws", "file.txt").await;
    assert!(result.is_err());
}

// =======================================================================
// Full path validation
// =======================================================================

#[tokio::test]
async fn compare_result_contains_full_paths() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_file(tmp.path(), "src/main.rs", "fn main() {}");

    let svc = SnapshotService::new();
    let ws = tmp.path().to_str().unwrap();
    svc.init(ws).await.unwrap();

    // Modify the file
    std::fs::write(
        tmp.path().join("src/main.rs"),
        "fn main() { println!(\"hi\") }",
    )
    .unwrap();

    let result = svc.compare(ws).await.unwrap();
    assert_eq!(result.unstaged.len(), 1);

    // relative_path should be just the file path relative to workspace
    assert_eq!(result.unstaged[0].relative_path, "src/main.rs");

    // file_path should contain the workspace prefix
    let canonical = std::fs::canonicalize(tmp.path()).unwrap();
    assert!(result.unstaged[0]
        .file_path
        .starts_with(canonical.to_str().unwrap()));
}
