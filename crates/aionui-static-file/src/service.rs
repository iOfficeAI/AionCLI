use std::path::{Path, PathBuf};

use aionui_common::path_safety::has_traversal;
use tokio::fs;
use tokio::io::AsyncReadExt;
use tracing::debug;

use crate::guard::{AccessDenied, AccessGuardFn, RequestContext};

/// Errors returned by [`StaticFileService`].
#[derive(Debug)]
pub enum ServeError {
    /// The requested path attempts directory traversal.
    Traversal(String),
    /// The guard rejected the request.
    Denied(AccessDenied),
    /// The file does not exist.
    NotFound(String),
    /// The file exceeds the configured size limit.
    TooLarge { size: u64, limit: u64 },
    /// An IO error occurred reading the file.
    Io(std::io::Error),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Traversal(p) => write!(f, "path traversal: {p}"),
            Self::Denied(d) => write!(f, "{d}"),
            Self::NotFound(p) => write!(f, "not found: {p}"),
            Self::TooLarge { size, limit } => {
                write!(f, "file too large: {size} bytes exceeds limit of {limit} bytes")
            }
            Self::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for ServeError {}

/// The result of a successful file resolution.
pub struct ServedFile {
    /// Canonical absolute path to the file on disk.
    pub path: PathBuf,
    /// MIME type string (e.g. "image/png").
    pub content_type: String,
    /// File size in bytes.
    pub size: u64,
    /// Last modified time (seconds since UNIX epoch), if available.
    pub last_modified: Option<u64>,
}

/// Maximum file size allowed for serving (256 MB).
const MAX_FILE_SIZE: u64 = 256 * 1024 * 1024;

/// Configuration for [`StaticFileService`].
#[derive(Default)]
pub struct StaticFileConfig {
    /// Optional access guard. When `None`, all requests are allowed.
    pub guard: Option<AccessGuardFn>,
    /// Maximum file size in bytes. Defaults to 256 MB.
    pub max_file_size: Option<u64>,
}

/// A stateless static file service with pluggable access control.
///
/// Resolves a (root, relative_path) pair into a validated file ready for
/// streaming. Does NOT perform HTTP-level concerns (headers, streaming) —
/// that is left to the caller (axum handler, etc.).
pub struct StaticFileService {
    config: StaticFileConfig,
}

impl StaticFileService {
    pub fn new(config: StaticFileConfig) -> Self {
        Self { config }
    }

    /// Create a service with no access guard (all requests allowed).
    pub fn permissive() -> Self {
        Self::new(StaticFileConfig::default())
    }

    /// Create a service with the given access guard.
    pub fn with_guard(guard: AccessGuardFn) -> Self {
        Self::new(StaticFileConfig {
            guard: Some(guard),
            ..Default::default()
        })
    }

    /// Resolve and validate a file request.
    ///
    /// 1. Checks the relative path for traversal patterns.
    /// 2. Canonicalizes and verifies the file stays within `root`.
    /// 3. Calls the access guard (if configured).
    /// 4. Returns file metadata for the caller to stream.
    pub async fn resolve(
        &self,
        root: &Path,
        relative_path: &str,
        context: &RequestContext,
    ) -> Result<ServedFile, ServeError> {
        // Fast pre-check for traversal
        if has_traversal(relative_path) {
            return Err(ServeError::Traversal(relative_path.to_owned()));
        }

        // Resolve absolute path
        let target = root.join(relative_path);
        let canonical = fs::canonicalize(&target)
            .await
            .map_err(|_| ServeError::NotFound(relative_path.to_owned()))?;

        // Canonicalize root to handle symlinks (e.g. macOS /var → /private/var)
        let canonical_root = fs::canonicalize(root).await.map_err(ServeError::Io)?;

        // Sandbox check
        if !canonical.starts_with(&canonical_root) {
            return Err(ServeError::Traversal(relative_path.to_owned()));
        }

        // Access guard
        if let Some(guard) = &self.config.guard {
            guard(context, &canonical).await.map_err(ServeError::Denied)?;
        }

        // File metadata
        let metadata = fs::metadata(&canonical)
            .await
            .map_err(|_| ServeError::NotFound(relative_path.to_owned()))?;

        if metadata.is_dir() {
            return Err(ServeError::NotFound(format!("{relative_path} is a directory")));
        }

        let size = metadata.len();
        let limit = self.config.max_file_size.unwrap_or(MAX_FILE_SIZE);
        if size > limit {
            return Err(ServeError::TooLarge { size, limit });
        }

        let last_modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());

        let content_type = mime_for_path(&canonical);

        debug!(path = %canonical.display(), content_type, size, "serving static file");

        Ok(ServedFile {
            path: canonical,
            content_type,
            size,
            last_modified,
        })
    }

    /// Open a file for streaming. Returns a tokio File handle.
    ///
    /// Call this after `resolve()` with the `ServedFile::path`.
    pub async fn open(&self, served: &ServedFile) -> Result<tokio::fs::File, ServeError> {
        fs::File::open(&served.path).await.map_err(ServeError::Io)
    }

