//! The write-ahead journal middleware: the one place a tool call is bracketed by
//! capture and completion (ADR-0125).
//!
//! ## Why middleware and not a per-tool hook
//!
//! A tool must not be able to journal itself. If it did, the journal would be a
//! *self-report* rather than an *observation*, and the one property that makes it
//! trustworthy — that a tool cannot claim to have changed less than it did —
//! would be lost. Bracketing every call from outside means the capture reads the
//! filesystem the tool is about to touch, before and after, regardless of what the
//! tool says about itself.
//!
//! ## The bracket
//!
//! ```text
//!   begin(call)  →  capture pre-images, append write-ahead rows
//!                        │  (failure ⇒ the call must not run)
//!                        ▼
//!                    tool executes
//!                        │
//!                        ▼
//!   finish(ids)  →  capture post-images, complete rows
//! ```
//!
//! [`begin`] returning `Err` is a **veto**, not a warning: an uncaptured mutation
//! would be silently irreversible, which is exactly the failure mode the journal
//! exists to prevent. The dispatch path turns the error into a terminal tool
//! result and never invokes the tool.
//!
//! ## Non-reversible calls still get a receipt
//!
//! A shell command, a network fetch, or a process signal is journaled as a
//! completed row with its class and irreversibility reason and no pre-image. That
//! receipt is what `/diff` shows under "cannot undo" and what `/undo` refuses with
//! a real explanation instead of silently omitting the call. Recording only the
//! reversible calls would make `/diff` a list of pleasing news.

use std::path::Path;

use nuo_protocol::Tool;
use nuo_tool::{EffectClass, ToolAccesses};

use super::{
    CaptureOutcome, CapturedPath, JournalOpToken, JournalRow, PreImageStore, WorkspaceJournalPort,
    complete, prepare,
};

/// Mirror the harness's monotonic clock into epoch milliseconds.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The journal ids written by [`begin`], waiting to be completed.
///
/// Holding these rather than the [`CapturedPath`]s alone is what ties the
/// post-image to the row it completes: the middleware cannot accidentally complete
/// a row another call created.
#[derive(Debug, Clone)]
pub struct PendingWrite {
    /// `(journal id, captured path)` in declaration order. Empty for a call that
    /// produced no write-ahead row (a non-reversible receipt, or a read).
    pub rows: Vec<(i64, CapturedPath)>,
    /// The class this call was journaled as.
    pub effect: EffectClass,
    /// Non-reversible receipts that must still be completed (they were appended
    /// pre-completed, so this is empty for them) — reserved for a future class
    /// that needs a post-image without a pre-image.
    pub receipt_only: bool,
}

impl PendingWrite {
    /// Nothing journaled: a pure read, or a call with no thread owner.
    pub fn skipped() -> Self {
        Self {
            rows: Vec::new(),
            effect: EffectClass::None,
            receipt_only: false,
        }
    }

    /// Whether anything was journaled, so `finish` can skip the work entirely for
    /// pure reads (the overwhelming majority of calls).
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && !self.receipt_only
    }
}

impl Default for PendingWrite {
    fn default() -> Self {
        Self::skipped()
    }
}

/// What identifying coordinates one journaled call needs.
#[derive(Debug, Clone)]
pub struct CallCoordinates<'a> {
    /// Owning thread. Empty means the agent has no thread yet (a subagent before
    /// attachment, a test), in which case journaling is skipped: a row with no
    /// owner cannot be found by `/diff` or `/undo` later.
    pub thread_id: &'a str,
    pub round_id: &'a str,
    pub turn_id: &'a str,
    pub call_id: &'a str,
    /// The next journal seq for this thread.
    pub seq: i64,
}

