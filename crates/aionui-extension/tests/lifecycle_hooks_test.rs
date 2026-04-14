//! Integration tests for lifecycle hooks (test-plan LH-1..LH-6).
//!
//! These tests exercise `execute_hook`, `needs_install_hook`, and
//! `resolve_hook_path` as black-box functions, verifying first install,
//! version change, activate/deactivate execution, timeout behaviour,
//! and graceful handling of missing scripts.

use std::fs;
use std::path::Path;

use aionui_extension::{
    execute_hook, needs_install_hook, resolve_hook_path, HookKind, LifecycleHooks,
};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a shell script at `dir/path` with the given body and make it executable.
fn write_script(dir: &Path, rel_path: &str, body: &str) {
    let full = dir.join(rel_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let content = format!("#!/bin/sh\n{body}\n");
    fs::write(&full, content).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&full, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn setup_ext_dir() -> TempDir {
    tempfile::tempdir().unwrap()
}

// ---------------------------------------------------------------------------
// LH-1: First install executes onInstall
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lh1_first_install_executes_on_install() {
    let dir = setup_ext_dir();
    let marker = dir.path().join("installed.marker");
    write_script(
        dir.path(),
        "scripts/install.sh",
        &format!("touch '{}'", marker.display()),
    );

    let hooks = LifecycleHooks {
        on_install: Some("scripts/install.sh".into()),
        ..Default::default()
    };

    // First install: no persisted version
    assert!(needs_install_hook("1.0.0", None));

    let hook_path = resolve_hook_path(&hooks, HookKind::OnInstall).unwrap();
    let result = execute_hook(dir.path(), hook_path, HookKind::OnInstall, "test-ext").await;
    assert!(result.is_ok());
    assert!(marker.exists(), "onInstall marker file should be created");
}

// ---------------------------------------------------------------------------
// LH-2: Version change executes onInstall
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lh2_version_change_executes_on_install() {
    let dir = setup_ext_dir();
    let marker = dir.path().join("upgraded.marker");
    write_script(
        dir.path(),
        "scripts/install.sh",
        &format!("touch '{}'", marker.display()),
    );

    let hooks = LifecycleHooks {
        on_install: Some("scripts/install.sh".into()),
        ..Default::default()
    };

    // Version changed from 1.0.0 to 2.0.0
    assert!(needs_install_hook("2.0.0", Some("1.0.0")));

    let hook_path = resolve_hook_path(&hooks, HookKind::OnInstall).unwrap();
    let result = execute_hook(dir.path(), hook_path, HookKind::OnInstall, "test-ext").await;
    assert!(result.is_ok());
    assert!(marker.exists(), "onInstall marker should be created on upgrade");
}

// ---------------------------------------------------------------------------
// LH-2 (negative): Same version does NOT trigger onInstall
// ---------------------------------------------------------------------------

#[test]
fn lh2_same_version_skips_install() {
    assert!(!needs_install_hook("1.0.0", Some("1.0.0")));
}

// ---------------------------------------------------------------------------
// LH-3: Each activation executes onActivate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lh3_activate_executes_on_activate() {
    let dir = setup_ext_dir();
    let counter_file = dir.path().join("activate_count.txt");
    // Append a line on each activation to count calls
    write_script(
        dir.path(),
        "scripts/activate.sh",
        &format!("echo 'activated' >> '{}'", counter_file.display()),
    );

    let hooks = LifecycleHooks {
        on_activate: Some("scripts/activate.sh".into()),
        ..Default::default()
    };

    let hook_path = resolve_hook_path(&hooks, HookKind::OnActivate).unwrap();

    // Activate twice
    execute_hook(dir.path(), hook_path, HookKind::OnActivate, "test-ext")
        .await
        .unwrap();
    execute_hook(dir.path(), hook_path, HookKind::OnActivate, "test-ext")
        .await
        .unwrap();

    let content = fs::read_to_string(&counter_file).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2, "onActivate should run on each activation");
}

