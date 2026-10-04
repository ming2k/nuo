//! The daemon's shutdown machinery (ADR-0101): one gate every trigger source
//! funnels into, a supervised task book, and the signal listeners that turn
//! OS signals into [`ShutdownReason`]s.
//!
//! Three invariants replace the pre-ADR-0101 "one hopefully-cooperative
//! `await` chain" model:
//!
//! 1. **Shutdown is a state transition, not a code path.** Signals, the
//!    `Shutdown` control verb, the idle-exit timer, and fatal startup errors
//!    all call [`ShutdownGate::request`]; the first reason wins, later ones
//!    are logged and ignored.
//! 2. **Every shutdown has a budget.** The graceful phases run under a
//!    deadline; when it expires — or a second signal arrives — the run loop
//!    escalates deterministically (abort remaining tasks, run RAII cleanup,
//!    exit) instead of hanging forever.
//! 3. **Every spawned task has an owner.** [`TaskBook`] tracks task names →
//!    handles so teardown can *confirm* exit (no more fire-and-forget racing
//!    the process end) and report exactly which task refused to stop.

use std::collections::HashMap;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Why the daemon is shutting down. The first reason wins; it is surfaced to
/// the operator in the exit log line so "why did my daemon stop" is always
/// answerable from the logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownReason {
    /// SIGINT (terminal Ctrl-C or an explicit `kill -INT`).
    SignalInterrupt,
    /// SIGTERM — the default for `kill <pid>`, systemd/docker stop, and every
    /// supervisor. Historically this bypassed graceful shutdown entirely
    /// (pre-ADR-0101); it now runs the same teardown as Ctrl-C.
    SignalTerminate,
    /// SIGHUP — terminal closed. A daemon spawned from an interactive shell
    /// gets this when the terminal goes away; draining beats dying.
    SignalHangup,
    /// The `Shutdown` control-plane verb (ADR-0100): a remote, scripted stop.
    ControlVerb,
    /// Idle-exit (ADR-0100 rule 3): zero hosted sessions and zero attached
    /// clients held for the configured grace period.
    IdleTimeout,
    /// A startup or runtime failure the daemon cannot survive (bind failure,
    /// a supervised core task panicking). The message surfaces in the exit
    /// log line.
    Fatal(String),
}

impl std::fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SignalInterrupt => f.write_str("interrupt (ctrl-c)"),
            Self::SignalTerminate => f.write_str("terminate (SIGTERM)"),
            Self::SignalHangup => f.write_str("hangup (SIGHUP)"),
            Self::ControlVerb => f.write_str("shutdown verb (control plane)"),
            Self::IdleTimeout => f.write_str("idle exit (no sessions, no clients)"),
            Self::Fatal(what) => write!(f, "fatal: {what}"),
        }
    }
}

/// Single shutdown trigger point: a cancellation gate plus the reason it
/// fired (first wins). Clone the gate into every subsystem that needs to
/// observe "the daemon is going down"; ask it why afterwards.
#[derive(Clone)]
pub struct ShutdownGate {
    cancel: CancellationToken,
    reason_tx: tokio::sync::watch::Sender<Option<ShutdownReason>>,
    reason_rx: tokio::sync::watch::Receiver<Option<ShutdownReason>>,
    /// Set when an escalation (second trigger) replaced the latched reason.
    /// Separate from the watch payload so `reason()` can keep reporting the
    /// *original* trigger for the exit line while `forced()` drives the
    /// run loop's phase decisions.
    forced: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The daemon build's version for handshake negotiation (ADR-0100).
    version: Option<String>,
}

impl ShutdownGate {
    pub fn new() -> Self {
        let (reason_tx, reason_rx) = tokio::sync::watch::channel(None);
        Self {
            cancel: CancellationToken::new(),
            reason_tx,
            reason_rx,
            forced: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            version: None,
        }
    }

    /// Attach this daemon build's version, reported to skewed clients in the
    /// handshake refusal (ADR-0100 rule 4). Set once by `host::run` before
    /// the listeners start accepting.
    pub fn with_version(self, version: impl Into<String>) -> Self {
        Self {
            version: Some(version.into()),
            ..self
        }
    }

    /// The daemon version this gate reports during handshake negotiation.
    pub fn version_of_daemon(&self) -> &str {
        self.version.as_deref().unwrap_or("unknown")
    }