/// Capture `tool`'s effects and append the write-ahead rows.
///
/// Returns `Ok(PendingWrite::default())` — an empty, skipped write — for the two
/// cases that must not journal: a call with no thread owner, and a pure read. A
/// capture or append failure is an error the dispatch path converts into a veto.
pub async fn begin(
    journal: &dyn WorkspaceJournalPort,
    workspace_root: Option<&Path>,
    coordinates: CallCoordinates<'_>,
    tool: &dyn Tool,
    arguments: &str,
) -> Result<PendingWrite, String> {
    // A row with no owner can never be found again; journaling it would consume
    // storage and CAS entries for a record nothing can read.
    if coordinates.thread_id.is_empty() {
        return Ok(PendingWrite::default());
    }
    // Fast path: a tool that declares no effect is a read. This is the common
    // case and must not touch the CAS or the writer.
    let ceiling = tool.effect_class();
    if ceiling == EffectClass::None && !tool.accesses(arguments).declares_write() {
        return Ok(PendingWrite::default());
    }

    let accesses = tool.accesses(arguments);
    let store = journal.store();

    // Non-reversible: a receipt, not a recovery point. Appended already complete
    // because there is no mid-call window to protect against.
    if ceiling != EffectClass::FileBytes {
        let row = receipt_row(&coordinates, tool.name(), ceiling, &accesses);
        // Errors here are logged, not fatal: failing a shell command because its
        // receipt could not be recorded would turn an observability gap into an
        // outage. The `Opaque` class already told the user this call is not
        // recoverable, so a missing receipt does not weaken a promise.
        match journal.append_receipt(row, now_ms()).await {
            Ok(_) => tracing::debug!(
                tool = %tool.name(),
                effect = ceiling.as_str(),
                "journaled non-reversible effect receipt"
            ),
            Err(error) => tracing::warn!(
                tool = %tool.name(),
                %error,
                "could not journal effect receipt; the call proceeds"
            ),
        }
        return Ok(PendingWrite {
            effect: ceiling,
            ..PendingWrite::default()
        });
    }

    // File-reversible: capture, then append each row. A failure at any step is a
    // veto.
    let root = workspace_root.unwrap_or_else(|| Path::new("."));
    let capture = prepare(root, ceiling, &accesses, store)?;
    if capture.effect != EffectClass::FileBytes || capture.paths.is_empty() {
        // Capture itself decided this is not reversible after all (e.g. a
        // recursive write claim). Journal it as a receipt at the reconciled class.
        let row = receipt_row(&coordinates, tool.name(), capture.effect, &accesses);
        let _ = journal.append_receipt(row, now_ms()).await;
        return Ok(PendingWrite {
            effect: capture.effect,
            ..PendingWrite::default()
        });
    }

    let mut rows = Vec::with_capacity(capture.paths.len());
    for (offset, captured) in capture.paths.iter().enumerate() {
        // A withheld capture is journaled as a *receipt at a reversible class's
        // path*: the row records that this call changed this file, and carries the
        // reason it cannot be restored. Downgrading the class to `opaque` would be
        // wrong (a file write is not arbitrary execution) and dropping the row would
        // hide the change entirely — both worse than a `file_bytes` row that explains
        // itself. `undoable_effects` filters on `irreversible_reason IS NULL`, so such
        // a row is never offered for reversal.
        let withheld_reason = captured.capture.withheld_reason();
        let row = JournalRow {
            thread_id: coordinates.thread_id.to_string(),
            round_id: coordinates.round_id.to_string(),
            turn_id: coordinates.turn_id.to_string(),
            call_id: coordinates.call_id.to_string(),
            seq: coordinates.seq + offset as i64,
            tool_name: tool.name().to_string(),
            path: Some(captured.path.to_string_lossy().into_owned()),
            op: match captured.op {
                super::CapturedOp::Create => JournalOpToken::Create,
                super::CapturedOp::Edit => JournalOpToken::Edit,
            },
            effect_class: EffectClass::FileBytes.as_str().to_string(),
            before_hash: captured.before_hash.clone(),
            irreversible_reason: withheld_reason.map(str::to_string),
        };
        // Write-ahead, always — including for a *withheld* pre-image. The row is
        // incomplete until `finish` observes the post-state, because the mutation has
        // not run yet. Deciding otherwise from `before_hash.is_none()` would mis-read
        // a refused capture as "no recovery point" and stamp completion before the
        // write happened.
        let id = journal.append(row).await?;
        rows.push((id, captured.clone()));
    }

    Ok(PendingWrite {
        rows,
        effect: capture.effect,
        receipt_only: false,
    })
}

