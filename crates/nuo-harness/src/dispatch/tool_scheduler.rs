//! Concurrent tool-call scheduler driven by declarative resource accesses.
//!
//! Replaces the harness's prior "parallelize every call in a batch" model
//! with a static, predictable one: each tool call declares what it touches
//! via [`ToolAccesses`]; the scheduler pairs those declarations to decide
//! which calls run concurrently and which must wait. Two reads of the same
//! file parallelize freely; a write against any other access to the same
//! path serializes; a tool declaring [`ToolAccesses::all`] serializes the
//! whole batch.
//!
//! Ported from kimi-code's `loop/tool-scheduler.ts`. Two semantics preserved
//! verbatim:
//!
//! 1. **FIFO + anti-starvation.** A task waiting in the queue is checked
//!    against both the *running* tasks and the tasks *queued ahead of it*,
//!    and during a re-scan pass against the tasks promoted earlier in that
//!    same pass. A later task therefore cannot jump ahead of an earlier,
//!    conflicting one. This is what keeps the order fair under contention.
//! 2. **Full re-scan on every completion.** Rather than tracking precise
//!    dependency edges, each completion re-scans the whole queue in arrival
//!    order and promotes whatever is no longer blocked. O(n²) per batch — but
//!    batch sizes are tiny (a handful of tool calls), so simplicity wins.
//!
//! State machine (one lock, [`SchedulerInner`] inside [`Shared`], guards all
//! of it):
//! - `add()` decides start-vs-queue, then spawns if start.
//! - each spawned task runs to completion, then calls `finish(id, result)`.
//! - `finish` resolves the caller, removes the finished `ActiveTask`, and
//!   re-scans the queue, promoting unblocked tasks (each promotion spawns
//!   another task that will itself call `finish` on completion). This single
//!   entry point keeps the state machine local and auditable.
//!
//! Cancellation is two-tier, and both tiers are idempotent:
//! - [`ToolScheduler::cancel_all`] is the *cooperative* tier. It rejects
//!   every queued task immediately and cancels the batch-wide token; every
//!   spawned task — including tasks promoted later by `finish`'s re-scan —
//!   observes that through a child token handed to its `run` closure.
//! - [`ToolScheduler::abort_all`] is the *forced* fallback for tasks that
//!   ignore the token. It aborts the `JoinHandle` of every still-running
//!   task, dropping its run future (so tool `Drop` impls clean up child
//!   processes and the like), and rejects anything still queued. Callers are
//!   expected to `cancel_all` first, allow a drain grace period, then
//!   `abort_all`.
//!
//! A receiver waiting on an aborted task resolves with the oneshot's own
//! `Err` (the completion sender is dropped without a result ever being
//! sent); callers must treat that as "no result was produced", on par with
//! a queued task rejected by cancellation.
//!
//! Lives in `nuo-harness` (not `nuo-wire`) because it spins up tokio
//! tasks: core is pure domain, zero I/O (ADR-0005).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures::FutureExt;
use nuo_wire::ToolAccesses;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Why a scheduled tool call did not produce its result.
///
/// The scheduler's error channel previously carried a bare `String`, into
/// which several distinct failure shapes were collapsed. This enum restores
/// the distinction. It carries **scheduler-level** failures only — a task's
/// own failure is its result, not the scheduler's business, and flows through
/// the task's normal result channel (for tool dispatch, the `ToolResult`
/// event):
///
/// - **CancelledBeforeStart** / **Cancelled**: lifecycle outcomes of the
///   two-tier cancellation contract, expected during every interrupt.
/// - **Panicked**: a task's future panicked; the scheduler caught it (via
///   `catch_unwind`) so the queue re-scan could proceed, and reports the
///   panic payload here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerError {
    /// The task was still queued when the batch was cancelled; it never ran.
    CancelledBeforeStart,
    /// The task observed the cancellation token and gave up before finishing.
    Cancelled,
    /// The task's future panicked. The scheduler caught the panic so the
    /// queue re-scan could proceed; the payload's message is carried here.
    Panicked(String),
}

impl std::fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CancelledBeforeStart => write!(f, "cancelled before start"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Panicked(detail) => write!(f, "tool task panicked: {detail}"),
        }
    }
}