    /// Open a file and seek to the given byte offset for range requests.
    /// Returns the file handle positioned at `start`.
    pub async fn open_range(&self, served: &ServedFile, start: u64) -> Result<tokio::fs::File, ServeError> {
        use tokio::io::AsyncSeekExt;
        let mut file = self.open(served).await?;
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(ServeError::Io)?;
        Ok(file)
    }

    /// Convenience: read the entire file into memory.
    /// Only use for small files. For large files, use `open()` + streaming.
    pub async fn read_all(&self, served: &ServedFile) -> Result<Vec<u8>, ServeError> {
        let mut file = self.open(served).await?;
        let mut buf = Vec::with_capacity(served.size as usize);
        file.read_to_end(&mut buf).await.map_err(ServeError::Io)?;
        Ok(buf)
    }
}

/// A parsed byte range from an HTTP Range header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    /// Content length for this range.
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_empty(&self) -> bool {
        self.end < self.start
    }

    /// Format as a Content-Range header value: `bytes start-end/total`.
    pub fn content_range_header(&self, total: u64) -> String {
        format!("bytes {}-{}/{}", self.start, self.end, total)
    }
}

/// Parse the first byte range from an HTTP Range header value.
///
/// Supports: `bytes=start-end`, `bytes=start-`, `bytes=-suffix`.
/// Returns `None` if the header is malformed or unsatisfiable.
pub fn parse_range(header_value: &str, file_size: u64) -> Option<ByteRange> {
    let s = header_value.strip_prefix("bytes=")?;
    let (start_str, end_str) = s.split_once('-')?;

    if start_str.is_empty() {
        // Suffix range: bytes=-500 means last 500 bytes
        let suffix: u64 = end_str.parse().ok()?;
        if suffix == 0 || suffix > file_size {
            return None;
        }
        let start = file_size - suffix;
        Some(ByteRange {
            start,
            end: file_size - 1,
        })
    } else {
        let start: u64 = start_str.parse().ok()?;
        if start >= file_size {
            return None;
        }
        let end = if end_str.is_empty() {
            file_size - 1
        } else {
            let e: u64 = end_str.parse().ok()?;
            e.min(file_size - 1)
        };
        if end < start {
            return None;
        }
        Some(ByteRange { start, end })
    }
}

