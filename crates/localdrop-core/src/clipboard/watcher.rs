//! Clipboard change detection for live synchronization.
//!
//! This module provides polling-based clipboard change detection since
//! there's no universal cross-platform clipboard change notification API.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use super::{ClipboardAccess, ClipboardContent};

/// Default polling interval for clipboard changes.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Notification when clipboard content changes.
#[derive(Debug, Clone)]
pub struct ClipboardChange {
    /// The new clipboard content
    pub content: ClipboardContent,
    /// Hash of the content
    pub hash: u64,
    /// When the change was detected
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Watches for clipboard changes via polling.
pub struct ClipboardWatcher {
    /// Polling interval
    poll_interval: Duration,
    /// Last known content hash
    last_hash: Arc<AtomicU64>,
}

impl ClipboardWatcher {
    /// Create a new clipboard watcher with default poll interval.
    #[must_use]
    pub fn new() -> Self {
        Self {
            poll_interval: DEFAULT_POLL_INTERVAL,
            last_hash: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Create a new clipboard watcher with custom poll interval.
    #[must_use]
    pub fn with_interval(poll_interval: Duration) -> Self {
        Self {
            poll_interval,
            last_hash: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Update the last known hash (to prevent detecting our own writes as changes).
    pub fn set_last_hash(&self, hash: u64) {
        self.last_hash.store(hash, Ordering::SeqCst);
    }

    /// Get the last known hash.
    #[must_use]
    pub fn get_last_hash(&self) -> u64 {
        self.last_hash.load(Ordering::SeqCst)
    }

    /// Start watching for clipboard changes.
    ///
    /// Returns a channel receiver that will receive `ClipboardChange` events
    /// whenever the clipboard content changes.
    ///
    /// The watch task runs until the returned receiver is dropped.
    ///
    /// # Arguments
    ///
    /// * `clipboard` - The clipboard accessor to use
    ///
    /// # Returns
    ///
    /// A tuple of (receiver, abort_handle) where:
    /// - receiver: receives clipboard change events
    /// - abort_handle: can be used to stop the watcher
    pub fn start(
        &self,
        mut clipboard: Box<dyn ClipboardAccess>,
    ) -> (mpsc::Receiver<ClipboardChange>, WatcherHandle) {
        let (tx, rx) = mpsc::channel(16);
        let poll_interval = self.poll_interval;
        let last_hash = Arc::clone(&self.last_hash);
        let last_hash_for_handle = Arc::clone(&self.last_hash);

        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop_rx.recv() => {
                        tracing::debug!("Clipboard watcher stopping");
                        break;
                    }
                    () = tokio::time::sleep(poll_interval) => {
                        match clipboard.read() {
                            Ok(Some(content)) => {
                                let current_hash = content.hash();
                                let stored_hash = last_hash.load(Ordering::SeqCst);

                                tracing::trace!(
                                    "Clipboard poll: current_hash={}, stored_hash={}, type={:?}",
                                    current_hash, stored_hash, content.content_type()
                                );

                                if current_hash != 0 && current_hash != stored_hash {
                                    tracing::info!(
                                        "Clipboard change detected: {} -> {} ({:?}, {} bytes)",
                                        stored_hash, current_hash, content.content_type(), content.size()
                                    );

                                    last_hash.store(current_hash, Ordering::SeqCst);

                                    let change = ClipboardChange {
                                        content,
                                        hash: current_hash,
                                        timestamp: chrono::Utc::now(),
                                    };

                                    if tx.send(change).await.is_err() {
                                        tracing::debug!("Clipboard change receiver dropped");
                                        break;
                                    }
                                }
                            }
                            Ok(None) => {
                                tracing::trace!("Clipboard poll: empty clipboard");
                            }
                            Err(e) => {
                                tracing::warn!("Clipboard poll: read error: {}", e);
                            }
                        }
                    }
                }
            }
        });

        (
            rx,
            WatcherHandle {
                stop_tx,
                last_hash: last_hash_for_handle,
            },
        )
    }
}

impl Default for ClipboardWatcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle to stop the clipboard watcher and update its state.
pub struct WatcherHandle {
    stop_tx: mpsc::Sender<()>,
    /// Shared reference to the watcher's last known hash
    last_hash: Arc<AtomicU64>,
}

impl WatcherHandle {
    /// Stop the watcher.
    pub async fn stop(self) {
        let _ = self.stop_tx.send(()).await;
    }

