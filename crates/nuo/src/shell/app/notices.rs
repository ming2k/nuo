//! Notice residency and the notice viewer's state (ADR-0121).
//!
//! # One funnel, so a bubble can never outlive its record
//!
//! Every disclosure the client surfaces — a routed `AgentNotice`, a failed
//! clipboard write, a link that could not be opened — goes through
//! `App::record_notice`. That function appends to the notice log first and
//! only then decides whether to raise a bubble:
//!
//! - a **fault** (`Warning` / `Error`) raises a bubble whose hint row names the
//!   viewer, so the interruption always leads somewhere;
//! - an **acknowledgement** (`Info`) is retained silently. It confirms
//!   something the user just did and can repeat, so interrupting them with it
//!   again would be noise — but it is still readable in the log, because "what
//!   did that command say?" is a question people ask a minute later.
//!
//! Producers call one function and cannot express "bubble without record",
//! which is the mechanism that makes "you had to be watching" impossible.
//!
//! # How long a bubble lives
//!
//! The dwell time is *derived from the message*, not a single magic number,
//! because "long enough to read" is a property of the text. Two published
//! floors and a ceiling bound it:
//!
//! - **5 s** is the perception threshold: WCAG 2.2.1 (Pause, Stop, Hide) draws
//!   the line at content that starts automatically and lasts more than five
//!   seconds, because below that a user who was looking elsewhere never had a
//!   fair chance to see it. Any auto-dismissing notice that carries information
//!   must therefore clear 5 s — and the acknowledgement dwell sits at 4 s
//!   because an ack is *not* information the user still needs (they caused it a
//!   moment ago and can repeat it), which is why it is also the only shape that
//!   yields to a keystroke.
//! - **10 s** is the attention ceiling: beyond it an unattended interruption
//!   stops being feedback and starts being an obstruction over the transcript
//!   the user is trying to read. Material's snackbar guidance (4 s default,
//!   10 s maximum) is the well-tested statement of the same range.
//!
//! Between those bounds the dwell scales with reading load at
//! `READING_MS_PER_WORD` — a deliberately slow rate (~150 wpm), because a
//! fault is unfamiliar text in a dense monospace grid, not prose. A one-line
//! `link down` gets the 6 s floor; a wrapped multi-clause backend error gets the
//! 10 s ceiling. The log outlives all of it, so erring long costs the user a
//! few seconds of a corner bubble, while erring short costs them the message.

use nuo_view::model::document::NoticeSeverity;
use nuo_view::model::log::{LineStatus, SurfaceLine};

use crate::shell::app::App;
use crate::shell::keymap::Key;

/// Dwell floor for a fault. Above WCAG's 5 s perception threshold, so a user
/// who was looking at the keyboard still has time to look up, read the line and
/// register the `Ctrl-o` hint.
const FAULT_BUBBLE_FLOOR_MS: u64 = 6_000;

/// Dwell ceiling for a fault. At Material's snackbar maximum: past this the
/// bubble stops being feedback and becomes an obstruction.
const FAULT_BUBBLE_CEILING_MS: u64 = 10_000;

/// Reading allowance per word (~150 wpm). Slower than prose on purpose: this is
/// unfamiliar diagnostic text rendered in a monospace grid, often containing
/// paths and identifiers the eye cannot skim.
const READING_MS_PER_WORD: u64 = 400;

/// Dwell for an acknowledgement. Material's snackbar default.
///
/// Below the 5 s perception threshold on purpose: an ack reports the outcome of
/// an action the user just took, so there is nothing in it they still need. It
/// only has to register that the action landed.
const ACK_BUBBLE_MS: u64 = 4_000;

/// How long a fault bubble should stay up for `text`.
///
/// Reading load is measured in words, not characters: a path-heavy message
/// (`/state/log/client.log*`) is one "word" but many characters, and it is the
/// word count that tracks the time a human needs to take it in.
pub(crate) fn fault_dwell_ms(text: &str) -> u64 {
    let words = text.split_whitespace().count() as u64;
    (FAULT_BUBBLE_FLOOR_MS + words * READING_MS_PER_WORD).min(FAULT_BUBBLE_CEILING_MS)
}