    /// Request shutdown for `reason`. Idempotent: the first caller's reason
    /// wins and the gate latches; later calls only log. `escalate=true`
    /// (a second trigger while already draining) latches the *forced* flag
    /// ([`Self::forced`]) so the run loop abandons its graceful phases and
    /// jumps to the force path.
    pub fn request(&self, reason: ShutdownReason, escalate: bool) {
        let reason_text = reason.to_string();
        let latches = self
            .reason_tx
            .send_if_modified(|slot| match (slot.is_some(), escalate) {
                (false, _) => {
                    *slot = Some(reason.clone());
                    true
                }
                (true, true) => {
                    // Escalation: latch the forced flag below; keep the
                    // slot's original reason — the exit line names what
                    // *started* the drain, the force is how it ended.
                    true
                }
                (true, false) => false,
            });
        if escalate {
            self.forced
                .store(true, std::sync::atomic::Ordering::Release);
            tracing::warn!(
                reason = %reason_text,
                "shutdown: second trigger while draining — escalating to forced exit"
            );
        }
        if latches {
            tracing::info!(reason = %reason_text, "shutdown: requested");
            self.cancel.cancel();
        } else if !escalate {
            tracing::debug!(reason = %reason_text, "shutdown: already draining; trigger ignored");
        }
    }

    /// Whether the drain was escalated (a second trigger): the run loop
    /// checks this between its graceful phases and skips the rest when set.
    /// A `Fatal` reason is forced from the start.
    pub fn forced(&self) -> bool {
        self.forced.load(std::sync::atomic::Ordering::Acquire)
            || matches!(
                self.reason_rx.borrow().as_ref(),
                Some(ShutdownReason::Fatal(_))
            )
    }

    /// The cancellation token: cancelled as soon as the first reason lands.
    pub fn cancelled(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Wait until the gate is triggered.
    pub async fn triggered(&self) {
        self.cancel.cancelled().await;
    }

    /// The first recorded reason, if any.
    pub fn reason(&self) -> Option<ShutdownReason> {
        self.reason_rx.borrow().clone()
    }

    /// A receiver that yields when a reason is recorded.
    pub fn reason_rx(&self) -> tokio::sync::watch::Receiver<Option<ShutdownReason>> {
        self.reason_rx.clone()
    }
}

impl Default for ShutdownGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Named, supervised task handles. Spawned through the book so shutdown can
/// *confirm* exit task-by-task (instead of dropping the runtime under live
/// tasks) and name any task that refused to stop within the budget.
#[derive(Default)]
pub struct TaskBook {
    tasks: std::sync::Mutex<HashMap<String, JoinHandle<()>>>,
}

impl TaskBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a spawned task under `name`. If a task of that name already
    /// exists it is aborted and replaced (listeners rebinding, etc.).
    pub fn track(&self, name: impl Into<String>, handle: JoinHandle<()>) {
        let name = name.into();
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = tasks.insert(name, handle) {
            old.abort();
        }
    }

    /// Wait (up to `budget` total across all tasks) for every tracked task to
    /// finish. Tasks that finish are removed from the book; the returned map
    /// names the ones that did not, with why (`panicked` vs `still running`)
    /// — the survivors are put back in the book so [`Self::abort_all`] can
    /// still reach them.
    ///
    /// Poll-based on purpose: shutdown code must be trivially auditable, and
    /// a 10ms poll loop costs nothing against a budget measured in seconds.
    pub async fn join_all_with_budget(
        &self,
        budget: std::time::Duration,
    ) -> HashMap<String, TaskExit> {
        let deadline = tokio::time::Instant::now() + budget;
        let mut pending: Vec<(String, JoinHandle<()>)> = {
            let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            tasks.drain().collect()
        };
        loop {
            // Reap everything that has finished (await on a finished handle
            // returns immediately; a panic surfaces as a JoinError).
            let mut survivors = Vec::new();
            let mut hung = HashMap::new();
            for (name, handle) in pending {
                if handle.is_finished() {
                    match handle.await {
                        Err(e) if e.is_panic() => {
                            hung.insert(name, TaskExit::Panicked);
                        }
                        Err(_) => { /* cancelled: fine */ }
                        Ok(()) => {}
                    }
                } else {
                    survivors.push((name, handle));
                }
            }
            if survivors.is_empty() || tokio::time::Instant::now() >= deadline {
                // Put the survivors back so abort_all can reach them.
                let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
                for (name, handle) in survivors {
                    hung.insert(name.clone(), TaskExit::StillRunning);
                    tasks.insert(name, handle);
                }
                return hung;
            }
            pending = survivors;
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Abort every tracked task immediately (the force path).
    pub fn abort_all(&self) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        for (name, handle) in tasks.drain() {
            handle.abort();
            tracing::warn!(task = %name, "shutdown: aborted hung task");
        }
    }

    /// Number of live tracked tasks (diagnostics).
    pub fn len(&self) -> usize {
        self.tasks.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Deterministic drain seam for tests. Installed via
/// [`LifecycleOptions::drain_probe`](crate::host::LifecycleOptions) — never
/// in production — it parks the run loop after the drain is announced and
/// *before* the graceful phases run, so a test can land an escalation (or
/// observe the budget) at an exact point instead of racing wall-clock
/// sleeps against sub-millisecond phases.
///
/// Both directions are race-safe: `Notify` stores one permit, so a signal
/// sent before the other side starts waiting is not lost.
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct DrainProbe {
    parked: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl DrainProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run-loop side: announce that the drain is parked, then wait for the
    /// test's release.
    pub async fn wait_released(&self) {
        self.parked.notify_one();
        self.release.notified().await;
    }

    /// Test side: wait until the run loop has parked its drain.
    pub async fn parked(&self) {
        self.parked.notified().await;
    }

    /// Test side: let the parked drain resume.
    pub fn release(&self) {
        self.release.notify_one();
    }
}

/// Why a task did not exit cleanly within the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskExit {
    /// The task's future panicked.
    Panicked,
    /// The task ignored cancellation and was still running at the deadline.
    StillRunning,
}

impl std::fmt::Display for TaskExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Panicked => f.write_str("panicked"),
            Self::StillRunning => f.write_str("still running"),
        }
    }
}

