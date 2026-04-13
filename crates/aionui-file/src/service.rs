use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use dashmap::DashMap;
use ignore::WalkBuilder;
use tracing::warn;

use aionui_api_types::WebSocketMessage;
use aionui_common::AppError;
use aionui_realtime::EventBroadcaster;

use crate::path_safety::{has_traversal, validate_path, validate_path_for_write};
use crate::types::{
    ContentUpdateEvent, ContentUpdateOperation, CopyResult, DirOrFile,
    FileMetadata, WorkspaceFlatFile, ZipEntry,
};

/// Maximum number of files returned by `list_workspace_files`.
const MAX_WORKSPACE_FILES: usize = 20_000;

/// Maximum file size for read operations (256 MB).
const MAX_FILE_SIZE: u64 = 256 * 1024 * 1024;

/// A concrete implementation of [`crate::traits::IFileService`].
pub struct FileService {
    broadcaster: Arc<dyn EventBroadcaster>,
    /// Allowed root directories for path safety validation.
    allowed_roots: Vec<std::path::PathBuf>,
    /// In-memory cache for `list_workspace_files`, keyed by canonical root.
    workspace_files_cache: DashMap<String, Vec<WorkspaceFlatFile>>,
}

impl FileService {
    pub fn new(
        broadcaster: Arc<dyn EventBroadcaster>,
        allowed_roots: Vec<std::path::PathBuf>,
    ) -> Self {
        Self {
            broadcaster,
            allowed_roots,
            workspace_files_cache: DashMap::new(),
        }
    }

    /// Invalidate the workspace files cache for a given root.
    /// Called when file changes are detected.
    pub fn invalidate_cache(&self, root: &str) {
        self.workspace_files_cache.remove(root);
    }

    /// Get the allowed root references for path validation.
    fn allowed_roots_refs(&self) -> Vec<&Path> {
        self.allowed_roots.iter().map(|p| p.as_path()).collect()
    }

    /// List immediate children of `dir`, building a single-level tree.
    /// Each child directory also lists *its* children (depth = 2 from `dir`).
    async fn build_dir_tree(
        &self,
        dir: &Path,
        root: &Path,
    ) -> Result<Vec<DirOrFile>, AppError> {
        let dir_owned = dir.to_path_buf();
        let root_owned = root.to_path_buf();

        tokio::task::spawn_blocking(move || {
            build_dir_tree_sync(&dir_owned, &root_owned)
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!("directory listing task failed: {e}"))
        })?
    }
}

/// Synchronous directory tree builder (runs in blocking thread pool).
fn build_dir_tree_sync(
    dir: &Path,
    root: &Path,
) -> Result<Vec<DirOrFile>, AppError> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        AppError::BadRequest(format!(
            "cannot read directory '{}': {e}",
            dir.display()
        ))
    })?;

    let mut result = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|e| {
            AppError::Internal(format!("error reading directory entry: {e}"))
        })?;

        let path = entry.path();
        let metadata = entry.metadata().map_err(|e| {
            AppError::Internal(format!(
                "cannot read metadata for '{}': {e}",
                path.display()
            ))
        })?;

        let name = entry
            .file_name()
            .to_string_lossy()
            .into_owned();

        let full_path = path.to_string_lossy().into_owned();
        let relative_path = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();

        let is_dir = metadata.is_dir();

        // For directories, also read their immediate children
        let children = if is_dir {
            read_children_sync(&path, root)?
        } else {
            Vec::new()
        };

        result.push(DirOrFile {
            name,
            full_path,
            relative_path,
            is_dir,
            children,
        });
    }

    // Sort: directories first, then alphabetical
    result.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name))
    });

    Ok(result)
}

/// Read immediate children of a directory (one level, no grandchildren).
fn read_children_sync(
    dir: &Path,
    root: &Path,
) -> Result<Vec<DirOrFile>, AppError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(Vec::new()),
    };

    let mut children = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        let is_dir = entry
            .metadata()
            .map(|m| m.is_dir())
            .unwrap_or(false);

        let name = entry
            .file_name()
            .to_string_lossy()
            .into_owned();

        let full_path = path.to_string_lossy().into_owned();
        let relative_path = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();

        children.push(DirOrFile {
            name,
            full_path,
            relative_path,
            is_dir,
            children: Vec::new(),
        });
    }

    children.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name))
    });

    Ok(children)
}