/// Determine MIME type from file extension.
fn mime_for_path(path: &Path) -> String {
    mime_guess::from_path(path).first_or_octet_stream().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::{AccessDenied, make_guard};
    use std::fs as stdfs;

    #[tokio::test]
    async fn resolve_simple_file() {
        let dir = tempfile::tempdir().unwrap();
        stdfs::write(dir.path().join("hello.txt"), "world").unwrap();

        let svc = StaticFileService::permissive();
        let ctx = RequestContext::default();
        let result = svc.resolve(dir.path(), "hello.txt", &ctx).await;
        assert!(result.is_ok());
        let served = result.unwrap();
        assert_eq!(served.content_type, "text/plain");
        assert_eq!(served.size, 5);
    }

    #[tokio::test]
    async fn resolve_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        stdfs::write(dir.path().join("secret.txt"), "x").unwrap();

        let svc = StaticFileService::permissive();
        let ctx = RequestContext::default();
        let result = svc.resolve(dir.path(), "../etc/passwd", &ctx).await;
        assert!(matches!(result, Err(ServeError::Traversal(_))));
    }

    #[tokio::test]
    async fn resolve_rejects_symlink_escape() {
        let sandbox = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        stdfs::write(&secret, "secret").unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, sandbox.path().join("escape")).unwrap();
        #[cfg(not(unix))]
        return;

        let svc = StaticFileService::permissive();
        let ctx = RequestContext::default();
        let result = svc.resolve(sandbox.path(), "escape", &ctx).await;
        assert!(matches!(result, Err(ServeError::Traversal(_))));
    }

    #[tokio::test]
    async fn resolve_not_found() {
        let dir = tempfile::tempdir().unwrap();

        let svc = StaticFileService::permissive();
        let ctx = RequestContext::default();
        let result = svc.resolve(dir.path(), "nope.txt", &ctx).await;
        assert!(matches!(result, Err(ServeError::NotFound(_))));
    }

    #[tokio::test]
    async fn resolve_directory_rejected() {
        let dir = tempfile::tempdir().unwrap();
        stdfs::create_dir(dir.path().join("subdir")).unwrap();

        let svc = StaticFileService::permissive();
        let ctx = RequestContext::default();
        let result = svc.resolve(dir.path(), "subdir", &ctx).await;
        assert!(matches!(result, Err(ServeError::NotFound(_))));
    }

    #[tokio::test]
    async fn guard_allows() {
        let dir = tempfile::tempdir().unwrap();
        stdfs::write(dir.path().join("ok.txt"), "yes").unwrap();

        let guard = make_guard(|_ctx: &RequestContext, _path: &Path| async { Ok(()) });
        let svc = StaticFileService::with_guard(guard);
        let ctx = RequestContext::default();
        let result = svc.resolve(dir.path(), "ok.txt", &ctx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn guard_denies() {
        let dir = tempfile::tempdir().unwrap();
        stdfs::write(dir.path().join("denied.txt"), "no").unwrap();

        let guard = make_guard(|_ctx: &RequestContext, _path: &Path| async { Err(AccessDenied::new("nope")) });
        let svc = StaticFileService::with_guard(guard);
        let ctx = RequestContext::default();
        let result = svc.resolve(dir.path(), "denied.txt", &ctx).await;
        assert!(matches!(result, Err(ServeError::Denied(_))));
    }

    #[tokio::test]
    async fn guard_with_user_context() {
        let dir = tempfile::tempdir().unwrap();
        stdfs::write(dir.path().join("file.txt"), "data").unwrap();

        let guard = make_guard(|ctx: &RequestContext, _path: &Path| {
            let allowed = ctx.user_id.as_deref() == Some("user-1");
            async move {
                if allowed {
                    Ok(())
                } else {
                    Err(AccessDenied::new("wrong user"))
                }
            }
        });
        let svc = StaticFileService::with_guard(guard);

        let good_ctx = RequestContext {
            user_id: Some("user-1".into()),
            ..Default::default()
        };
        assert!(svc.resolve(dir.path(), "file.txt", &good_ctx).await.is_ok());

        let bad_ctx = RequestContext {
            user_id: Some("user-2".into()),
            ..Default::default()
        };
        assert!(svc.resolve(dir.path(), "file.txt", &bad_ctx).await.is_err());
    }

    #[tokio::test]
    async fn mime_detection() {
        let dir = tempfile::tempdir().unwrap();
        stdfs::write(dir.path().join("image.png"), &[0x89, 0x50]).unwrap();
        stdfs::write(dir.path().join("page.html"), "<html>").unwrap();
        stdfs::write(dir.path().join("data.json"), "{}").unwrap();
        stdfs::write(dir.path().join("style.css"), "body{}").unwrap();

        let svc = StaticFileService::permissive();
        let ctx = RequestContext::default();

        let png = svc.resolve(dir.path(), "image.png", &ctx).await.unwrap();
        assert_eq!(png.content_type, "image/png");

        let html = svc.resolve(dir.path(), "page.html", &ctx).await.unwrap();
        assert_eq!(html.content_type, "text/html");

        let json = svc.resolve(dir.path(), "data.json", &ctx).await.unwrap();
        assert_eq!(json.content_type, "application/json");

        let css = svc.resolve(dir.path(), "style.css", &ctx).await.unwrap();
        assert_eq!(css.content_type, "text/css");
    }

    #[tokio::test]
    async fn read_all_works() {
        let dir = tempfile::tempdir().unwrap();
        stdfs::write(dir.path().join("data.bin"), b"hello world").unwrap();

        let svc = StaticFileService::permissive();
        let ctx = RequestContext::default();
        let served = svc.resolve(dir.path(), "data.bin", &ctx).await.unwrap();
        let data = svc.read_all(&served).await.unwrap();
        assert_eq!(data, b"hello world");
    }

    #[tokio::test]
    async fn open_range_seeks_correctly() {
        let dir = tempfile::tempdir().unwrap();
        stdfs::write(dir.path().join("data.bin"), b"0123456789").unwrap();

        let svc = StaticFileService::permissive();
        let ctx = RequestContext::default();
        let served = svc.resolve(dir.path(), "data.bin", &ctx).await.unwrap();
        let mut file = svc.open_range(&served, 5).await.unwrap();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"56789");
    }

    #[test]
    fn parse_range_start_end() {
        let r = parse_range("bytes=0-499", 1000).unwrap();
        assert_eq!(r.start, 0);
        assert_eq!(r.end, 499);
        assert_eq!(r.len(), 500);
    }

    #[test]
    fn parse_range_start_only() {
        let r = parse_range("bytes=500-", 1000).unwrap();
        assert_eq!(r.start, 500);
        assert_eq!(r.end, 999);
        assert_eq!(r.len(), 500);
    }

    #[test]
    fn parse_range_suffix() {
        let r = parse_range("bytes=-200", 1000).unwrap();
        assert_eq!(r.start, 800);
        assert_eq!(r.end, 999);
        assert_eq!(r.len(), 200);
    }

    #[test]
    fn parse_range_clamps_end() {
        let r = parse_range("bytes=900-2000", 1000).unwrap();
        assert_eq!(r.start, 900);
        assert_eq!(r.end, 999);
    }

    #[test]
    fn parse_range_unsatisfiable() {
        assert!(parse_range("bytes=1000-", 1000).is_none());
        assert!(parse_range("bytes=-0", 1000).is_none());
        assert!(parse_range("invalid", 1000).is_none());
        assert!(parse_range("bytes=500-400", 1000).is_none());
    }

    #[test]
    fn byte_range_content_range_header() {
        let r = ByteRange { start: 0, end: 499 };
        assert_eq!(r.content_range_header(1000), "bytes 0-499/1000");
    }
}
