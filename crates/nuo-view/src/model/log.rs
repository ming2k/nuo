//! The receipt-log tier (ADR-0116, `[INV-LAYER-03]`).
//!
//! A [`SurfaceLog`] is a surface's own scrollback: an addressable, capped,
//! **ephemeral** record of what was asked and what came back. It exists because
//! the presentation stack had only two content layers — the thread transcript
//! and a toast — and toasts are strictly reserved for three affirmation shapes.
//! Everything else (diagnostics, connection and asset failures, retrieval
//! receipts, configuration confirmations, a dialog's own question/answer pairs)
//! was therefore forced into the transcript timeline, where it cannot be
//! displayed effectively: the transcript streams, is not addressable, and
//! participates in compaction, search, export and the model window.
//!
//! # Residency contract
//!
//! A log line is *never* persisted, never enters the model window, never
//! participates in compaction/search/export, and dies with its owner's lifetime
//! (a closed dialog, a left dashboard). Durable facts stay facts first — they
//! live in the session store and are re-projected on resume — and only their
//! *presentation* lands here.
//!
//! # Vocabulary
//!
//! Three heads, chosen so any surface can speak them without inventing
//! variants: what was **asked** ([`SurfaceHead::Query`]), what **came back**
//! ([`SurfaceHead::Reply`]), and what was **observed** ([`SurfaceHead::Note`]).
//! Status ([`LineStatus`]) is orthogonal to head — a query is usually neutral, a
//! reply may have failed, a note may be a warning.

use std::collections::VecDeque;

/// Identity of a content surface that owns a log.
///
/// A static label, because every surface in the product is a compile-time
/// entity (`"thread"`, `"dashboard"`, `"dialog:models"`, …). The identifier is
/// the *rendezvous*: the routing policy
/// ([`crate::model::layer`]) names a surface, and the registry below holds its
/// log. This keeps the presentation substrate free of any dependency on the
/// host's surface enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SurfaceId(pub &'static str);

impl SurfaceId {
    /// The ambient thread surface (the one surface that must never receive a
    /// log line — it owns the transcript instead).
    pub const THREAD: Self = Self("thread");
    /// The dashboard cockpit console.
    pub const DASHBOARD: Self = Self("dashboard");
}

/// How a line ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStatus {
    /// Neutral information.
    Info,
    /// The operation succeeded.
    Ok,
    /// The operation failed; the body carries the reason.
    Failed,
}

impl LineStatus {
    /// Whether the line reports a failure.
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Failed)
    }
}

/// What kind of utterance a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceHead {
    /// Something the user (or the surface's own grammar) asked for.
    Query,
    /// The answer to a query.
    Reply,
    /// A local observation that belongs to neither side of an exchange.
    Note,
}

/// One immutable log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceLine {
    /// Wall-clock milliseconds since the epoch. Lines are append-only, so this
    /// is the only ordering key a surface may show.
    pub at_ms: u64,
    /// What kind of utterance this is.
    pub head: SurfaceHead,
    /// How it ended.
    pub status: LineStatus,
    /// The one-line summary.
    pub text: String,
    /// The surface's own tag for this line (a target, a verb, a subject). The
    /// log tier never interprets it; it exists so a surface can keep its
    /// vocabulary without inventing a second log type.
    pub label: Option<String>,
    /// Optional detail prose (a server error body, a diagnostic's context).
    pub detail: Option<String>,
}

impl SurfaceLine {
    /// A neutral note.
    pub fn note(text: impl Into<String>, at_ms: u64) -> Self {
        Self {
            at_ms,
            head: SurfaceHead::Note,
            status: LineStatus::Info,
            text: text.into(),
            label: None,
            detail: None,
        }
    }

    /// A query the surface dispatched.
    pub fn query(text: impl Into<String>, at_ms: u64) -> Self {
        Self {
            at_ms,
            head: SurfaceHead::Query,
            status: LineStatus::Info,
            text: text.into(),
            label: None,
            detail: None,
        }
    }

    /// A reply, with its terminal status.
    pub fn reply(ok: bool, text: impl Into<String>, at_ms: u64) -> Self {
        Self {
            at_ms,
            head: SurfaceHead::Reply,
            status: if ok {
                LineStatus::Ok
            } else {
                LineStatus::Failed
            },
            text: text.into(),
            label: None,
            detail: None,
        }
    }

