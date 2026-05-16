use aionui_db::{init_database, init_database_memory, maybe_copy_legacy_database};
use sqlx::Row;

// -- T1.1 Initialization --

#[tokio::test]
async fn init_creates_users_table() {
    let db = init_database_memory().await.unwrap();

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(db.pool())
        .await
        .unwrap();

    assert!(
        count.0 >= 1,
        "users table should exist and have at least the system user"
    );
}

// -- T1.2 Pragma configuration --

#[tokio::test]
async fn pragma_foreign_keys_enabled() {
    let db = init_database_memory().await.unwrap();

    let row: (i64,) = sqlx::query_as("PRAGMA foreign_keys")
        .fetch_one(db.pool())
        .await
        .unwrap();

    assert_eq!(row.0, 1, "foreign_keys should be ON");
}

#[tokio::test]
async fn pragma_busy_timeout() {
    let db = init_database_memory().await.unwrap();

    let row: (i64,) = sqlx::query_as("PRAGMA busy_timeout")
        .fetch_one(db.pool())
        .await
        .unwrap();

    assert_eq!(row.0, 5000, "busy_timeout should be 5000ms");
}

#[tokio::test]
async fn pragma_journal_mode_wal_on_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = init_database(&dir.path().join("test.db")).await.unwrap();

    let row: (String,) = sqlx::query_as("PRAGMA journal_mode")
        .fetch_one(db.pool())
        .await
        .unwrap();

    assert_eq!(
        row.0.to_lowercase(),
        "wal",
        "journal_mode should be WAL for file-backed DB"
    );
    db.close().await;
}

// -- T1.3 Idempotent re-initialization --

#[tokio::test]
async fn idempotent_reinit_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");

    // First init + insert test data
    let db = init_database(&path).await.unwrap();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, created_at, updated_at) \
         VALUES ('u1', 'alice', 'hash123', 1000, 1000)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.close().await;

    // Second init — data should persist
    let db = init_database(&path).await.unwrap();
    let row = sqlx::query("SELECT username FROM users WHERE id = 'u1'")
        .fetch_one(db.pool())
        .await
        .unwrap();

    assert_eq!(row.get::<String, _>("username"), "alice");
    db.close().await;
}

// -- T1.4 Migrations --

#[tokio::test]
async fn migrations_applied() {
    let db = init_database_memory().await.unwrap();

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM _sqlx_migrations WHERE success = 1")
        .fetch_one(db.pool())
        .await
        .unwrap();

    assert!(count.0 >= 1, "at least one migration should be applied");
}

// -- T1.5 System default user --

#[tokio::test]
async fn system_default_user_exists() {
    let db = init_database_memory().await.unwrap();

    let row = sqlx::query("SELECT id, username, password_hash FROM users WHERE id = 'system_default_user'")
        .fetch_one(db.pool())
        .await
        .unwrap();

    assert_eq!(row.get::<String, _>("id"), "system_default_user");
    assert_eq!(row.get::<String, _>("username"), "admin");
    assert_eq!(
        row.get::<String, _>("password_hash"),
        "",
        "system user should have empty password hash"
    );
}

#[tokio::test]
async fn system_user_has_valid_timestamps() {
    let before = aionui_common::now_ms();
    let db = init_database_memory().await.unwrap();
    let after = aionui_common::now_ms();

    let row = sqlx::query("SELECT created_at, updated_at FROM users WHERE id = 'system_default_user'")
        .fetch_one(db.pool())
        .await
        .unwrap();

    let created = row.get::<i64, _>("created_at");
    let updated = row.get::<i64, _>("updated_at");
    assert!(
        created >= before && created <= after,
        "created_at should be within test window"
    );
    assert!(
        updated >= before && updated <= after,
        "updated_at should be within test window"
    );
}

// -- Schema validation --

#[tokio::test]
async fn users_table_accepts_all_columns() {
    let db = init_database_memory().await.unwrap();

    sqlx::query(
        "INSERT INTO users \
         (id, username, email, password_hash, avatar_path, jwt_secret, created_at, updated_at, last_login) \
         VALUES ('u1', 'testuser', 'test@example.com', 'hash', '/avatar.png', 'secret', 1000, 2000, 3000)",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let row = sqlx::query("SELECT * FROM users WHERE id = 'u1'")
        .fetch_one(db.pool())
        .await
        .unwrap();

    assert_eq!(row.get::<String, _>("email"), "test@example.com");
    assert_eq!(
        row.get::<Option<String>, _>("avatar_path"),
        Some("/avatar.png".to_string())
    );
    assert_eq!(row.get::<Option<String>, _>("jwt_secret"), Some("secret".to_string()));
    assert_eq!(row.get::<Option<i64>, _>("last_login"), Some(3000));
}

#[tokio::test]
async fn username_unique_constraint() {
    let db = init_database_memory().await.unwrap();

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, created_at, updated_at) \
         VALUES ('u1', 'duplicate', 'h', 1, 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let result = sqlx::query(
        "INSERT INTO users (id, username, password_hash, created_at, updated_at) \
         VALUES ('u2', 'duplicate', 'h', 1, 1)",
    )
    .execute(db.pool())
    .await;

    assert!(result.is_err(), "duplicate username should violate unique constraint");
}

#[tokio::test]
async fn email_unique_constraint() {
    let db = init_database_memory().await.unwrap();

    sqlx::query(
        "INSERT INTO users (id, username, email, password_hash, created_at, updated_at) \
         VALUES ('u1', 'user1', 'same@example.com', 'h', 1, 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let result = sqlx::query(
        "INSERT INTO users (id, username, email, password_hash, created_at, updated_at) \
         VALUES ('u2', 'user2', 'same@example.com', 'h', 1, 1)",
    )
    .execute(db.pool())
    .await;

    assert!(result.is_err(), "duplicate email should violate unique constraint");
}

// -- Corruption recovery --

#[tokio::test]
async fn corruption_recovery_creates_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");

    // Write invalid content to simulate corruption
    std::fs::write(&path, b"not a valid sqlite database").unwrap();

    let db = init_database(&path).await.unwrap();

    // Recovered database should work
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(count.0 >= 1, "recovered DB should have system user");

    // Backup file should exist
    let has_backup = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains("backup"));
    assert!(has_backup, "backup of corrupted file should exist");

    db.close().await;
}