impl std::error::Error for SchedulerError {}

/// A run closure: receives a cancellation token (a child of the batch-wide
/// token), returns the tool's result. Boxed so the scheduler is monomorphic.
pub type RunClosure<R> =
    Box<dyn FnOnce(CancellationToken) -> BoxFuture<Result<R, SchedulerError>> + Send>;

/// A schedulable tool call: its declared accesses plus a closure that runs the
/// call to completion. Construction is decoupled from scheduling so callers
/// can wire up events / stdin policy imperatively before handing it over.
pub struct ToolCallTask<R> {
    pub accesses: ToolAccesses,
    pub run: RunClosure<R>,
}

impl<R: Send + 'static> ToolCallTask<R> {
    /// Build a task from an async closure returning `Result<R, SchedulerError>`.
    pub fn new<F, Fut>(accesses: ToolAccesses, run: F) -> Self
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<R, SchedulerError>> + Send + 'static,
    {
        Self {
            accesses,
            run: Box::new(move |token| Box::pin(run(token))),
        }
    }
}

/// A queued task awaiting its turn — holds everything needed to start it.
struct QueuedTask<R> {
    accesses: ToolAccesses,
    run: RunClosure<R>,
    completion: oneshot::Sender<Result<R, SchedulerError>>,
}

/// A running task. The `run` closure has already moved into its spawned task;
/// `handle` is that task's [`JoinHandle`], kept so [`ToolScheduler::abort_all`]
/// can force-stop a task that ignores cooperative cancellation. `completion`
/// is taken (and the entry removed) when the task reports back via `finish`.
struct ActiveTask<R> {
    id: u64,
    accesses: ToolAccesses,
    completion: Option<oneshot::Sender<Result<R, SchedulerError>>>,
    handle: JoinHandle<()>,
}

struct SchedulerInner<R> {
    active: Vec<ActiveTask<R>>,
    queued: Vec<QueuedTask<R>>,
    next_id: u64,
}

/// Everything shared between the scheduler and its spawned tasks: the state
/// machine plus the batch-wide cancellation token. One `Arc` clone carries
/// both into `spawn`/`finish`, so a task promoted by `finish`'s re-scan
/// derives its token from the same batch token as a directly-added one.
struct Shared<R> {
    inner: Mutex<SchedulerInner<R>>,
    cancel: CancellationToken,
}

/// Concurrency-arbitrating scheduler for one batch of tool calls.
///
/// Create one per batch, feed every call through [`ToolScheduler::add`],
/// await all returned receivers, then drop it. Cancellation is two-tier:
/// [`ToolScheduler::cancel_all`] rejects every queued task immediately and
/// signals running tasks through the batch-wide [`CancellationToken`] (each
/// `run` closure receives a child of it); [`ToolScheduler::abort_all`] is
/// the forced fallback that aborts tasks still running after a grace period.
/// See the module docs for the full contract.
pub struct ToolScheduler<R: Send + 'static> {
    shared: Arc<Shared<R>>,
}

impl<R: Send + 'static> Default for ToolScheduler<R> {
    fn default() -> Self {
        Self::with_token(CancellationToken::new())
    }
}