/// The viewer's own presentation state.
///
/// Deliberately *not* a `DialogView` entity: the viewer must be reachable from
/// every scene (a fault is a fact about the client, not about the conversation),
/// so its state lives on `App` beside the log it reads rather than inside the
/// session-scoped dialog registry.
///
/// The cursor is an index into the retained log, oldest-first. It is kept in
/// bounds by [`Self::clamp`] after every mutation, so a notice evicted by the
/// cap can never leave the cursor pointing past the end.
#[derive(Debug, Clone, Default)]
pub struct NoticeViewer {
    /// The selected notice, oldest-first.
    pub index: usize,
    /// Body scroll offset of the viewer.
    pub scroll: usize,
    /// Whether the body follows the selection.
    pub follow: bool,
    /// Whether the selected row is expanded to its wrapped detail.
    pub detail: bool,
    /// Scroll offset of the expanded row's detail.
    pub detail_scroll: usize,
    /// The body height the last paint produced. Written by the renderer so
    /// `PgUp`/`PgDn` step by a real page instead of a guess.
    pub body_height: u16,
}

impl NoticeViewer {
    /// A viewer that opens at the newest notice, which is what a user reaching
    /// for it after a missed bubble is looking for.
    pub fn open_at_newest(count: usize) -> Self {
        Self {
            index: count.saturating_sub(1),
            scroll: 0,
            follow: true,
            detail: false,
            detail_scroll: 0,
            body_height: 0,
        }
    }

    /// Keep the cursor inside `[0, count)`.
    pub fn clamp(&mut self, count: usize) {
        if count == 0 {
            self.index = 0;
            self.detail = false;
        } else if self.index >= count {
            self.index = count - 1;
        }
    }

    /// Move the cursor by one row and stop following the newest entry.
    pub fn step(&mut self, forward: bool, count: usize) {
        self.clamp(count);
        if forward {
            self.index = (self.index + 1).min(count.saturating_sub(1));
        } else {
            self.index = self.index.saturating_sub(1);
        }
        // A deliberate move means the user is reading, not watching: pin the
        // scroll so a notice arriving mid-read cannot yank the view.
        self.follow = false;
        self.detail = false;
        self.detail_scroll = 0;
    }

    /// Toggle the selected row's detail block.
    pub fn toggle_detail(&mut self, count: usize) {
        self.clamp(count);
        self.detail = !self.detail && count > 0;
        self.detail_scroll = 0;
    }
}

impl App {
    /// Record one disclosure and paint its bubble.
    ///
    /// Returns whether the disclosure was retained (an empty message is not).
    /// The log append happens in every case: `severity` decides *how the bubble
    /// behaves*, never *whether the record exists*. That is the inversion —
    /// retention is unconditional, interruption is not.
    pub(crate) fn record_notice(
        &mut self,
        severity: NoticeSeverity,
        topic: impl Into<String>,
        text: impl Into<String>,
        at_ms: u64,
    ) -> bool {
        self.record_notice_with_detail(severity, topic, text, None, at_ms)
    }

    /// Record one disclosure together with its structured detail.
    ///
    /// The detail is what the viewer's expanded row renders below the summary —
    /// the full provider body of a retry, the context of a diagnostic. A
    /// one-line summary alone is not enough for the content that most needs
    /// pasting, and since that content no longer lives in the transcript this is
    /// the only place it survives.
    pub(crate) fn record_notice_with_detail(
        &mut self,
        severity: NoticeSeverity,
        topic: impl Into<String>,
        text: impl Into<String>,
        detail: Option<String>,
        at_ms: u64,
    ) -> bool {
        let topic = topic.into();
        let log = self.notice_log();
        let Some(line) =
            crate::shell::app::receipts::record(log, severity, topic, text, detail, at_ms)
        else {
            return false;
        };
        let line = line.clone();
        self.raise_notice_bubble(&line);
        true
    }

    /// Record a local fault (a client-side action that failed) under a topic
    /// tag. Local faults are always `Error`: there is no degraded-success shape
    /// in the clipboard/link vocabulary, and inventing one here would let a
    /// silent degradation look like success.
    pub(crate) fn record_local_fault(&mut self, topic: impl Into<String>, text: impl Into<String>) {
        self.record_notice(
            NoticeSeverity::Error,
            topic,
            text,
            crate::shell::event_loop::now_epoch_ms(),
        );
    }