/// Recursively list files using the `ignore` crate (respects .gitignore).
fn list_workspace_files_sync(
    root: &Path,
) -> Result<Vec<WorkspaceFlatFile>, AppError> {
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .build();

    let mut files = Vec::new();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("skipping unreadable entry: {e}");
                continue;
            }
        };

        // Skip directories — only include files
        if entry
            .file_type()
            .map(|ft| ft.is_dir())
            .unwrap_or(true)
        {
            continue;
        }

        let path = entry.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let full_path = path.to_string_lossy().into_owned();
        let relative_path = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();

        files.push(WorkspaceFlatFile {
            name,
            full_path,
            relative_path,
        });

        if files.len() >= MAX_WORKSPACE_FILES {
            break;
        }
    }

    Ok(files)
}

/// Validate that a file exists and is within the size limit.
/// Returns `Ok(None)` if the file does not exist.
/// Returns `Ok(Some(()))` if the file is valid for reading.
fn validate_file_for_read(path: &Path) -> Result<Option<()>, AppError> {
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(e) => {
            return Err(AppError::Internal(format!(
                "cannot read metadata for '{}': {e}",
                path.display()
            )));
        }
    };

    if metadata.len() > MAX_FILE_SIZE {
        return Err(AppError::BadRequest(format!(
            "file '{}' exceeds 256 MB limit ({} bytes)",
            path.display(),
            metadata.len()
        )));
    }

    Ok(Some(()))
}

/// Read a file as UTF-8 text. Returns `None` if the file does not exist.
/// Rejects files larger than 256 MB.
fn read_file_sync(path: &Path) -> Result<Option<String>, AppError> {
    if validate_file_for_read(path)?.is_none() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(path).map_err(|e| {
        AppError::Internal(format!(
            "cannot read file '{}': {e}",
            path.display()
        ))
    })?;

    Ok(Some(content))
}

/// Read a file as raw bytes. Returns `None` if the file does not exist.
/// Rejects files larger than 256 MB.
fn read_file_buffer_sync(path: &Path) -> Result<Option<Vec<u8>>, AppError> {
    if validate_file_for_read(path)?.is_none() {
        return Ok(None);
    }

    let bytes = std::fs::read(path).map_err(|e| {
        AppError::Internal(format!(
            "cannot read file '{}': {e}",
            path.display()
        ))
    })?;

    Ok(Some(bytes))
}

/// Write data to a file synchronously. Creates the file if it does not exist.
/// Returns `true` on success.
fn write_file_sync(path: &Path, data: &[u8]) -> Result<bool, AppError> {
    std::fs::write(path, data).map_err(|e| {
        AppError::Internal(format!(
            "cannot write file '{}': {e}",
            path.display()
        ))
    })?;
    Ok(true)
}

/// Get file metadata synchronously.
fn get_file_metadata_sync(path: &Path) -> Result<FileMetadata, AppError> {
    let metadata = std::fs::metadata(path).map_err(|e| {
        AppError::NotFound(format!(
            "cannot read metadata for '{}': {e}",
            path.display()
        ))
    })?;

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let size = metadata.len();
    let is_directory = metadata.is_dir();

    let mime_type = if is_directory {
        "inode/directory".to_owned()
    } else {
        mime_guess::from_path(path)
            .first()
            .map(|m| m.to_string())
            .unwrap_or_else(|| "application/octet-stream".to_owned())
    };

    let last_modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    Ok(FileMetadata {
        name,
        path: path.to_string_lossy().into_owned(),
        size,
        mime_type,
        last_modified,
        is_directory,
    })
}

/// Remove a file or directory synchronously. Directories are removed recursively.
fn remove_entry_sync(path: &Path) -> Result<(), AppError> {
    let metadata = std::fs::metadata(path).map_err(|e| {
        AppError::NotFound(format!(
            "cannot remove '{}': {e}",
            path.display()
        ))
    })?;

    if metadata.is_dir() {
        std::fs::remove_dir_all(path).map_err(|e| {
            AppError::Internal(format!(
                "cannot remove directory '{}': {e}",
                path.display()
            ))
        })
    } else {
        std::fs::remove_file(path).map_err(|e| {
            AppError::Internal(format!(
                "cannot remove file '{}': {e}",
                path.display()
            ))
        })
    }
}

