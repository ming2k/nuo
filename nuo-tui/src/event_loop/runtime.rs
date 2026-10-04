//! Loop ↔ translator coordination facts (ADR-0197 M1).
//!
//! After the translator seam, the response listener and the monitor client
//! own **no** application state: they send
//! [`crate::event_loop::mutations::AppMutation`]s (see
//! `mutations.rs`) and the event loop — the sole `App` writer — applies
//! them. What remains here is *not* mirrored state. These are the few
//! cross-task coordination facts that cannot be a mutation because they flow
//! the other way (loop → translator), or because they are pure wakeup
//! machinery:
//!
//! - [`UiRuntime::viewed_session_id`] — the loop publishes the session the
//!   user is viewing each frame; the translator reads it to scope
//!   on-demand replies (e.g. `TokenUsageReport` routing) that must not
//!   populate the modal after a session switch raced them.
//! - [`UiRuntime::is_responding`] — the stream-coalescing hint. The
//!   translator reads it to decide whether a high-frequency stream update
//!   may wait for the 10 fps heartbeat instead of waking the loop per
//!   delta; both sides write it.
//! - [`UiRuntime::awaiting_oauth_add`] — mirror of `App::awaiting_oauth_add`
//!   so the translator can tell the OAuth add-flow (URL shown in the modal)
//!   from a reconnect (URL shown in the transcript) and avoid duplicating
//!   the URL into the transcript.
//! - [`UiRuntime::trust_gate_dismissed`] — per-run latch (ADR-0175): the
//!   daemon republishes `HarnessState` periodically; without the latch a
//!   dismissed trust gate would re-open on the next snapshot. The applier
//!   sets it when the gate is answered; the translator reads it to decide
//!   whether to republish a quarantined snapshot.
//! - [`UiRuntime::dirty`] / [`UiRuntime::dirty_notify`] — the redraw
//!   signal/wakeup pair: translators flip the flag and notify so the loop's
//!   `select!` wakes immediately; high-frequency stream updates rely on the
//!   loop's heartbeat instead.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;

pub(crate) fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

pub(crate) enum SideViewSignal {
    Opened { side_id: String },
    Closed,
}

pub struct UiRuntime {
    pub dirty: Arc<AtomicBool>,
    pub dirty_notify: Arc<tokio::sync::Notify>,
    /// Stream-coalescing hint (loop ↔ translator; see module docs).
    pub is_responding: Arc<AtomicBool>,
    /// Mirror of `App::awaiting_oauth_add` (loop → translator; see module docs).
    pub awaiting_oauth_add: Arc<AtomicBool>,
    /// Per-run trust-gate latch (ADR-0175; see module docs).
    pub trust_gate_dismissed: Arc<AtomicBool>,
    /// The session the user is currently viewing (loop → translator;
    /// see module docs).
    pub viewed_session_id: Arc<Mutex<Option<String>>>,
    /// The mutation channel's sink, reachable from loop-side spawns
    /// (dashboard control receipts) and handed to the translators.
    pub mutations: crate::event_loop::mutations::MutationSink,
}

impl UiRuntime {
    #[cfg(test)]
    pub fn minimal_for_test() -> Self {
        let (tx, _rx) = tokio::sync::mpsc::channel::<crate::event_loop::mutations::AppMutation>(16);
        Self {
            dirty: Arc::new(AtomicBool::new(false)),
            dirty_notify: Arc::new(tokio::sync::Notify::new()),
            is_responding: Arc::new(AtomicBool::new(false)),
            awaiting_oauth_add: Arc::new(AtomicBool::new(false)),
            trust_gate_dismissed: Arc::new(AtomicBool::new(false)),
            viewed_session_id: Arc::new(Mutex::new(None)),
            mutations: crate::event_loop::mutations::MutationSink::new(tx),
        }
    }
}
