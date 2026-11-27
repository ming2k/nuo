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
    /// The client's own notice log: every disclosure the shell surfaced,
    /// retained after its bubble fades.
    ///
    /// Client-global rather than thread-scoped on purpose. A fault ("could not
    /// reach the backend", "clipboard write timed out") is a fact about *this
    /// client process*, not about the conversation on screen, and it must stay
    /// readable across a tab or session switch — the user who missed the bubble
    /// is often no longer standing where it appeared. Nothing here is ever
    /// persisted ([`SurfaceLog`]'s residency contract).
    pub const NOTICES: Self = Self("notices");
}

/// How a line ended.
///
/// Four rungs, because "it came back" is not the whole story: a degraded reply
/// (`Warn` — the workspace loaded but two roots were skipped, the image was
/// withheld) is neither a success nor a failure, and collapsing it into either
/// one lies to the reader who came to this log precisely because the bubble
/// vanished. The mapping from a disclosure's severity is therefore total:
/// `Info → Info`, `Ok → Ok`, `Warning → Warn`, `Error → Failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStatus {
    /// Neutral information.
    Info,
    /// The operation succeeded.
    Ok,
    /// The operation completed with a degraded outcome.
    Warn,
    /// The operation failed; the body carries the reason.
    Failed,
}

impl LineStatus {
    /// Whether the line reports a failure.
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Failed)
    }

    /// Whether the line is worth interrupting the user for: a failure or a
    /// degraded outcome. This is the single predicate the presentation layer
    /// consults to decide whether a retained line also raises a bubble, so
    /// "which faults interrupt?" cannot drift between call sites.
    pub fn is_fault(self) -> bool {
        matches!(self, Self::Failed | Self::Warn)
    }

    /// The rung a disclosure's severity maps onto.
    ///
    /// Total on purpose: a new severity cannot compile until its residency is
    /// chosen, exactly as a new `NoticeKind` cannot (`crate::model::layer`).
    pub fn from_notice_severity(severity: crate::model::document::NoticeSeverity) -> Self {
        use crate::model::document::NoticeSeverity;
        match severity {
            NoticeSeverity::Info => Self::Info,
            NoticeSeverity::Warning => Self::Warn,
            NoticeSeverity::Error => Self::Failed,
        }
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

    /// A note that reports an outcome rather than plain information — the
    /// shape a routed diagnostic takes when it lands in a log. The severity
    /// vocabulary is the log's own ([`LineStatus`]) so a surface never has to
    /// know which disclosure system produced the line.
    pub fn disclosure(status: LineStatus, text: impl Into<String>, at_ms: u64) -> Self {
        Self {
            at_ms,
            head: SurfaceHead::Note,
            status,
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
///
/// # Unread is a property of the log, not of the viewer
///
/// The log counts the lines that arrived since its viewer last looked. Keeping
/// the watermark here rather than in the widget state means the count survives
/// a scene switch, a re-layout and a viewer that has never been constructed —
/// and it cannot be silently reset by an unrelated rebuild of the UI tree,
/// which is exactly the bug class that makes "you missed 3 errors" unreliable.
#[derive(Debug, Clone)]
pub struct SurfaceLog {
    lines: VecDeque<SurfaceLine>,
    cap: usize,
    unread: usize,
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
            unread: 0,
        }
    }

    /// Append one line, evicting the oldest when full.
    pub fn push(&mut self, line: SurfaceLine) {
        if self.lines.len() == self.cap {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
        self.unread = self.unread.saturating_add(1);
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

    /// How many lines arrived since the viewer last acknowledged the log.
    pub fn unread(&self) -> usize {
        self.unread
    }

    /// Whether any unread line is a fault — the only unread state worth a
    /// badge, since an unread acknowledgement ("copied") is not news.
    pub fn has_unread_fault(&self) -> bool {
        self.unread > 0
            && self
                .lines
                .iter()
                .rev()
                .take(self.unread)
                .any(|l| l.status.is_fault())
    }

    /// Whether the log currently holds any fault at all.
    pub fn has_fault(&self) -> bool {
        self.lines.iter().any(|l| l.status.is_fault())
    }

    /// Mark every line read: the viewer has been looked at.
    pub fn mark_read(&mut self) {
        self.unread = 0;
    }

    /// Drop every line. Called when the owning surface opens or is dismissed,
    /// depending on its retention policy.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.unread = 0;
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

    /// The log for `id`, created on first use with `cap`.
    ///
    /// A surface whose retention bound differs from the default states it here
    /// rather than rewriting the log after the fact: the cap is a property of
    /// the log's *creation*, and building one to rebuild it would silently
    /// disturb the unread watermark (`push` counts an arrival).
    pub fn entry_with_cap(&mut self, id: SurfaceId, cap: usize) -> &mut SurfaceLog {
        self.logs
            .entry(id)
            .or_insert_with(|| SurfaceLog::with_cap(cap))
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
    fn a_named_cap_is_applied_at_creation_and_kept_on_re_entry() {
        // The notice log states its own bound; re-entering it (a scene switch
        // never discards it) must not rebuild the log, because rebuilding would
        // re-count every retained line as unread.
        let mut logs = SurfaceLogs::new();
        logs.entry_with_cap(SurfaceId::NOTICES, 3)
            .push(SurfaceLine::note("a", 1));
        logs.entry_with_cap(SurfaceId::NOTICES, 3)
            .push(SurfaceLine::note("b", 2));
        let log = logs.get(SurfaceId::NOTICES).expect("created");
        assert_eq!(log.cap(), 3, "the declared cap is the one in force");
        assert_eq!(log.len(), 2);
        assert_eq!(log.unread(), 2, "exactly two arrivals, not four");
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

    #[test]
    fn unread_counts_arrivals_and_only_faults_raise_a_badge() {
        // The whole point of the log: a line the user never saw stays counted
        // until a viewer acknowledges it. An unread acknowledgement ("copied")
        // is not news, so it must not raise the badge — only a fault does.
        let mut log = SurfaceLog::with_cap(8);
        log.push(SurfaceLine::disclosure(LineStatus::Ok, "copied", 1));
        assert_eq!(log.unread(), 1);
        assert!(!log.has_unread_fault(), "an ack is not a fault");

        log.push(SurfaceLine::disclosure(LineStatus::Failed, "boom", 2));
        assert_eq!(log.unread(), 2);
        assert!(log.has_unread_fault());

        log.mark_read();
        assert_eq!(log.unread(), 0);
        assert!(!log.has_unread_fault());
        assert!(log.has_fault(), "reading is not forgetting");
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn eviction_does_not_leave_a_badge_for_a_line_that_is_gone() {
        // Unread is clamped by what is actually retained: a badge can never
        // count lines that were evicted before the viewer looked.
        let mut log = SurfaceLog::with_cap(2);
        for i in 0..5 {
            log.push(SurfaceLine::disclosure(
                LineStatus::Failed,
                format!("f{i}"),
                i,
            ));
        }
        assert_eq!(log.unread(), 5, "every arrival was unread");
        assert_eq!(log.len(), 2);
        log.mark_read();
        log.clear();
        assert_eq!(log.unread(), 0);
    }

    #[test]
    fn severity_maps_onto_the_log_vocabulary_totally() {
        use crate::model::document::NoticeSeverity;
        assert_eq!(
            LineStatus::from_notice_severity(NoticeSeverity::Info),
            LineStatus::Info
        );
        assert_eq!(
            LineStatus::from_notice_severity(NoticeSeverity::Warning),
            LineStatus::Warn
        );
        assert_eq!(
            LineStatus::from_notice_severity(NoticeSeverity::Error),
            LineStatus::Failed
        );
        assert!(LineStatus::Warn.is_fault());
        assert!(!LineStatus::Ok.is_fault());
        assert!(!LineStatus::Info.is_fault());
    }

    #[test]
    fn the_notice_log_is_a_named_client_global_surface() {
        // The routing table names this surface, so it must be distinct from the
        // ambient thread surface — a disclosure is never thread dialogue.
        assert_ne!(SurfaceId::NOTICES, SurfaceId::THREAD);
        assert_eq!(SurfaceId::NOTICES.0, "notices");
    }
}