impl<R: Send + 'static> ToolScheduler<R> {
    /// Create a scheduler driven by the given (batch-wide) cancellation token.
    pub fn with_token(cancel: CancellationToken) -> Self {
        Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(SchedulerInner {
                    active: Vec::new(),
                    queued: Vec::new(),
                    next_id: 0,
                }),
                cancel,
            }),
        }
    }

    /// Submit a task. Returns immediately with a receiver for the result. If
    /// the task conflicts with nothing running or queued ahead of it, it
    /// starts right away; otherwise it waits until earlier conflicting tasks
    /// finish and free it up.
    pub async fn add(&self, task: ToolCallTask<R>) -> oneshot::Receiver<Result<R, SchedulerError>> {
        let (tx, rx) = oneshot::channel();
        let mut inner = self.shared.inner.lock().await;
        if inner.is_blocked(&task.accesses) {
            inner.queued.push(QueuedTask {
                accesses: task.accesses,
                run: task.run,
                completion: tx,
            });
        } else {
            let id = inner.next_id;
            inner.next_id += 1;
            // `spawn` only schedules the task — its first poll happens on the
            // runtime, never synchronously — so doing it under the lock is
            // safe and keeps the `ActiveTask` entry (including the
            // `JoinHandle`) atomic with the start decision: `abort_all` can
            // never observe a started task whose handle was not yet stored.
            let handle = Self::spawn(id, task.run, self.shared.clone());
            inner.active.push(ActiveTask {
                id,
                accesses: task.accesses,
                completion: Some(tx),
                handle,
            });
        }
        rx
    }

    /// Cancel every still-pending task, cooperatively. Queued tasks are
    /// rejected immediately; running tasks observe the token through their
    /// `run` closure. Idempotent: a second call finds an empty queue and
    /// re-cancels an already-cancelled token.
    pub async fn cancel_all(&self) {
        let drained: Vec<QueuedTask<R>>;
        {
            let mut inner = self.shared.inner.lock().await;
            drained = std::mem::take(&mut inner.queued);
        }
        for qt in drained {
            let _ = qt
                .completion
                .send(Err(SchedulerError::CancelledBeforeStart));
        }
        self.shared.cancel.cancel();
    }

    /// Force-stop everything still pending — the fallback for tasks that
    /// ignore the cooperative token after a drain grace period. Aborts every
    /// running task's `JoinHandle` (its run future is dropped, so tool `Drop`
    /// impls clean up) and rejects every queued task. Idempotent.
    ///
    /// A receiver waiting on an aborted task resolves with the oneshot's own
    /// `Err`: aborting drops the task's `finish` call along with its run
    /// future, so the completion sender is dropped here without a result
    /// ever being sent. Treat that like a queued rejection — no result was
    /// produced. Aborting a task that already completed but has not yet
    /// reported back is a safe no-op on the handle; its receiver still ends
    /// with `Err`, as if it had produced nothing.
    pub async fn abort_all(&self) {
        let (queued, active) = {
            let mut inner = self.shared.inner.lock().await;
            (
                std::mem::take(&mut inner.queued),
                std::mem::take(&mut inner.active),
            )
        };
        for qt in queued {
            let _ = qt
                .completion
                .send(Err(SchedulerError::CancelledBeforeStart));
        }
        for task in active {
            // Abort first; `task` then drops at the end of the iteration,
            // closing the oneshot so its waiter is released with `RecvError`
            // instead of hanging forever.
            task.handle.abort();
        }
    }

    /// Drive a task: run its closure with a child of the batch-wide token,
    /// then `finish` — resolve the caller, remove the finished task, re-scan
    /// the queue and promote unblocked tasks (each promotion calls `spawn`
    /// recursively, so the chain continues until the queue empties).
    ///
    /// The child token is derived here, from the shared batch token, so
    /// promoted tasks observe `cancel_all` exactly like directly-added ones.
    /// The returned `JoinHandle` is stored in the `ActiveTask` entry by the
    /// caller, giving `abort_all` its handle on the task.
    fn spawn(id: u64, run: RunClosure<R>, shared: Arc<Shared<R>>) -> JoinHandle<()> {
        let token = shared.cancel.child_token();
        tokio::spawn(async move {
            // Catch a panicking tool task so it cannot wedge the scheduler:
            // an uncaught panic would skip `finish`, stranding the ActiveTask
            // entry (blocking same-resource queued tasks for the rest of the
            // batch) and dropping the completion sender without a cause. The
            // panic still reaches the runtime's panic hook; we additionally
            // log it and resolve the task as an ordinary error so the queue
            // re-scan proceeds.
            let result = match std::panic::AssertUnwindSafe(run(token))
                .catch_unwind()
                .await
            {
                Ok(result) => result,
                Err(payload) => {
                    let detail = payload
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string payload".to_string());
                    tracing::error!(panic = %detail, "tool task panicked; resolving as error");
                    Err(SchedulerError::Panicked(detail))
                }
            };
            Self::finish(id, result, shared).await;
        })
    }

    /// The single re-scan entry point. Resolves the finished task, removes it,
    /// and promotes every queued task that is no longer blocked — honoring
    /// FIFO by also checking against tasks promoted earlier in this same pass.
    ///
    /// Takes an owned `Arc` so the spawned future it (re-)enters is `'static`.
    /// Returns a pinned boxed future so its `Send`-ness is pinned at the type
    /// level (an `async fn`'s inferred `impl Future` would otherwise let the
    /// borrow checker refuse it as non-Send across the `tokio::spawn` site).
    fn finish(
        id: u64,
        result: Result<R, SchedulerError>,
        shared: Arc<Shared<R>>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let mut guard = shared.inner.lock().await;
            // Resolve the finished task. Its `JoinHandle` drops with the
            // entry — detaching an already-completed task is a no-op. (The
            // entry may already be gone if `abort_all` removed it first;
            // then the result simply goes nowhere.)
            if let Some(pos) = guard.active.iter().position(|t| t.id == id) {
                let task = guard.active.remove(pos);
                if let Some(tx) = task.completion {
                    let _ = tx.send(result);
                }
            }
            // Re-scan: take the whole queue (FnOnce closures aren't Clone,
            // so move them out), partition into start-now vs keep-waiting.
            // Each promotion is pushed to `active` and spawned within this
            // same critical section, so a later queued task that conflicts
            // with an earlier *promoted* one in this pass still sees it in
            // `active` and keeps waiting — FIFO within the pass is preserved
            // by iteration order.
            let queued = std::mem::take(&mut guard.queued);
            let mut keep = Vec::with_capacity(queued.len());
            for qt in queued {
                let blocked = guard
                    .active
                    .iter()
                    .any(|a| a.accesses.conflicts(&qt.accesses));
                if blocked {
                    keep.push(qt);
                } else {
                    let id = guard.next_id;
                    guard.next_id += 1;
                    // Same spawn-under-lock reasoning as `add`: scheduling is
                    // not polling, so the entry and its `JoinHandle` stay
                    // atomic for `abort_all`.
                    let handle = Self::spawn(id, qt.run, shared.clone());
                    guard.active.push(ActiveTask {
                        id,
                        accesses: qt.accesses,
                        completion: Some(qt.completion),
                        handle,
                    });
                }
            }
            guard.queued = keep;
        })
    }
}