    /// Update the watcher's last known hash.
    ///
    /// Call this BEFORE writing to the clipboard to prevent the watcher
    /// from detecting our own writes as changes.
    pub fn set_last_hash(&self, hash: u64) {
        self.last_hash.store(hash, Ordering::SeqCst);
    }

    /// Get a reference to the last_hash Arc for sharing with other tasks.
    ///
    /// This allows multiple tasks to update the watcher's hash state.
    #[must_use]
    pub fn last_hash_ref(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.last_hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Mock clipboard for testing
    struct MockClipboard {
        content: Arc<Mutex<Option<ClipboardContent>>>,
    }

    impl MockClipboard {
        fn new() -> Self {
            Self {
                content: Arc::new(Mutex::new(None)),
            }
        }

        #[allow(dead_code)]
        fn set_content(&self, content: ClipboardContent) {
            *self.content.lock().unwrap() = Some(content);
        }
    }

    impl ClipboardAccess for MockClipboard {
        fn read(&mut self) -> crate::Result<Option<ClipboardContent>> {
            Ok(self.content.lock().unwrap().clone())
        }

        fn write(&mut self, content: &ClipboardContent) -> crate::Result<()> {
            *self.content.lock().unwrap() = Some(content.clone());
            Ok(())
        }

        fn write_and_wait(
            &mut self,
            content: &ClipboardContent,
            _timeout: std::time::Duration,
        ) -> crate::Result<()> {
            self.write(content)
        }

        fn content_hash(&mut self) -> u64 {
            self.content
                .lock()
                .unwrap()
                .as_ref()
                .map_or(0, ClipboardContent::hash)
        }

        fn read_expected(
            &mut self,
            _expected: Option<crate::protocol::ClipboardContentType>,
        ) -> crate::Result<Option<ClipboardContent>> {
            self.read()
        }
    }

    #[test]
    fn test_watcher_creation() {
        let watcher = ClipboardWatcher::new();
        assert_eq!(watcher.get_last_hash(), 0);
    }

    #[test]
    fn test_watcher_set_hash() {
        let watcher = ClipboardWatcher::new();
        watcher.set_last_hash(12345);
        assert_eq!(watcher.get_last_hash(), 12345);
    }

    #[tokio::test]
    async fn test_watcher_detects_change() {
        let watcher = ClipboardWatcher::with_interval(Duration::from_millis(50));

        let mock = MockClipboard::new();
        let content_ref = Arc::clone(&mock.content);

        let (mut rx, handle) = watcher.start(Box::new(mock));

        {
            *content_ref.lock().unwrap() = Some(ClipboardContent::Text("Test content".to_string()));
        }

        let result = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;

        assert!(result.is_ok());
        let change = result.unwrap();
        assert!(change.is_some());

        if let Some(ClipboardChange { content, .. }) = change {
            if let ClipboardContent::Text(text) = content {
                assert_eq!(text, "Test content");
            } else {
                panic!("Expected text content");
            }
        }

        handle.stop().await;
    }

    #[tokio::test]
    async fn test_watcher_handle_set_last_hash() {
        let watcher = ClipboardWatcher::with_interval(Duration::from_millis(50));
        let mock = MockClipboard::new();
        let content_ref = Arc::clone(&mock.content);

        let (mut rx, handle) = watcher.start(Box::new(mock));

        {
            *content_ref.lock().unwrap() = Some(ClipboardContent::Text("New content".to_string()));
        }

        let expected_hash = ClipboardContent::Text("New content".to_string()).hash();
        handle.set_last_hash(expected_hash);

        let result = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
        assert!(
            result.is_err(),
            "Expected timeout - hash was pre-set so no change should be detected"
        );

        handle.stop().await;
    }

    #[tokio::test]
    async fn test_watcher_handle_last_hash_ref() {
        let watcher = ClipboardWatcher::with_interval(Duration::from_millis(50));
        let mock = MockClipboard::new();

        let (_rx, handle) = watcher.start(Box::new(mock));

        let hash_ref = handle.last_hash_ref();
        hash_ref.store(99999, std::sync::atomic::Ordering::SeqCst);

        assert_eq!(hash_ref.load(std::sync::atomic::Ordering::SeqCst), 99999);

        handle.set_last_hash(12345);

        assert_eq!(hash_ref.load(std::sync::atomic::Ordering::SeqCst), 12345);

        handle.stop().await;
    }
}