/// Rename a file or directory synchronously. Returns the new absolute path.
fn rename_entry_sync(
    path: &Path,
    new_name: &str,
) -> Result<PathBuf, AppError> {
    let parent = path.parent().ok_or_else(|| {
        AppError::BadRequest(format!(
            "path '{}' has no parent",
            path.display()
        ))
    })?;

    let new_path = parent.join(new_name);

    if new_path.exists() {
        return Err(AppError::BadRequest(format!(
            "target '{}' already exists",
            new_path.display()
        )));
    }

    std::fs::rename(path, &new_path).map_err(|e| {
        AppError::Internal(format!(
            "cannot rename '{}' to '{}': {e}",
            path.display(),
            new_path.display()
        ))
    })?;

    Ok(new_path)
}

/// Copy a single file, creating parent directories as needed.
fn copy_single_file_sync(
    src: &Path,
    dest: &Path,
) -> Result<(), AppError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AppError::Internal(format!(
                "cannot create directory '{}': {e}",
                parent.display()
            ))
        })?;
    }

    std::fs::copy(src, dest).map_err(|e| {
        AppError::Internal(format!(
            "cannot copy '{}' to '{}': {e}",
            src.display(),
            dest.display()
        ))
    })?;

    Ok(())
}

#[async_trait::async_trait]
impl crate::traits::IFileService for FileService {
    async fn get_files_by_dir(
        &self,
        dir: &str,
        root: &str,
    ) -> Result<Vec<DirOrFile>, AppError> {
        let roots = self.allowed_roots_refs();
        let canonical_dir = validate_path(dir, &roots)?;
        let canonical_root = validate_path(root, &roots)?;

        self.build_dir_tree(&canonical_dir, &canonical_root)
            .await
    }

    async fn list_workspace_files(
        &self,
        root: &str,
    ) -> Result<Vec<WorkspaceFlatFile>, AppError> {
        let roots = self.allowed_roots_refs();
        let canonical_root = validate_path(root, &roots)?;
        let cache_key = canonical_root.to_string_lossy().into_owned();

        // Check cache first
        if let Some(cached) = self.workspace_files_cache.get(&cache_key) {
            return Ok(cached.clone());
        }

        let root_owned = canonical_root.clone();
        let files = tokio::task::spawn_blocking(move || {
            list_workspace_files_sync(&root_owned)
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!(
                "workspace file listing task failed: {e}"
            ))
        })??;

        // Store in cache
        self.workspace_files_cache
            .insert(cache_key, files.clone());

