//! Tracking of live interactive and non-interactive connections.
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// How long a draining server waits, per connection, for the client to
/// complete the closing handshake after it is sent `Close(1001 GoingAway)`.
pub const DRAIN_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// The registry of open client connections. Handled uniformly so
/// draining and idle-exit inspect one table rather than each protocol having
/// its own tracking. Connection tasks carry a cancellation token stored here
/// so `drain()` can request an orderly disconnect (`Close(1001 GoingAway)`)
/// instead of vanishing under it. Connections remove themselves on exit.
#[derive(Clone)]
pub struct ConnTable {
    inner: Arc<std::sync::Mutex<ConnTableInner>>,
    notify: Arc<tokio::sync::Notify>,
}

impl Default for ConnTable {
    fn default() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(ConnTableInner::default())),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }
}

#[derive(Default)]
struct ConnTableInner {
    next_id: u64,
    conns: HashMap<u64, CancellationToken>,
    interactive_count: usize,
    has_had_interactive: bool,
}

pub struct InteractiveConnGuard {
    table: ConnTable,
}

impl Drop for InteractiveConnGuard {
    fn drop(&mut self) {
        self.table.unregister_interactive();
    }
}

impl ConnTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_interactive(&self) -> InteractiveConnGuard {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.interactive_count += 1;
            inner.has_had_interactive = true;
        }
        self.notify.notify_waiters();
        InteractiveConnGuard {
            table: self.clone(),
        }
    }

    pub fn unregister_interactive(&self) {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.interactive_count = inner.interactive_count.saturating_sub(1);
        }
        self.notify.notify_waiters();
    }

    pub fn interactive_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .interactive_count
    }

    pub fn has_had_interactive(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_had_interactive
    }

    pub async fn notified(&self) {
        self.notify.notified().await;
    }

    pub(crate) fn register(&self) -> (u64, CancellationToken) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.next_id += 1;
        let id = inner.next_id;
        let token = CancellationToken::new();
        inner.conns.insert(id, token.clone());
        (id, token)
    }

    pub(crate) fn unregister(&self, id: u64) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .conns
            .remove(&id);
    }

    /// Signal every live connection to close, and wait (bounded) for the
    /// connection tasks to observe it. Best-effort: a client that ignores
    /// the close frame is dropped when the process exits.
    pub async fn drain(&self) {
        let tokens: Vec<CancellationToken> = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.conns.values().cloned().collect()
        };
        for token in &tokens {
            token.cancel();
        }
        let deadline = tokio::time::Instant::now() + DRAIN_CLOSE_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            let remaining = {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.conns.len()
            };
            if remaining == 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        tracing::warn!(
            connections = self
                .inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .conns
                .len(),
            "serve: connections did not close within the drain window"
        );
    }

    /// Number of live connections (the idle-exit probe counts these).
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .conns
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