    /// Paint the notice bubble, superseding any in-flight one.
    ///
    /// A fault and an acknowledgement share the single bubble slot and both
    /// *win* it — a disclosure is always newer news than a copy receipt. What
    /// differs is the shape: a fault says where the record went (`Ctrl-o`) and
    /// stays longer, because it has to be read; an ack only has to be noticed.
    /// That is the one place the two differ, so it lives here rather than at
    /// each producer.
    fn raise_notice_bubble(&mut self, line: &SurfaceLine) {
        self.copy_toast_until = None;
        self.notice_toast_severity = match line.status {
            LineStatus::Failed => NoticeSeverity::Error,
            LineStatus::Warn => NoticeSeverity::Warning,
            LineStatus::Ok | LineStatus::Info => NoticeSeverity::Info,
        };
        self.notice_toast_message = line.text.clone();
        self.notice_toast_topic = line.label.clone().unwrap_or_default();
        let ms = if line.status.is_fault() {
            fault_dwell_ms(&line.text)
        } else {
            ACK_BUBBLE_MS
        };
        self.notice_toast_until =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(ms));
    }

    /// The pointer row a fault bubble paints under its message.
    ///
    /// `None` for an acknowledgement: an ack needs no escape hatch, and adding
    /// one would spend a row telling the user about a log they do not need.
    pub(crate) fn notice_bubble_hint(&self) -> Option<String> {
        if !self.notice_toast_severity.is_fault() {
            return None;
        }
        Some(format!("{} · all notices", Key::CTRL_O.display()))
    }

    /// Whether the notice viewer is open.
    pub(crate) fn notice_viewer_open(&self) -> bool {
        self.notice_viewer.is_some()
    }

    /// Open the notice viewer at the newest notice and acknowledge the log.
    ///
    /// Opening is what marks the log read: the badge counts *unseen* notices,
    /// and a viewer that is on screen is by definition seen. Marking on close
    /// instead would leave the badge lit for as long as the viewer is open,
    /// which is a lie the user can see.
    pub(crate) fn open_notice_viewer(&mut self) {
        self.notice_viewer = Some(NoticeViewer::open_at_newest(self.notice_count()));
        self.mark_notices_read();
    }

    /// Close the viewer, keeping the scroll position for the next open.
    pub(crate) fn close_notice_viewer(&mut self) {
        self.notice_viewer = None;
    }

    /// Toggle the viewer.
    pub(crate) fn toggle_notice_viewer(&mut self) {
        if self.notice_viewer_open() {
            self.close_notice_viewer();
        } else {
            self.open_notice_viewer();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_view::model::log::LineStatus;

    #[test]
    fn a_fault_dwells_above_the_perception_threshold_and_below_the_ceiling() {
        // WCAG 2.2.1 draws the perception line at 5s; Material's snackbar max is
        // 10s. Every fault must sit strictly inside that band, and the band must
        // actually be used (a one-word fault must not dwell at the ceiling).
        for text in [
            "x",
            "could not reach the backend",
            "Failed to open link https://example.test/x: permission denied",
            "Anthropic HTTP 529 Overloaded — the upstream refused the request after \
             five attempts; the previously cached model list is still shown",
        ] {
            let dwell = fault_dwell_ms(text);
            assert!(
                dwell > 5_000,
                "a fault must outlive WCAG's 5s perception threshold: {dwell}ms for {text:?}"
            );
            assert!(
                dwell <= 10_000,
                "and must not exceed the 10s attention ceiling: {dwell}ms for {text:?}"
            );
        }
        assert_eq!(
            fault_dwell_ms(""),
            FAULT_BUBBLE_FLOOR_MS,
            "an empty message gets the floor, not the ceiling"
        );
        assert!(
            fault_dwell_ms("x") > FAULT_BUBBLE_FLOOR_MS,
            "even one word buys a little more than the floor"
        );
        assert_eq!(
            fault_dwell_ms(&"word ".repeat(200)),
            FAULT_BUBBLE_CEILING_MS,
            "a wall of text is capped rather than allowed to grow without bound"
        );
    }

    #[test]
    fn dwell_scales_with_reading_load() {
        // The whole point of deriving instead of hard-coding: a longer message
        // buys more time, monotonically, until the ceiling.
        let short = fault_dwell_ms("link down");
        let medium = fault_dwell_ms("the backend link is down and the request was not delivered");
        assert!(short < medium, "{short} < {medium}");
        assert_eq!(
            ACK_BUBBLE_MS, 4_000,
            "an ack uses Material's snackbar default: below the perception \
             threshold, because it reports something the user just did"
        );
        const {
            assert!(
                ACK_BUBBLE_MS < FAULT_BUBBLE_FLOOR_MS,
                "and is always shorter-lived than any fault"
            )
        };
    }

    #[test]
    fn an_error_is_retained_and_interrupts_with_a_hint() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        assert!(app.record_notice(NoticeSeverity::Error, "backend", "link down", 1));
        assert_eq!(app.notice_count(), 1, "the record is the home");
        assert!(app.has_unread_notice_fault());
        assert!(
            app.notice_toast_until.is_some(),
            "a fault interrupts while the user can still act on it"
        );
        assert_eq!(app.notice_toast_message, "link down");
        assert_eq!(app.notice_toast_topic, "backend");
    }

    #[test]
    fn an_acknowledgement_is_retained_but_never_raises_the_fault_badge() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        assert!(app.record_notice(NoticeSeverity::Info, "command", "delegated on", 1));
        assert_eq!(app.notice_count(), 1, "still readable a minute later");
        assert!(!app.has_unread_notice_fault(), "but not news");
        assert!(!app.has_notice_fault(), "an ack is not a fault");
        assert!(
            app.notice_bubble_hint().is_none(),
            "an ack needs no escape hatch: the user just caused it"
        );
    }

    #[test]
    fn a_fault_bubble_advertises_the_viewer_but_an_ack_does_not() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        app.record_notice(NoticeSeverity::Error, "backend", "link down", 1);
        let hint = app
            .notice_bubble_hint()
            .expect("a fault points at its record");
        assert!(hint.contains("all notices"), "{hint}");
        app.record_notice(NoticeSeverity::Info, "command", "delegated on", 2);
        assert!(app.notice_bubble_hint().is_none());
    }

    #[test]
    fn a_warning_is_a_fault_and_a_newer_fault_supersedes_the_bubble() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        app.record_notice(NoticeSeverity::Warning, "images", "withheld", 1);
        assert_eq!(app.notice_toast_severity, NoticeSeverity::Warning);
        app.record_notice(NoticeSeverity::Error, "backend", "gone", 2);
        assert_eq!(
            app.notice_toast_message, "gone",
            "the one slot shows the newest fault"
        );
        assert_eq!(app.notice_count(), 2, "and the earlier one is not lost");
        assert_eq!(app.notice_lines()[0].status, LineStatus::Warn);
    }

    #[test]
    fn opening_the_viewer_acknowledges_the_log_but_keeps_it() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        app.record_notice(NoticeSeverity::Error, "backend", "link down", 1);
        app.open_notice_viewer();
        assert!(app.notice_viewer_open());
        assert_eq!(app.unread_notices(), 0, "a viewer on screen is seen");
        assert_eq!(app.notice_count(), 1, "reading is not forgetting");
        assert!(app.has_notice_fault());
        app.close_notice_viewer();
        assert!(!app.notice_viewer_open());
    }

    #[test]
    fn clearing_forgets_the_notices_and_resets_the_viewer() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        app.record_notice(NoticeSeverity::Error, "backend", "link down", 1);
        app.open_notice_viewer();
        app.notice_viewer.as_mut().expect("open").scroll = 4;
        app.clear_notices();
        assert!(!app.has_notices());
        assert_eq!(app.notice_viewer.as_ref().expect("open").scroll, 0);
    }

    #[test]
    fn a_local_fault_is_recorded_as_an_error_under_its_topic() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        app.record_local_fault("copy", "clipboard is empty");
        let line = app.notice_lines().back().expect("recorded");
        assert_eq!(line.status, LineStatus::Failed);
        assert_eq!(line.label.as_deref(), Some("copy"));
    }

    #[test]
    fn an_empty_disclosure_never_reaches_the_log_or_a_bubble() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        assert!(!app.record_notice(NoticeSeverity::Error, "t", "   ", 1));
        assert!(!app.has_notices());
        assert!(app.notice_toast_until.is_none());
    }
}