    /// Attach the surface's own tag for this line.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Attach detail prose.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// A surface's capped receipt log.
///
/// The cap is a line count, not a byte or token budget: this is a working
/// record whose lifetime is the surface's, so the only invariant is that it
/// cannot grow without bound while the surface is open.
#[derive(Debug, Clone)]
pub struct SurfaceLog {
    lines: VecDeque<SurfaceLine>,
    cap: usize,
}

impl Default for SurfaceLog {
    fn default() -> Self {
        Self::with_cap(256)
    }
}

impl SurfaceLog {
    /// A log bounded at `cap` lines (clamped to at least one).
    pub fn with_cap(cap: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// Append one line, evicting the oldest when full.
    pub fn push(&mut self, line: SurfaceLine) {
        if self.lines.len() == self.cap {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// The lines in append order (oldest first).
    pub fn lines(&self) -> &VecDeque<SurfaceLine> {
        &self.lines
    }

    /// The line count.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The line cap.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Drop every line. Called when the owning surface opens or is dismissed,
    /// depending on its retention policy.
    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

/// Every open surface's log, addressed by [`SurfaceId`].
///
/// One registry, so "where does this content live?" has one answer and a
/// surface cannot invent a private log type. `[INV-LAYER-01]`: a line has
/// exactly one home.
#[derive(Debug, Default)]
pub struct SurfaceLogs {
    logs: std::collections::BTreeMap<SurfaceId, SurfaceLog>,
}

impl SurfaceLogs {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// The log for `id`, created on first use with the default cap.
    pub fn entry(&mut self, id: SurfaceId) -> &mut SurfaceLog {
        self.logs.entry(id).or_default()
    }

    /// The log for `id`, if the surface has ever been logged to.
    pub fn get(&self, id: SurfaceId) -> Option<&SurfaceLog> {
        self.logs.get(&id)
    }

    /// The log for `id`, created on first use.
    pub fn get_or_insert(&mut self, id: SurfaceId) -> &mut SurfaceLog {
        self.entry(id)
    }

    /// Push one line onto `id`'s log, creating it when absent.
    pub fn push(&mut self, id: SurfaceId, line: SurfaceLine) {
        self.entry(id).push(line);
    }

    /// Drop `id`'s log entirely (surface closed, nothing retained).
    pub fn discard(&mut self, id: SurfaceId) {
        self.logs.remove(&id);
    }

    /// Clear `id`'s lines but keep the log allocated.
    pub fn clear(&mut self, id: SurfaceId) {
        if let Some(log) = self.logs.get_mut(&id) {
            log.clear();
        }
    }

    /// The ids that currently hold lines.
    pub fn ids(&self) -> impl Iterator<Item = SurfaceId> + '_ {
        self.logs.keys().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eviction_keeps_the_newest_lines() {
        let mut log = SurfaceLog::with_cap(3);
        for i in 0..5 {
            log.push(SurfaceLine::note(format!("line {i}"), i as u64));
        }
        let texts: Vec<&str> = log.lines().iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["line 2", "line 3", "line 4"]);
        assert_eq!(log.len(), 3);
    }

    #[test]
    fn reply_status_tracks_the_outcome() {
        assert_eq!(SurfaceLine::reply(true, "ok", 0).status, LineStatus::Ok);
        assert_eq!(
            SurfaceLine::reply(false, "nope", 0).status,
            LineStatus::Failed
        );
        assert!(SurfaceLine::reply(false, "nope", 0).status.is_failure());
    }

    #[test]
    fn registry_owns_one_log_per_surface() {
        let mut logs = SurfaceLogs::new();
        logs.push(SurfaceId::DASHBOARD, SurfaceLine::note("a", 1));
        logs.push(SurfaceId("dialog:models"), SurfaceLine::note("b", 2));
        assert_eq!(logs.get(SurfaceId::DASHBOARD).map(SurfaceLog::len), Some(1));
        assert_eq!(
            logs.get(SurfaceId("dialog:models")).map(SurfaceLog::len),
            Some(1)
        );
        assert!(logs.get(SurfaceId("dialog:tools")).is_none());
        logs.discard(SurfaceId("dialog:models"));
        assert!(logs.get(SurfaceId("dialog:models")).is_none());
    }

    #[test]
    fn a_zero_cap_still_holds_one_line() {
        let mut log = SurfaceLog::with_cap(0);
        log.push(SurfaceLine::note("only", 0));
        assert_eq!(log.len(), 1);
        assert_eq!(log.cap(), 1);
    }
}