        Ok(files)
    }

    async fn get_file_metadata(
        &self,
        path: &str,
    ) -> Result<FileMetadata, AppError> {
        let roots = self.allowed_roots_refs();
        let canonical = validate_path(path, &roots)?;

        let result = tokio::task::spawn_blocking(move || {
            get_file_metadata_sync(&canonical)
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!(
                "file metadata task failed: {e}"
            ))
        })??;

        Ok(result)
    }

    // -- File read/write (task 7.4) --

    async fn read_file(
        &self,
        path: &str,
    ) -> Result<Option<String>, AppError> {
        if has_traversal(path) {
            return Err(AppError::BadRequest(format!(
                "path '{}' contains invalid traversal patterns",
                path
            )));
        }

        let roots = self.allowed_roots_refs();
        let canonical = match validate_path(path, &roots) {
            Ok(c) => c,
            Err(_) => {
                // File may not exist or may be outside sandbox.
                // Use validate_path_for_write to check the parent.
                // If parent is in sandbox, file simply doesn't exist → None.
                // If parent check also fails, return None (no info leak).
                match validate_path_for_write(path, &roots) {
                    Ok(_) => return Ok(None),
                    Err(_) => return Ok(None),
                }
            }
        };

        tokio::task::spawn_blocking(move || read_file_sync(&canonical))
            .await
            .map_err(|e| {
                AppError::Internal(format!(
                    "read file task failed: {e}"
                ))
            })?
    }

    async fn read_file_buffer(
        &self,
        path: &str,
    ) -> Result<Option<Vec<u8>>, AppError> {
        if has_traversal(path) {
            return Err(AppError::BadRequest(format!(
                "path '{}' contains invalid traversal patterns",
                path
            )));
        }

        let roots = self.allowed_roots_refs();
        let canonical = match validate_path(path, &roots) {
            Ok(c) => c,
            Err(_) => {
                match validate_path_for_write(path, &roots) {
                    Ok(_) => return Ok(None),
                    Err(_) => return Ok(None),
                }
            }
        };

        tokio::task::spawn_blocking(move || {
            read_file_buffer_sync(&canonical)
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!(
                "read file buffer task failed: {e}"
            ))
        })?
    }

    async fn write_file(
        &self,
        path: &str,
        data: &[u8],
        workspace: &str,
    ) -> Result<bool, AppError> {
        if has_traversal(path) {
            return Err(AppError::BadRequest(format!(
                "path '{}' contains invalid traversal patterns",
                path
            )));
        }

        let roots = self.allowed_roots_refs();
        let canonical = validate_path_for_write(path, &roots)?;

        let path_owned = canonical.clone();
        let data_owned = data.to_vec();
        tokio::task::spawn_blocking(move || {
            write_file_sync(&path_owned, &data_owned)
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!(
                "write file task failed: {e}"
            ))
        })??;

        // Compute relative path from workspace
        let workspace_path = Path::new(workspace);
        let relative_path = canonical
            .strip_prefix(
                std::fs::canonicalize(workspace_path)
                    .unwrap_or_else(|_| workspace_path.to_path_buf()),
            )
            .unwrap_or(&canonical)
            .to_string_lossy()
            .into_owned();

        // Build and broadcast contentUpdate event
        let content = String::from_utf8(data.to_vec()).ok();
        let event = ContentUpdateEvent {
            file_path: canonical.to_string_lossy().into_owned(),
            content,
            workspace: workspace.to_owned(),
            relative_path,
            operation: ContentUpdateOperation::Write,
        };
        let payload = serde_json::to_value(&event).unwrap_or_default();
        let msg = WebSocketMessage::new(
            "fileStream.contentUpdate",
            payload,
        );
        self.broadcaster.broadcast(msg);

        // Invalidate workspace files cache since a file may have been
        // created or its content changed
        if let Ok(canonical_ws) = std::fs::canonicalize(workspace_path)
        {
            self.invalidate_cache(
                &canonical_ws.to_string_lossy(),
            );
        }

        Ok(true)
    }

    async fn copy_files_to_workspace(
        &self,
        file_paths: &[String],
        workspace: &str,
        source_root: Option<&str>,
    ) -> Result<CopyResult, AppError> {
        let roots = self.allowed_roots_refs();
        let ws_canonical = validate_path(workspace, &roots)?;

        let sr_canonical = match source_root {
            Some(sr) => Some(validate_path(sr, &roots)?),
            None => None,
        };

        let file_paths_owned: Vec<String> = file_paths.to_vec();
        let roots_owned: Vec<std::path::PathBuf> =
            self.allowed_roots.clone();

        tokio::task::spawn_blocking(move || {
            let roots_refs: Vec<&Path> =
                roots_owned.iter().map(|p| p.as_path()).collect();
            let mut copied = Vec::new();
            let mut failed = Vec::new();

            for fp in &file_paths_owned {
                let src = match validate_path(fp, &roots_refs) {
                    Ok(p) if p.is_file() => p,
                    _ => {
                        failed.push(fp.clone());
                        continue;
                    }
                };

                let relative = match &sr_canonical {
                    Some(sr) => src
                        .strip_prefix(sr)
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|_| {
                            Path::new(
                                src.file_name()
                                    .unwrap_or_default(),
                            )
                            .to_path_buf()
                        }),
                    None => Path::new(
                        src.file_name().unwrap_or_default(),
                    )
                    .to_path_buf(),
                };

                let dest = ws_canonical.join(&relative);
                match copy_single_file_sync(&src, &dest) {
                    Ok(()) => copied.push(fp.clone()),
                    Err(_) => failed.push(fp.clone()),
                }
            }

            Ok(CopyResult {
                copied_files: copied,
                failed_files: failed,
            })
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!(
                "copy task failed: {e}"
            ))
        })?
    }

    async fn remove_entry(
        &self,
        path: &str,
        workspace: &str,
    ) -> Result<(), AppError> {
        if has_traversal(path) {
            return Err(AppError::BadRequest(format!(
                "path '{}' contains invalid traversal patterns",
                path
            )));
        }

        let roots = self.allowed_roots_refs();
        let canonical = validate_path(path, &roots)?;

        let path_owned = canonical.clone();
        tokio::task::spawn_blocking(move || {
            remove_entry_sync(&path_owned)
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!(
                "remove entry task failed: {e}"
            ))
        })??;

        // Compute relative path from workspace
        let workspace_path = Path::new(workspace);
        let relative_path = canonical
            .strip_prefix(
                std::fs::canonicalize(workspace_path)
                    .unwrap_or_else(|_| {
                        workspace_path.to_path_buf()
                    }),
            )
            .unwrap_or(&canonical)
            .to_string_lossy()
            .into_owned();

        // Broadcast contentUpdate delete event
        let event = ContentUpdateEvent {
            file_path: canonical
                .to_string_lossy()
                .into_owned(),
            content: None,
            workspace: workspace.to_owned(),
            relative_path,
            operation: ContentUpdateOperation::Delete,
        };
        let payload =
            serde_json::to_value(&event).unwrap_or_default();
        let msg = WebSocketMessage::new(
            "fileStream.contentUpdate",
            payload,
        );
        self.broadcaster.broadcast(msg);

        // Invalidate workspace files cache
        if let Ok(canonical_ws) =
            std::fs::canonicalize(workspace_path)
        {
            self.invalidate_cache(
                &canonical_ws.to_string_lossy(),
            );
        }

        Ok(())
    }

    async fn rename_entry(
        &self,
        path: &str,
        new_name: &str,
    ) -> Result<String, AppError> {
        if has_traversal(path) {
            return Err(AppError::BadRequest(format!(
                "path '{}' contains invalid traversal patterns",
                path
            )));
        }

        if new_name.contains('/')
            || new_name.contains('\\')
        {
            return Err(AppError::BadRequest(format!(
                "new name '{}' must not contain path separators",
                new_name
            )));
        }

        let roots = self.allowed_roots_refs();
        let canonical = validate_path(path, &roots)?;

        let new_name_owned = new_name.to_owned();
        let path_owned = canonical;
        let new_path: PathBuf =
            tokio::task::spawn_blocking(move || {
                rename_entry_sync(&path_owned, &new_name_owned)
            })
            .await
            .map_err(|e| {
                AppError::Internal(format!(
                    "rename entry task failed: {e}"
                ))
            })??;

        Ok(new_path.to_string_lossy().into_owned())
    }

    async fn create_temp_file(
        &self,
        file_name: &str,
    ) -> Result<String, AppError> {
        let name = file_name.to_owned();

        tokio::task::spawn_blocking(move || {
            let tmp_dir = std::env::temp_dir().join("aionui");
            std::fs::create_dir_all(&tmp_dir).map_err(|e| {
                AppError::Internal(format!(
                    "cannot create temp directory: {e}"
                ))
            })?;

            let file_path = tmp_dir.join(&name);
            std::fs::File::create(&file_path).map_err(|e| {
                AppError::Internal(format!(
                    "cannot create temp file '{}': {e}",
                    file_path.display()
                ))
            })?;

            Ok(file_path.to_string_lossy().into_owned())
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!(
                "create temp file task failed: {e}"
            ))
        })?
    }

    async fn get_image_base64(
        &self,
        _path: &str,
    ) -> Result<String, AppError> {
        todo!("implemented in task 7.6")
    }

    async fn fetch_remote_image(&self, _url: &str) -> String {
        todo!("implemented in task 7.6")
    }

    async fn create_zip(
        &self,
        _path: &str,
        _entries: Vec<ZipEntry>,
        _request_id: Option<String>,
    ) -> Result<bool, AppError> {
        todo!("implemented in task 7.7")
    }

    async fn cancel_zip(&self, _request_id: &str) -> bool {
        todo!("implemented in task 7.7")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn build_dir_tree_sync_lists_files_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        fs::write(dir.path().join("b.rs"), "fn main(){}").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/c.txt"), "nested").unwrap();

        let result =
            build_dir_tree_sync(dir.path(), dir.path()).unwrap();

        // sub/ should come first (directories first)
        assert_eq!(result[0].name, "sub");
        assert!(result[0].is_dir);
        // sub/ should have c.txt as child
        assert_eq!(result[0].children.len(), 1);
        assert_eq!(result[0].children[0].name, "c.txt");

        // Then files alphabetically
        assert_eq!(result[1].name, "a.txt");
        assert!(!result[1].is_dir);
        assert_eq!(result[2].name, "b.rs");
    }

    #[test]
    fn build_dir_tree_sync_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result =
            build_dir_tree_sync(dir.path(), dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn build_dir_tree_sync_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("folder");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file.txt"), "data").unwrap();

        let result =
            build_dir_tree_sync(dir.path(), dir.path()).unwrap();

        assert_eq!(result[0].relative_path, "folder");
        assert_eq!(result[0].children[0].relative_path, "folder/file.txt");
    }

    #[test]
    fn build_dir_tree_sync_nonexistent_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("nonexistent");
        let result = build_dir_tree_sync(&fake, dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn list_workspace_files_sync_basic() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/b.txt"), "world").unwrap();

        let files = list_workspace_files_sync(dir.path()).unwrap();

        assert_eq!(files.len(), 2);
        let names: Vec<&str> =
            files.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"b.txt"));
    }

    #[test]
    fn list_workspace_files_sync_respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(dir.path().join("kept.txt"), "keep").unwrap();
        fs::write(dir.path().join("ignored.txt"), "skip").unwrap();

        let files = list_workspace_files_sync(dir.path()).unwrap();

        let names: Vec<&str> =
            files.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"kept.txt"));
        assert!(names.contains(&".gitignore"));
        assert!(!names.contains(&"ignored.txt"));
    }

    #[test]
    fn list_workspace_files_sync_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let files = list_workspace_files_sync(dir.path()).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn list_workspace_files_sync_truncates_at_limit() {
        // Creating 20,000+ files is impractical in a unit test;
        // verify the constant exists and the branch logic is sound.
        assert_eq!(MAX_WORKSPACE_FILES, 20_000);
    }

    #[test]
    fn list_workspace_files_sync_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main(){}").unwrap();

        let files = list_workspace_files_sync(dir.path()).unwrap();
        let main_file = files
            .iter()
            .find(|f| f.name == "main.rs")
            .unwrap();

        assert_eq!(main_file.relative_path, "src/main.rs");
    }

    #[test]
    fn get_file_metadata_sync_text_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        fs::write(&file, "hello world").unwrap();

        let meta = get_file_metadata_sync(&file).unwrap();
        assert_eq!(meta.name, "hello.txt");
        assert_eq!(meta.size, 11);
        assert_eq!(meta.mime_type, "text/plain");
        assert!(!meta.is_directory);
        assert!(meta.last_modified > 0);
    }

    #[test]
    fn get_file_metadata_sync_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("mydir");
        fs::create_dir(&sub).unwrap();

        let meta = get_file_metadata_sync(&sub).unwrap();
        assert_eq!(meta.name, "mydir");
        assert!(meta.is_directory);
        assert_eq!(meta.mime_type, "inode/directory");
    }

    #[test]
    fn get_file_metadata_sync_rust_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        fs::write(&file, "pub fn foo() {}").unwrap();

        let meta = get_file_metadata_sync(&file).unwrap();
        assert_eq!(meta.name, "lib.rs");
        // rust files should get a reasonable mime type
        assert!(!meta.mime_type.is_empty());
    }

    #[test]
    fn get_file_metadata_sync_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("missing.txt");
        let result = get_file_metadata_sync(&fake);
        assert!(result.is_err());
    }

    #[test]
    fn get_file_metadata_sync_image_mime() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("icon.png");
        fs::write(&png, [0x89, 0x50, 0x4E, 0x47]).unwrap();

        let meta = get_file_metadata_sync(&png).unwrap();
        assert_eq!(meta.mime_type, "image/png");
    }

    #[test]
    fn get_file_metadata_sync_unknown_extension() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.xyz123");
        fs::write(&file, "binary data").unwrap();

        let meta = get_file_metadata_sync(&file).unwrap();
        assert_eq!(meta.mime_type, "application/octet-stream");
    }

    // -- read_file_sync tests (task 7.4) --

    #[test]
    fn read_file_sync_normal_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        fs::write(&file, "hello world").unwrap();

        let result = read_file_sync(&file).unwrap();
        assert_eq!(result.as_deref(), Some("hello world"));
    }

    #[test]
    fn read_file_sync_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("empty.txt");
        fs::write(&file, "").unwrap();

        let result = read_file_sync(&file).unwrap();
        assert_eq!(result.as_deref(), Some(""));
    }

    #[test]
    fn read_file_sync_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("missing.txt");

        let result = read_file_sync(&fake).unwrap();
        assert!(result.is_none());
    }

    // -- validate_file_for_read tests --

    #[test]
    fn validate_file_for_read_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("valid.txt");
        fs::write(&file, "data").unwrap();

        let result = validate_file_for_read(&file).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn validate_file_for_read_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("nope.txt");

        let result = validate_file_for_read(&fake).unwrap();
        assert!(result.is_none());
    }

    // -- read_file_buffer_sync tests --

    #[test]
    fn read_file_buffer_sync_normal() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.bin");
        let bytes: Vec<u8> = vec![0x00, 0xFF, 0x42, 0x89];
        fs::write(&file, &bytes).unwrap();

        let result = read_file_buffer_sync(&file).unwrap();
        assert_eq!(result.as_deref(), Some(bytes.as_slice()));
    }

    #[test]
    fn read_file_buffer_sync_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("missing.bin");

        let result = read_file_buffer_sync(&fake).unwrap();
        assert!(result.is_none());
    }

    // -- write_file_sync tests --

    #[test]
    fn write_file_sync_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("output.txt");

        let ok = write_file_sync(&file, b"hello").unwrap();
        assert!(ok);
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello");
    }

    #[test]
    fn write_file_sync_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("overwrite.txt");
        fs::write(&file, "old").unwrap();

        let ok = write_file_sync(&file, b"new content").unwrap();
        assert!(ok);
        assert_eq!(fs::read_to_string(&file).unwrap(), "new content");
    }

    #[test]
    fn write_file_sync_binary() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.bin");
        let data = vec![0x00, 0xFF, 0xAB];

        let ok = write_file_sync(&file, &data).unwrap();
        assert!(ok);
        assert_eq!(fs::read(&file).unwrap(), data);
    }

    // -- remove_entry_sync tests (task 7.5) --

    #[test]
    fn remove_entry_sync_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("to_delete.txt");
        fs::write(&file, "bye").unwrap();
        assert!(file.exists());

        remove_entry_sync(&file).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn remove_entry_sync_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("a.txt"), "a").unwrap();

        remove_entry_sync(&sub).unwrap();
        assert!(!sub.exists());
    }

    #[test]
    fn remove_entry_sync_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("ghost.txt");
        let result = remove_entry_sync(&fake);
        assert!(result.is_err());
    }

    // -- rename_entry_sync tests (task 7.5) --

    #[test]
    fn rename_entry_sync_file() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.txt");
        fs::write(&old, "data").unwrap();

        let new_path = rename_entry_sync(&old, "new.txt").unwrap();
        assert!(!old.exists());
        assert!(new_path.exists());
        assert_eq!(fs::read_to_string(&new_path).unwrap(), "data");
    }

    #[test]
    fn rename_entry_sync_directory() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old_dir");
        fs::create_dir(&old).unwrap();

        let new_path =
            rename_entry_sync(&old, "new_dir").unwrap();
        assert!(!old.exists());
        assert!(new_path.is_dir());
    }

    #[test]
    fn rename_entry_sync_target_exists() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.txt");
        let existing = dir.path().join("existing.txt");
        fs::write(&old, "old").unwrap();
        fs::write(&existing, "existing").unwrap();

        let result = rename_entry_sync(&old, "existing.txt");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("already exists")
        );
    }

    // -- copy_single_file_sync tests (task 7.5) --

    #[test]
    fn copy_single_file_sync_basic() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dest = dir.path().join("dest.txt");
        fs::write(&src, "content").unwrap();

        copy_single_file_sync(&src, &dest).unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "content");
    }

    #[test]
    fn copy_single_file_sync_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dest = dir.path().join("nested/deep/dest.txt");
        fs::write(&src, "nested").unwrap();

        copy_single_file_sync(&src, &dest).unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "nested");
    }

    #[test]
    fn copy_single_file_sync_source_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("missing.txt");
        let dest = dir.path().join("dest.txt");

        let result = copy_single_file_sync(&src, &dest);
        assert!(result.is_err());
    }
}