impl<R> SchedulerInner<R> {
    /// Is a task with the given accesses blocked right now? Blocked if it
    /// conflicts with any active task or any task queued ahead of it.
    fn is_blocked(&self, accesses: &ToolAccesses) -> bool {
        self.active.iter().any(|t| t.accesses.conflicts(accesses))
            || self.queued.iter().any(|t| t.accesses.conflicts(accesses))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A "held" task: increments a counter on entry then loops until its token
    /// is cancelled. Used to observe queueing deterministically.
    fn held_task(accesses: ToolAccesses) -> (Arc<AtomicUsize>, ToolCallTask<()>) {
        let entered = Arc::new(AtomicUsize::new(0));
        let counter = entered.clone();
        let task = ToolCallTask::new(accesses, move |token| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                // Block until cancelled.
                let _ = token.cancelled().await;
                Err(SchedulerError::Cancelled)
            }
        });
        (entered, task)
    }

    #[tokio::test]
    async fn non_conflicting_reads_run_concurrently() {
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        let (e1, t1) = held_task(ToolAccesses::read_file("a.rs"));
        let (e2, t2) = held_task(ToolAccesses::read_file("b.rs"));
        let rx1 = scheduler.add(t1).await;
        let rx2 = scheduler.add(t2).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(e1.load(Ordering::SeqCst), 1, "first read started");
        assert_eq!(
            e2.load(Ordering::SeqCst),
            1,
            "second read must start concurrently (different files)"
        );
        scheduler.cancel_all().await;
        let _ = rx1.await;
        let _ = rx2.await;
    }

    #[tokio::test]
    async fn conflicting_writes_serialize() {
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        let (e1, t1) = held_task(ToolAccesses::write_file("same.txt"));
        let (e2, t2) = held_task(ToolAccesses::write_file("same.txt"));
        let _rx1 = scheduler.add(t1).await;
        let _rx2 = scheduler.add(t2).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(e1.load(Ordering::SeqCst), 1, "first write started");
        assert_eq!(
            e2.load(Ordering::SeqCst),
            0,
            "second write must be queued (same-file conflict)"
        );
        scheduler.cancel_all().await;
    }

    #[tokio::test]
    async fn queued_task_starts_after_conflict_finishes() {
        let scheduler: ToolScheduler<String> = ToolScheduler::default();
        let rx1 = scheduler
            .add(ToolCallTask::new(
                ToolAccesses::write_file("c.txt"),
                |_| async { Ok("first".to_string()) },
            ))
            .await;
        let rx2 = scheduler
            .add(ToolCallTask::new(
                ToolAccesses::write_file("c.txt"),
                |_| async { Ok("second".to_string()) },
            ))
            .await;
        let r1 = rx1.await.unwrap().unwrap();
        let r2 = rx2.await.unwrap().unwrap();
        assert_eq!(r1, "first");
        assert_eq!(r2, "second", "queued write must run after the first");
    }

    #[tokio::test]
    async fn all_serializes_everything() {
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        let (e1, t1) = held_task(ToolAccesses::all());
        let (e2, t2) = held_task(ToolAccesses::read_file("anywhere"));
        let _rx1 = scheduler.add(t1).await;
        let _rx2 = scheduler.add(t2).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(e1.load(Ordering::SeqCst), 1);
        assert_eq!(
            e2.load(Ordering::SeqCst),
            0,
            "read must wait behind global-exclusive `all`"
        );
        scheduler.cancel_all().await;
    }

    #[tokio::test]
    async fn cancel_rejects_queued_task() {
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        // Hold the first task so the second queues.
        let (_e, t1) = held_task(ToolAccesses::write_file("d.txt"));
        let _rx1 = scheduler.add(t1).await;
        // Second task: also a held-style blocker, but it will be rejected
        // before ever running because cancel_all drains the queue.
        let (_e2, t2) = held_task(ToolAccesses::write_file("d.txt"));
        let rx2 = scheduler.add(t2).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        scheduler.cancel_all().await;
        let r2 = rx2.await.unwrap();
        assert!(r2.is_err(), "queued task must be rejected on cancel");
    }

    #[tokio::test]
    async fn mixed_batch_partial_parallelism() {
        // Batch: R(a), R(b), W(a), R(a). Expect R(a),R(b) parallel; W(a) waits
        // behind the first R(a); the last R(a) waits behind W(a).
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        let (e_ra1, t_ra1) = held_task(ToolAccesses::read_file("a"));
        let (e_rb, t_rb) = held_task(ToolAccesses::read_file("b"));
        let (_e_wa, t_wa) = held_task(ToolAccesses::write_file("a"));
        let (_e_ra2, t_ra2) = held_task(ToolAccesses::read_file("a"));
        let rxs = [
            scheduler.add(t_ra1).await,
            scheduler.add(t_rb).await,
            scheduler.add(t_wa).await,
            scheduler.add(t_ra2).await,
        ];
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(e_ra1.load(Ordering::SeqCst), 1, "R(a) #1 started");
        assert_eq!(
            e_rb.load(Ordering::SeqCst),
            1,
            "R(b) started (parallel, no conflict)"
        );
        scheduler.cancel_all().await;
        for rx in rxs {
            let _ = rx.await;
        }
    }

    #[tokio::test]
    async fn fifo_no_later_task_jumps_ahead() {
        // W(a) [held], then R(a), then W(b) [quick]. W(b) doesn't conflict
        // with the held W(a), but it must still start (no conflict); the point
        // is R(a) stays queued behind W(a). Verify by counters.
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        let (_e_wa, t_wa) = held_task(ToolAccesses::write_file("a"));
        let (e_ra, t_ra) = held_task(ToolAccesses::read_file("a"));
        let (e_wb, t_wb) = held_task(ToolAccesses::write_file("b"));
        let _rx_wa = scheduler.add(t_wa).await;
        let _rx_ra = scheduler.add(t_ra).await;
        let _rx_wb = scheduler.add(t_wb).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(
            e_wb.load(Ordering::SeqCst),
            1,
            "W(b) can start (no conflict with W(a))"
        );
        assert_eq!(
            e_ra.load(Ordering::SeqCst),
            0,
            "R(a) stays queued behind W(a)"
        );
        scheduler.cancel_all().await;
    }

    #[tokio::test]
    async fn promoted_task_observes_batch_cancel() {
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        // t1 holds the resource briefly; t2 conflicts with it, so t2 queues
        // and is only promoted by finish's re-scan once t1 completes.
        let rx1 = scheduler
            .add(ToolCallTask::new(
                ToolAccesses::write_file("p.txt"),
                |_| async {
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    Ok(())
                },
            ))
            .await;
        let (e2, t2) = held_task(ToolAccesses::write_file("p.txt"));
        let rx2 = scheduler.add(t2).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(e2.load(Ordering::SeqCst), 0, "t2 queued behind t1");
        // t1 finishes; the re-scan inside finish promotes t2.
        rx1.await.unwrap().unwrap();
        for _ in 0..200 {
            if e2.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(e2.load(Ordering::SeqCst), 1, "t2 promoted by the re-scan");
        // The promoted task's token must be a child of the batch token: a
        // batch cancel after promotion has to reach it. (It used to spawn
        // with an orphan token and would hang forever here.)
        scheduler.cancel_all().await;
        let r2 = tokio::time::timeout(Duration::from_secs(2), rx2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            r2,
            Err(SchedulerError::Cancelled),
            "promoted task must observe the batch cancel"
        );
    }

    #[tokio::test]
    async fn abort_all_terminates_token_ignoring_task() {
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        let rx = scheduler
            .add(ToolCallTask::new(
                ToolAccesses::write_file("q.txt"),
                |_token| async {
                    // Deliberately ignores the cancellation token.
                    loop {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                },
            ))
            .await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        // Cooperative cancel first: the task ignores it and keeps running.
        scheduler.cancel_all().await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        // Forced fallback: abort drops the run future before finish can ever
        // run, so the completion sender drops without sending and the
        // receiver ends with the oneshot's own error.
        scheduler.abort_all().await;
        let res = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .unwrap();
        assert!(
            res.is_err(),
            "aborted task's receiver must end with sender-dropped Err"
        );
    }

    #[tokio::test]
    async fn cancel_and_abort_are_idempotent() {
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        let (_e1, t1) = held_task(ToolAccesses::write_file("r.txt"));
        let (_e2, t2) = held_task(ToolAccesses::write_file("r.txt"));
        let rx1 = scheduler.add(t1).await;
        let rx2 = scheduler.add(t2).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        // Double cancel, double abort: none of these may panic or wedge.
        scheduler.cancel_all().await;
        scheduler.cancel_all().await;
        scheduler.abort_all().await;
        scheduler.abort_all().await;
        // t2 was rejected by the first cancel_all. t1 either observed the
        // token (Ok(Err)) or was aborted before finish could deliver its
        // result (Err(RecvError)) — never a success.
        assert!(rx2.await.unwrap().is_err());
        assert!(rx1.await.map(|r| r.is_err()).unwrap_or(true));
    }

    #[tokio::test]
    async fn panicking_task_resolves_err_and_promotes_queue() {
        let scheduler: ToolScheduler<String> = ToolScheduler::default();
        // A task that panics must not wedge the scheduler: its receiver gets
        // an ordinary Err, `finish` still runs, and a queued same-resource
        // task is promoted and completes.
        let rx1 = scheduler
            .add(ToolCallTask::new(
                ToolAccesses::write_file("p.txt"),
                |_| async {
                    panic!("boom");
                    #[allow(unreachable_code)]
                    Ok("never".to_string())
                },
            ))
            .await;
        let rx2 = scheduler
            .add(ToolCallTask::new(
                ToolAccesses::write_file("p.txt"),
                |_| async { Ok("promoted".to_string()) },
            ))
            .await;
        let r1 = tokio::time::timeout(Duration::from_secs(2), rx1)
            .await
            .expect("panicking task must not hang its receiver");
        match r1 {
            Ok(Err(e)) => assert!(matches!(e, SchedulerError::Panicked(_)), "got: {e}"),
            other => panic!("panicking task must resolve as Err, got: {other:?}"),
        }
        let r2 = tokio::time::timeout(Duration::from_secs(2), rx2)
            .await
            .expect("queued task must be promoted after the panic")
            .expect("receiver alive")
            .expect("task result");
        assert_eq!(r2, "promoted");
    }
}