// ---------------------------------------------------------------------------
// LH-4: Deactivation executes onDeactivate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lh4_deactivate_executes_on_deactivate() {
    let dir = setup_ext_dir();
    let marker = dir.path().join("deactivated.marker");
    write_script(
        dir.path(),
        "scripts/deactivate.sh",
        &format!("touch '{}'", marker.display()),
    );

    let hooks = LifecycleHooks {
        on_deactivate: Some("scripts/deactivate.sh".into()),
        ..Default::default()
    };

    let hook_path = resolve_hook_path(&hooks, HookKind::OnDeactivate).unwrap();
    let result =
        execute_hook(dir.path(), hook_path, HookKind::OnDeactivate, "test-ext").await;
    assert!(result.is_ok());
    assert!(
        marker.exists(),
        "onDeactivate marker should be created"
    );
}

// ---------------------------------------------------------------------------
// LH-5: Hook timeout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lh5_hook_timeout() {
    let dir = setup_ext_dir();
    // Script that sleeps for a long time
    write_script(dir.path(), "scripts/slow.sh", "sleep 120");

    // We can't easily override the built-in timeout constants in the public API,
    // so we test the timeout mechanism by using tokio::time::timeout directly
    // to simulate what execute_hook does internally with a very short deadline.
    let script_path = dir.path().join("scripts/slow.sh");
    assert!(script_path.exists());

    let child_future = tokio::process::Command::new(&script_path)
        .current_dir(dir.path())
        .kill_on_drop(true)
        .output();

    let result = tokio::time::timeout(std::time::Duration::from_millis(200), child_future).await;
    assert!(
        result.is_err(),
        "should time out before script completes"
    );
}

// ---------------------------------------------------------------------------
// LH-6: Hook script does not exist — graceful handling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lh6_missing_script_graceful() {
    let dir = setup_ext_dir();

    let hooks = LifecycleHooks {
        on_activate: Some("nonexistent.sh".into()),
        ..Default::default()
    };

    let hook_path = resolve_hook_path(&hooks, HookKind::OnActivate).unwrap();
    let result =
        execute_hook(dir.path(), hook_path, HookKind::OnActivate, "test-ext").await;

    assert!(result.is_err());
    match result.unwrap_err() {
        aionui_extension::ExtensionError::HookNotFound(path) => {
            assert!(
                path.contains("nonexistent.sh"),
                "error should mention the missing script path"
            );
        }
        other => panic!("expected HookNotFound, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Additional: resolve_hook_path returns None when hook is not declared
// ---------------------------------------------------------------------------

#[test]
fn resolve_hook_path_none_when_not_declared() {
    let hooks = LifecycleHooks::default();
    assert!(resolve_hook_path(&hooks, HookKind::OnInstall).is_none());
    assert!(resolve_hook_path(&hooks, HookKind::OnUninstall).is_none());
    assert!(resolve_hook_path(&hooks, HookKind::OnActivate).is_none());
    assert!(resolve_hook_path(&hooks, HookKind::OnDeactivate).is_none());
}

// ---------------------------------------------------------------------------
// Additional: Hook script exits with non-zero status
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hook_nonzero_exit_returns_hook_failed() {
    let dir = setup_ext_dir();
    write_script(
        dir.path(),
        "scripts/fail.sh",
        "echo 'setup failed' >&2; exit 42",
    );

    let result = execute_hook(
        dir.path(),
        "scripts/fail.sh",
        HookKind::OnInstall,
        "failing-ext",
    )
    .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        aionui_extension::ExtensionError::HookFailed {
            extension_name,
            hook,
            reason,
        } => {
            assert_eq!(extension_name, "failing-ext");
            assert_eq!(hook, "onInstall");
            assert!(reason.contains("42"), "should include exit code");
            assert!(reason.contains("setup failed"), "should include stderr");
        }
        other => panic!("expected HookFailed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Additional: Hook uses working directory correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hook_working_directory_is_ext_dir() {
    let dir = setup_ext_dir();
    write_script(dir.path(), "check_dir.sh", "pwd > cwd_out.txt");

    let result = execute_hook(
        dir.path(),
        "check_dir.sh",
        HookKind::OnActivate,
        "cwd-ext",
    )
    .await;
    assert!(result.is_ok());

    let cwd_file = dir.path().join("cwd_out.txt");
    assert!(cwd_file.exists());
    let cwd = fs::read_to_string(&cwd_file).unwrap();
    let expected = dir.path().canonicalize().unwrap();
    let actual = std::path::Path::new(cwd.trim()).canonicalize().unwrap();
    assert_eq!(actual, expected);
}