/// Observe post-images and complete the write-ahead rows.
///
/// Called after the tool returns, **including** when it failed: a tool that
/// errored halfway may still have written bytes, and a row left uncompleted would
/// be permanently un-undoable. Completion records what is actually on disk, which
/// is true whether or not the call succeeded.
///
/// Failure here is logged rather than propagated: the mutation already happened,
/// so failing the tool result would not un-happen it, and the incomplete row
/// remains honest ("may or may not have completed") for a later `/diff`.
pub async fn finish(journal: &dyn WorkspaceJournalPort, pending: PendingWrite) {
    if pending.rows.is_empty() {
        return;
    }
    let captures: Vec<CapturedPath> = pending.rows.iter().map(|(_, p)| p.clone()).collect();
    let observed = match complete(&captures, journal.store()) {
        Ok(observed) => observed,
        Err(error) => {
            tracing::warn!(
                %error,
                "could not capture post-images; the journal rows stay unconfirmed"
            );
            return;
        }
    };
    // `complete` returns observations in capture order; zip them against the row
    // ids the same way, so id and path cannot be mismatched.
    for ((id, captured), (path, after_hash)) in pending.rows.iter().zip(observed) {
        debug_assert_eq!(
            &captured.path, &path,
            "completion observations must stay aligned with their rows"
        );
        if let Err(error) = journal.complete(*id, after_hash, now_ms()).await {
            tracing::warn!(
                effect_id = *id,
                %error,
                "could not complete a journal row; it stays unconfirmed"
            );
        }
    }
}

/// Build a receipt row for a non-reversible call.
///
/// The path is `None` unless the call named exactly one, because a receipt for an
/// unbounded effect (a shell command, a network call) has no single file to point
/// at and inventing one would make `/diff` claim a change it cannot describe.
fn receipt_row(
    coordinates: &CallCoordinates<'_>,
    tool_name: &str,
    class: EffectClass,
    accesses: &ToolAccesses,
) -> JournalRow {
    let single_path = accesses.single_file_path();
    JournalRow {
        thread_id: coordinates.thread_id.to_string(),
        round_id: coordinates.round_id.to_string(),
        turn_id: coordinates.turn_id.to_string(),
        call_id: coordinates.call_id.to_string(),
        seq: coordinates.seq,
        tool_name: tool_name.to_string(),
        path: single_path,
        op: JournalOpToken::Edit,
        effect_class: class.as_str().to_string(),
        before_hash: None,
        irreversible_reason: class.irreversibility_reason().map(str::to_string),
    }
}

/// Whether `capture` produced anything that must be completed.
///
/// Exposed so the dispatch path can skip the `finish` call entirely for the common
/// read case without constructing a `PendingWrite`.
pub fn capture_produced_rows(capture: &CaptureOutcome) -> bool {
    !capture.paths.is_empty()
}

/// A [`PreImageStore`] adapter over any `Fn`-shaped store, for tests and for
/// frontends that keep their own CAS.
pub struct FnStore<Put, Get> {
    pub put: Put,
    pub get: Get,
}

impl<Put, Get> PreImageStore for FnStore<Put, Get>
where
    Put: Fn(&[u8]) -> Result<String, String> + Send + Sync,
    Get: Fn(&str) -> Option<Vec<u8>> + Send + Sync,
{
    fn put(&self, bytes: &[u8]) -> Result<String, String> {
        (self.put)(bytes)
    }

    fn get(&self, hash: &str) -> Option<Vec<u8>> {
        (self.get)(hash)
    }
}

#[cfg(test)]
mod tests;
