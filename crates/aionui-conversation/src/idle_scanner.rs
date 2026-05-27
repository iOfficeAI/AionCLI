//! Background scanner that asks the conv layer for idle conversations
//! and cancels them. Runs until the shutdown watch fires.
//!
//! The decision logic ("Idle for ≥ N seconds AND ConvActor::Idle")
//! lives on `IConversationService::collect_idle`; cancellation flows
//! through `IConversationService::cancel_idle`, which resolves the
//! conversation owner internally so this background task does not
//! need an authenticated user identity.

use std::sync::Arc;
use std::time::Duration;

use aionui_common::ErrorChain;
use tracing::{debug, info, warn};

use crate::conv_service_trait::IConversationService;

/// Default idle timeout (5 minutes) — same default as the legacy
/// connect-layer scanner so operator-facing behaviour is unchanged.
const DEFAULT_IDLE_TIMEOUT_SECS: i64 = 5 * 60;

/// Scan interval (1 minute) — same default as the legacy connect-layer
/// scanner.
const SCAN_INTERVAL_SECS: u64 = 60;

/// Start the conv-layer idle scanner.
///
/// Returns a `JoinHandle` for the spawned task. The task polls every
/// `scan_interval_secs` and cancels conversations whose actor has been
/// `Idle` for longer than `idle_timeout_secs`. The watch channel
/// propagates graceful shutdown.
pub fn start_idle_scanner(
    service: Arc<dyn IConversationService>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    idle_timeout_secs: Option<i64>,
    scan_interval_secs: Option<u64>,
) -> tokio::task::JoinHandle<()> {
    let threshold_secs = idle_timeout_secs.unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS);
    // tokio::time::interval rejects a zero period, so clamp to 1 second to
    // keep the call infallible for callers that pass `Some(0)` from tests.
    let scan_interval = scan_interval_secs.unwrap_or(SCAN_INTERVAL_SECS).max(1);
    info!(
        threshold_secs,
        scan_interval_secs = scan_interval,
        "Starting conv-layer idle scanner"
    );

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(scan_interval));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let idle_ids = service.collect_idle(threshold_secs * 1000);
                    if idle_ids.is_empty() {
                        debug!("Idle scan: no idle conversations found");
                        continue;
                    }
                    info!(count = idle_ids.len(), "Idle scan: cancelling idle conversations");
                    for id in idle_ids {
                        if let Err(e) = service.cancel_idle(&id).await {
                            warn!(
                                conversation_id = %id,
                                error = %ErrorChain(&e),
                                "Failed to cancel idle conversation"
                            );
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!("Idle scanner received shutdown signal");
                        break;
                    }
                }
            }
        }
        info!("Idle scanner stopped");
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_api_types::{
        ConversationListResponse, ConversationResponse, CreateConversationRequest, ListConversationsQuery,
        SendMessageRequest,
    };
    use aionui_common::AppError;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::broadcast;

    use crate::conv_service_trait::{ConversationEvent, ConversationStatus};

    /// Minimal `IConversationService` stub that records every
    /// `cancel_idle` call so the scanner test can verify dispatch.
    struct StubService {
        idle_ids: Mutex<Vec<String>>,
        cancelled: Mutex<Vec<String>>,
        collect_calls: AtomicUsize,
    }

    impl StubService {
        fn new(idle_ids: Vec<String>) -> Self {
            Self {
                idle_ids: Mutex::new(idle_ids),
                cancelled: Mutex::new(Vec::new()),
                collect_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl IConversationService for StubService {
        async fn create(&self, _user_id: &str, _opts: CreateConversationRequest) -> Result<String, AppError> {
            unimplemented!()
        }
        async fn delete(&self, _user_id: &str, _id: &str) -> Result<(), AppError> {
            unimplemented!()
        }
        async fn get(&self, _user_id: &str, _id: &str) -> Result<ConversationResponse, AppError> {
            unimplemented!()
        }
        async fn list(&self, _user_id: &str, _q: ListConversationsQuery) -> Result<ConversationListResponse, AppError> {
            unimplemented!()
        }
        async fn warmup(&self, _user_id: &str, _id: &str) -> Result<(), AppError> {
            unimplemented!()
        }
        async fn send(&self, _user_id: &str, _id: &str, _req: SendMessageRequest) -> Result<String, AppError> {
            unimplemented!()
        }
        async fn cancel(&self, _user_id: &str, _id: &str) -> Result<(), AppError> {
            unimplemented!()
        }
        async fn cancel_idle(&self, id: &str) -> Result<(), AppError> {
            self.cancelled.lock().unwrap().push(id.to_owned());
            Ok(())
        }
        fn status(&self, _id: &str) -> ConversationStatus {
            ConversationStatus::Idle
        }
        fn subscribe(&self, _id: &str) -> broadcast::Receiver<ConversationEvent> {
            let (tx, rx) = broadcast::channel(1);
            drop(tx);
            rx
        }
        fn collect_idle(&self, _threshold_ms: i64) -> Vec<String> {
            self.collect_calls.fetch_add(1, Ordering::SeqCst);
            self.idle_ids.lock().unwrap().clone()
        }
    }

    #[tokio::test]
    async fn scanner_dispatches_cancel_idle_for_each_returned_id() {
        let stub = Arc::new(StubService::new(vec!["a".into(), "b".into()]));
        let svc: Arc<dyn IConversationService> = stub.clone();

        let (tx, rx) = tokio::sync::watch::channel(false);
        // Tight scan interval so the test does not need to wait a minute.
        let handle = start_idle_scanner(svc, rx, Some(1), Some(0));

        // First interval tick fires immediately; a brief sleep gives the
        // task a chance to run a cycle.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Trigger graceful shutdown and join.
        tx.send(true).unwrap();
        handle.await.unwrap();

        let cancelled = stub.cancelled.lock().unwrap().clone();
        assert!(cancelled.contains(&"a".to_owned()));
        assert!(cancelled.contains(&"b".to_owned()));
    }

    #[tokio::test]
    async fn scanner_exits_on_shutdown_without_running_when_no_idle() {
        let stub = Arc::new(StubService::new(Vec::new()));
        let svc: Arc<dyn IConversationService> = stub.clone();

        let (tx, rx) = tokio::sync::watch::channel(false);
        let handle = start_idle_scanner(svc, rx, Some(1), Some(0));
        tx.send(true).unwrap();
        handle.await.unwrap();

        // The scanner may have ticked zero or one time before observing
        // shutdown — we only assert that no cancel happened.
        assert!(stub.cancelled.lock().unwrap().is_empty());
    }
}
