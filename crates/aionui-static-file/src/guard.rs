use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

/// Result of an access check.
pub type GuardResult = Result<(), AccessDenied>;

/// Returned when the guard denies access.
#[derive(Debug, Clone)]
pub struct AccessDenied {
    pub reason: String,
}

impl AccessDenied {
    pub fn new(reason: impl Into<String>) -> Self {
        Self { reason: reason.into() }
    }
}

impl std::fmt::Display for AccessDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "access denied: {}", self.reason)
    }
}

impl std::error::Error for AccessDenied {}

/// Request context passed to the guard for access decisions.
///
/// Intentionally minimal — the guard implementor decides what fields
/// matter. Future versions can add fields without breaking existing guards.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    /// Authenticated user ID, if any.
    pub user_id: Option<String>,
    /// Conversation or session ID associated with this request.
    pub conversation_id: Option<String>,
}

/// A pluggable async function that decides whether a file request is allowed.
///
/// - Return `Ok(())` to allow the request.
/// - Return `Err(AccessDenied)` to reject it.
///
/// When no guard is configured, the static file service allows all requests.
pub type AccessGuardFn =
    Arc<dyn Fn(&RequestContext, &Path) -> Pin<Box<dyn Future<Output = GuardResult> + Send + 'static>> + Send + Sync>;

/// Helper to create an `AccessGuardFn` from any async function/closure.
///
/// # Example
/// ```
/// use aionui_static_file::guard::{make_guard, RequestContext, GuardResult, AccessDenied};
/// use std::path::Path;
///
/// let guard = make_guard(|ctx: &RequestContext, _path: &Path| {
///     let has_user = ctx.user_id.is_some();
///     async move {
///         if has_user {
///             Ok(())
///         } else {
///             Err(AccessDenied::new("unauthenticated"))
///         }
///     }
/// });
/// ```
pub fn make_guard<F, Fut>(f: F) -> AccessGuardFn
where
    F: Fn(&RequestContext, &Path) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = GuardResult> + Send + 'static,
{
    Arc::new(move |ctx, path| Box::pin(f(ctx, path)))
}