// -- Directory creation --

#[tokio::test]
async fn creates_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("nested").join("test.db");

    let db = init_database(&path).await.unwrap();
    assert!(path.exists(), "database file should be created in nested directory");
    db.close().await;
}

// -- Legacy database copy --

#[test]
fn copy_legacy_noop_when_no_legacy_db() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("aioncli.db");

    maybe_copy_legacy_database(&target).unwrap();
    assert!(!target.exists(), "target should not be created when no legacy db");
}

#[test]
fn copy_legacy_noop_when_target_exists() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("aioncli.db");
    let legacy = dir.path().join("aionui.db");

    std::fs::write(&legacy, b"legacy data").unwrap();
    std::fs::write(&target, b"existing target").unwrap();

    maybe_copy_legacy_database(&target).unwrap();

    let content = std::fs::read(&target).unwrap();
    assert_eq!(content, b"existing target", "existing target must not be overwritten");
}

#[test]
fn copy_legacy_copies_when_target_missing() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("aioncli.db");
    let legacy = dir.path().join("aionui.db");

    std::fs::write(&legacy, b"legacy database content").unwrap();

    maybe_copy_legacy_database(&target).unwrap();

    assert!(target.exists(), "target should be created");
    let content = std::fs::read(&target).unwrap();
    assert_eq!(content, b"legacy database content", "content should match legacy");

    let legacy_content = std::fs::read(&legacy).unwrap();
    assert_eq!(
        legacy_content, b"legacy database content",
        "legacy must not be modified"
    );
}

#[test]
fn copy_legacy_removes_wal_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("aioncli.db");
    let legacy = dir.path().join("aionui.db");

    std::fs::write(&legacy, b"legacy data").unwrap();
    std::fs::write(target.with_extension("db-wal"), b"wal").unwrap();
    std::fs::write(target.with_extension("db-shm"), b"shm").unwrap();

    maybe_copy_legacy_database(&target).unwrap();

    assert!(
        !target.with_extension("db-wal").exists(),
        "WAL sidecar should be removed"
    );
    assert!(
        !target.with_extension("db-shm").exists(),
        "SHM sidecar should be removed"
    );
}

#[test]
fn copy_legacy_overwrites_leftover_tmp() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("aioncli.db");
    let legacy = dir.path().join("aionui.db");
    let tmp = target.with_extension("db.tmp");

    std::fs::write(&legacy, b"real data").unwrap();
    std::fs::write(&tmp, b"leftover from crash").unwrap();

    maybe_copy_legacy_database(&target).unwrap();

    assert!(target.exists(), "target should be created");
    assert!(!tmp.exists(), "tmp file should be cleaned up via rename");
    let content = std::fs::read(&target).unwrap();
    assert_eq!(content, b"real data");
}

#[tokio::test]
async fn copy_legacy_then_init_database_works() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("aioncli.db");
    let legacy = dir.path().join("aionui.db");

    let legacy_db = init_database(&legacy).await.unwrap();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, created_at, updated_at) \
         VALUES ('test_user', 'alice', 'hash', 1000, 1000)",
    )
    .execute(legacy_db.pool())
    .await
    .unwrap();
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(legacy_db.pool())
        .await
        .unwrap();
    legacy_db.close().await;

    maybe_copy_legacy_database(&target).unwrap();

    let db = init_database(&target).await.unwrap();

    let row = sqlx::query("SELECT username FROM users WHERE id = 'test_user'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("username"), "alice");

    let legacy_db2 = init_database(&legacy).await.unwrap();
    let row2 = sqlx::query("SELECT username FROM users WHERE id = 'test_user'")
        .fetch_one(legacy_db2.pool())
        .await
        .unwrap();
    assert_eq!(row2.get::<String, _>("username"), "alice");

    db.close().await;
    legacy_db2.close().await;
}