/// Install the OS signal listeners: SIGINT/SIGTERM/SIGHUP on Unix (SIGHUP is
/// terminal death for a shell-spawned daemon), Ctrl-C alone on Windows.
/// Every signal funnels into `gate`; the second signal escalates. Returns a
/// handle the run loop drops to unregister the listeners (tests keep their
/// own trigger source instead).
pub struct SignalGuard {
    _keep: Vec<JoinHandle<()>>,
}

impl SignalGuard {
    pub fn install(gate: std::sync::Arc<ShutdownGate>) -> Self {
        let mut keep = Vec::new();
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            for (kind, reason) in [
                (SignalKind::interrupt(), ShutdownReason::SignalInterrupt),
                (SignalKind::terminate(), ShutdownReason::SignalTerminate),
                (SignalKind::hangup(), ShutdownReason::SignalHangup),
            ] {
                let Ok(stream) = signal(kind) else {
                    tracing::warn!("shutdown: could not install signal listener");
                    continue;
                };
                let gate = gate.clone();
                let mut stream = stream;
                keep.push(tokio::spawn(async move {
                    // First signal = request, subsequent = escalate. A
                    // signal handler that itself awaits a signal cannot miss
                    // one that arrives between requests: `stream.recv()`
                    // buffers.
                    loop {
                        stream.recv().await;
                        let already = gate.reason().is_some();
                        gate.request(reason.clone(), already);
                    }
                }));
            }
        }
        #[cfg(not(unix))]
        {
            let g = gate.clone();
            keep.push(tokio::spawn(async move {
                loop {
                    if tokio::signal::ctrl_c().await.is_ok() {
                        let already = g.reason().is_some();
                        g.request(ShutdownReason::SignalInterrupt, already);
                    }
                }
            }));
        }
        Self { _keep: keep }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_reason_wins_and_later_triggers_are_ignored() {
        let gate = ShutdownGate::new();
        gate.request(ShutdownReason::IdleTimeout, false);
        gate.request(ShutdownReason::ControlVerb, false);
        assert_eq!(gate.reason(), Some(ShutdownReason::IdleTimeout));
        assert!(gate.cancel.is_cancelled());
    }

    #[tokio::test]
    async fn escalation_latches_forced_and_keeps_the_original_reason() {
        let gate = ShutdownGate::new();
        assert!(!gate.forced());
        gate.request(ShutdownReason::SignalInterrupt, false);
        assert!(!gate.forced(), "a lone first trigger is not an escalation");
        gate.request(ShutdownReason::SignalTerminate, true);
        assert!(gate.forced(), "a second trigger while draining escalates");
        // The exit line still names the trigger that *started* the drain;
        // the force is only how it ended.
        assert_eq!(gate.reason(), Some(ShutdownReason::SignalInterrupt));
    }

    #[tokio::test]
    async fn triggered_resolves_after_request() {
        let gate = ShutdownGate::new();
        let g = gate.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            g.request(ShutdownReason::ControlVerb, false);
        });
        gate.triggered().await;
        assert_eq!(gate.reason(), Some(ShutdownReason::ControlVerb));
    }

    #[tokio::test]
    async fn task_book_joins_a_finishing_task_and_names_a_hung_one() {
        let book = TaskBook::new();
        book.track("quick", tokio::spawn(async {}));
        let gate = ShutdownGate::new();
        let g = gate.clone();
        book.track(
            "hung",
            tokio::spawn(async move {
                let _ = g; // hold it alive
                std::future::pending::<()>().await;
            }),
        );
        let hung = book
            .join_all_with_budget(std::time::Duration::from_millis(150))
            .await;
        assert_eq!(hung.get("hung"), Some(&TaskExit::StillRunning));
        assert!(!hung.contains_key("quick"));
        book.abort_all();
    }
}
