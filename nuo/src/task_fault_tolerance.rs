//! Task supervision primitives (the fix for the "fire-and-forget daemon" gap).
//!
//! Before this module, every long-lived task in the daemon — session drivers,
//! the monitor tap, round tasks, background refresh loops — was spawned with
//! its `JoinHandle` dropped. A panic in any of them killed the task silently:
//! the process survived, but a session could freeze as a zombie entry whose
//! request channel nobody drained, the monitor stream could go dark while the
//! driver kept burning tokens, and a round could wedge `RoundLifecycle` in
//! `Running` forever. `tool_scheduler` already had the right pattern (catch
//! the unwind, resolve it as an ordinary error); this module generalizes it.
//!
//! Three policies, one per task class:
//!
//! * **evict** — the task's death invalidates the state it owned. The session
//!   driver is the only example: after a driver panic nothing about the
//!   session is trustworthy, so the session is torn down through the standard
//!   [`SessionRegistry::kill_session`](crate::registry::SessionRegistry) path
//!   (clients then lazy-resume it on the next attach).
//! * **isolate** — the task must survive individual bad inputs. The monitor
//!   tap folds every agent response; a poison event costs one dropped frame,
//!   not the session's whole observability path.
//! * **restart** — the task is a self-contained loop whose job can simply be
//!   re-entered (accept loops, periodic refreshers). Bounded exponential
//!   backoff; a loop that keeps panicking escalates after
//!   `SUPERVISED_RESTART_LIMIT` attempts.

/// Extract a human-readable message from a panic payload.
pub(crate) fn panic_detail(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string payload".to_string())
}

/// Backoff schedule (milliseconds) between supervised restarts. Equal-jitter
/// is unnecessary here: restarts are rare, so the fixed schedule is fine.
///
/// Currently only exercised by tests in this module: the production restart
/// call sites live where the supervised loops are defined (e.g. the schedule
/// scheduler in `muta-agent`), which size their own tables. Kept here as
/// the shared reference schedule the daemon's supervision policy documents.
#[cfg(test)]
const SUPERVISED_RESTART_BACKOFF_MS: [u64; 4] = [250, 1_000, 4_000, 15_000];

/// After this many panics a supervised loop gives up and stops restarting.
/// The point is to make "background job died" visible, not to hot-restart a
/// fundamentally broken loop (which would spam the log).
#[cfg(test)]
pub(crate) const SUPERVISED_RESTART_LIMIT: usize = SUPERVISED_RESTART_BACKOFF_MS.len();

/// How long to wait between restarts after the backoff table is exhausted
/// (used while the restart budget lasts, see `SUPERVISED_RESTART_LIMIT`).
#[cfg(test)]
fn restart_backoff_ms(attempt: usize) -> u64 {
    // attempt is 0-based; saturate at the last (largest) entry.
    let idx = attempt.min(SUPERVISED_RESTART_BACKOFF_MS.len() - 1);
    SUPERVISED_RESTART_BACKOFF_MS[idx]
}

/// A restart outer shell around a `loop`-shaped future body. Runs `body`,
/// and if it panics, logs and restarts with backoff up to
/// `SUPERVISED_RESTART_LIMIT` times. Normal returns (`Ok`/`Err`/`()`) are
/// **not** restarted — only panics are: an ordinary return means the loop
/// finished its job or hit an error path it understands.
///
/// Currently exercised by the unit tests below; production call sites with
/// their own state to re-enter (the schedule scheduler) inline the same
/// pattern next to their loop. New background loops should prefer this
/// helper over re-inlining.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn supervise_loop<F, Fut>(name: &'static str, mut body: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    use futures::FutureExt;
    // Same schedule the tests below exercise; inlined here so the helper is
    // self-contained for future call sites.
    const BACKOFF_MS: [u64; 4] = [250, 1_000, 4_000, 15_000];
    const LIMIT: usize = BACKOFF_MS.len();
    let mut attempt = 0usize;
    loop {
        let panicked = std::panic::AssertUnwindSafe(body())
            .catch_unwind()
            .await
            .is_err();
        if !panicked {
            return;
        }
        if attempt >= LIMIT {
            tracing::error!(
                task = name,
                attempts = attempt,
                "supervised loop kept panicking; giving up"
            );
            return;
        }
        let backoff =
            std::time::Duration::from_millis(BACKOFF_MS[attempt.min(BACKOFF_MS.len() - 1)]);
        tracing::warn!(
            task = name,
            attempt,
            backoff_ms = backoff.as_millis() as u64,
            "supervised loop panicked; restarting with backoff"
        );
        attempt += 1;
        tokio::time::sleep(backoff).await;
    }
}

/// Install the daemon-wide panic hook: every panic is logged with its origin
/// before the default hook writes to stderr. Detached daemons have no
/// controlling terminal, so without this a task panic was invisible.
pub(crate) fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(
            location = ?info.location().map(|l| l.to_string()),
            "panic: {info}"
        );
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_detail_strings() {
        assert_eq!(panic_detail(Box::new("boom")), "boom");
        assert_eq!(panic_detail(Box::new("boom".to_string())), "boom");
        assert_eq!(panic_detail(Box::new(42usize)), "non-string payload");
    }

    #[test]
    fn backoff_is_monotonic_and_saturates() {
        let mut prev = 0;
        for a in 0..SUPERVISED_RESTART_LIMIT * 2 {
            let v = restart_backoff_ms(a);
            assert!(v >= prev);
            prev = v;
        }
        assert_eq!(
            restart_backoff_ms(100),
            *SUPERVISED_RESTART_BACKOFF_MS.last().unwrap()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn supervise_loop_restarts_on_panic_then_exits_on_clean_return() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let runs = std::sync::Arc::new(AtomicUsize::new(0));
        let runs2 = runs.clone();
        supervise_loop("test-loop", move || {
            let runs = runs2.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                if runs.load(Ordering::SeqCst) < 3 {
                    panic!("kaboom");
                }
            }
        })
        .await;
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn supervise_loop_gives_up_after_limit() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let runs = std::sync::Arc::new(AtomicUsize::new(0));
        let runs2 = runs.clone();
        supervise_loop("always-panics", move || {
            let runs = runs2.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                panic!("forever");
            }
        })
        .await;
        // body ran once per restart attempt plus the initial run, then stopped.
        assert_eq!(runs.load(Ordering::SeqCst), SUPERVISED_RESTART_LIMIT + 1);
    }
}
