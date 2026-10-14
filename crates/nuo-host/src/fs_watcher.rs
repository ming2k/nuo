//! Platform-agnostic, kernel-level reactive filesystem watcher (ADR-0165).
//!
//! Uses OS-native notification backends (Linux inotify, macOS FSEvents/kqueue,
//! Windows IOCP/ReadDirectoryChangesW) via [`notify`] with an integrated async
//! debouncing layer to produce high-signal, burst-coalesced filesystem change events.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{Event, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{broadcast, mpsc};

/// Semantic category of a detected filesystem change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FsEventKind {
    /// Files or directories created or modified.
    Modified,
    /// Files or directories deleted.
    Removed,
    /// Any other filesystem event.
    Any,
}

/// A debounced, platform-independent filesystem event.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FsEvent {
    /// Unique canonical or relative paths affected by this event batch.
    pub paths: Vec<PathBuf>,
    /// Coarse classification of the batch.
    pub kind: FsEventKind,
}

/// A platform-agnostic reactive filesystem watcher backed by OS kernel events.
pub struct FsWatcher {
    watcher: Box<dyn Watcher + Send>,
    watched_paths: Arc<Mutex<HashSet<PathBuf>>>,
    events_tx: broadcast::Sender<FsEvent>,
    _debounce_task: tokio::task::JoinHandle<()>,
}

impl FsWatcher {
    /// Default debounce window to coalesce rapid sequential filesystem writes (e.g. atomic editor saves).
    pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(200);

    /// Create a new filesystem watcher with the specified debounce interval.
    pub fn new(debounce: Duration) -> Result<Self, String> {
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<notify::Result<Event>>();
        let (events_tx, _) = broadcast::channel::<FsEvent>(64);
        let watched_paths = Arc::new(Mutex::new(HashSet::new()));

        let tx = raw_tx.clone();
        let watcher: Box<dyn Watcher + Send> = match RecommendedWatcher::new(
            move |res| {
                let _ = tx.send(res);
            },
            notify::Config::default(),
        ) {
            Ok(w) => Box::new(w),
            Err(e) => {
                tracing::warn!("kernel watcher unavailable ({e}), falling back to poll watcher");
                Box::new(
                    PollWatcher::new(
                        move |res| {
                            let _ = raw_tx.send(res);
                        },
                        notify::Config::default().with_poll_interval(debounce),
                    )
                    .map_err(|err| format!("failed to initialize filesystem watcher: {err}"))?,
                )
            }
        };

        let bcast_tx = events_tx.clone();
        let debounce_task = tokio::spawn(async move {
            let mut pending_paths: HashSet<PathBuf> = HashSet::new();
            let mut pending_is_removal = false;

            loop {
                tokio::select! {
                    Some(event_res) = raw_rx.recv() => {
                        if let Ok(event) = event_res {
                            use notify::EventKind;
                            // Pure access/read events (e.g. IN_ACCESS, IN_OPEN, IN_CLOSE_NOWRITE) must not
                            // be interpreted as mutations.
                            if matches!(event.kind, EventKind::Access(_)) {
                                continue;
                            }
                            let is_remove = matches!(event.kind, EventKind::Remove(_));
                            if is_remove {
                                pending_is_removal = true;
                            }
                            for path in event.paths {
                                pending_paths.insert(path);
                            }
                        }
                    }
                    _ = tokio::time::sleep(debounce), if !pending_paths.is_empty() => {
                        let paths: Vec<PathBuf> = pending_paths.drain().collect();
                        let kind = if pending_is_removal {
                            FsEventKind::Removed
                        } else {
                            FsEventKind::Modified
                        };
                        pending_is_removal = false;
                        let _ = bcast_tx.send(FsEvent { paths, kind });
                    }
                    else => {
                        // Channel closed and no pending paths
                        break;
                    }
                }
            }
        });

        Ok(Self {
            watcher,
            watched_paths,
            events_tx,
            _debounce_task: debounce_task,
        })
    }

    /// Add a path to the watched set.
    pub fn watch<P: AsRef<Path>>(&mut self, path: P, recursive: bool) -> Result<(), String> {
        let path = path.as_ref();
        let mode = if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };

        self.watcher
            .watch(path, mode)
            .map_err(|e| format!("failed to watch path '{}': {e}", path.display()))?;

        if let Ok(mut set) = self.watched_paths.lock() {
            set.insert(path.to_path_buf());
        }

        Ok(())
    }

    /// Watch a path only if it currently exists on the filesystem.
    pub fn watch_if_exists<P: AsRef<Path>>(
        &mut self,
        path: P,
        recursive: bool,
    ) -> Result<bool, String> {
        let path = path.as_ref();
        if path.exists() {
            self.watch(path, recursive)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Remove a path from the watched set.
    pub fn unwatch<P: AsRef<Path>>(&mut self, path: P) -> Result<(), String> {
        let path = path.as_ref();
        self.watcher
            .unwatch(path)
            .map_err(|e| format!("failed to unwatch path '{}': {e}", path.display()))?;

        if let Ok(mut set) = self.watched_paths.lock() {
            set.remove(path);
        }

        Ok(())
    }

    /// Subscribe to the stream of debounced filesystem events.
    pub fn subscribe(&self) -> broadcast::Receiver<FsEvent> {
        self.events_tx.subscribe()
    }

    /// Check if a path is currently registered for watching.
    pub fn is_watching<P: AsRef<Path>>(&self, path: P) -> bool {
        self.watched_paths
            .lock()
            .map(|set| set.contains(path.as_ref()))
            .unwrap_or(false)
    }
}

impl Drop for FsWatcher {
    fn drop(&mut self) {
        self._debounce_task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn watcher_lifecycle_and_debounce() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let watch_dir = tmp.path().join("watch_target");
        std::fs::create_dir_all(&watch_dir).unwrap();

        let mut watcher = FsWatcher::new(Duration::from_millis(50)).expect("create watcher");
        watcher.watch(&watch_dir, true).expect("watch dir");
        assert!(watcher.is_watching(&watch_dir));

        let mut rx = watcher.subscribe();

        // Write a test file
        let test_file = watch_dir.join("test.txt");
        std::fs::write(&test_file, b"hello").unwrap();

        // Wait for debounced event
        let event = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("timeout waiting for fs event")
            .expect("receive fs event");

        assert!(event.paths.iter().any(|p| p.ends_with("test.txt")));

        watcher.unwatch(&watch_dir).expect("unwatch dir");
        assert!(!watcher.is_watching(&watch_dir));
    }

    #[tokio::test]
    async fn watcher_ignores_file_reads() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let watch_dir = tmp.path().join("watch_read_target");
        std::fs::create_dir_all(&watch_dir).unwrap();
        let test_file = watch_dir.join("read_only.txt");
        std::fs::write(&test_file, b"sample content").unwrap();

        let mut watcher = FsWatcher::new(Duration::from_millis(50)).expect("create watcher");
        watcher.watch(&watch_dir, true).expect("watch dir");
        let mut rx = watcher.subscribe();

        // Perform file read
        let _ = std::fs::read_to_string(&test_file);

        // Wait to verify no event was generated by the read
        let result = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
        assert!(result.is_err(), "file reads must not generate fs events");
    }
}
