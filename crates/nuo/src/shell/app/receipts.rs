//! The client's notice log: every disclosure the shell surfaced, retained
//! after its bubble fades (ADR-0116 `[INV-LAYER-03]`, ADR-0121).
//!
//! # Why the log is the destination and the bubble is the exception
//!
//! A bubble is the client's *interruption* channel, not its record. It is
//! painted for a second or two and then dropped, and the shell has exactly one
//! bubble slot — so a second fault arriving 400ms after the first *erases* the
//! first. A user who looked down at the keyboard, or who was reading the
//! transcript while a "could not reach the backend" flashed past, has lost the
//! message entirely, and the message is the thing they needed to copy.
//!
//! So residency is inverted here: **the log is the home, and the bubble is a
//! pointer to it.** Every disclosure is appended to the notice log first; the
//! bubble then says where the rest went (`Ctrl-o`) so the interruption is never
//! a dead end, and the log's unread count keeps a badge lit until the user
//! actually looks.
//!
//! # One registry, one answer
//!
//! The log itself is an ordinary [`SurfaceLog`] in [`App::logs`], addressed by
//! [`SurfaceId::NOTICES`] — the same tier, the same cap, the same residency
//! contract as any other surface's receipts ([ADR-0116]). This module adds no
//! storage of its own: it is the *typed vocabulary* the shell speaks when it
//! records a disclosure, so a caller cannot invent a second place to put one.
//!
//! # What this log is not
//!
//! It is not a log *file* — the rolling client log holds the full history, and
//! every diagnostic record names its path. It is not persisted, never enters the
//! model window, and never participates in compaction, search or export
//! (`[INV-LAYER-03]`). It dies with the process, which is the correct lifetime
//! for "what did this client tell me while I was not looking".

use nuo_view::model::document::NoticeSeverity;
use nuo_view::model::log::{LineStatus, SurfaceId, SurfaceLine, SurfaceLog};

use crate::shell::app::App;

/// How many notices are retained.
///
/// Larger than the eight-entry receipt window it replaces because the log is now
/// a reading surface rather than a last-resort recovery affordance: the viewer
/// scrolls, so the cap bounds memory, not comprehension.
pub(crate) const CAPACITY: usize = 128;

/// Record one disclosure and return the retained line.
///
/// Empty messages are dropped at the door: a blank row in the viewer is a row
/// the user cannot act on.
/// Record one disclosure plus its structured detail.
///
/// `detail` is rendered by the viewer's expanded row. It exists because the
/// summary that fits one row is not always the whole message — a retry receipt's
/// provider body, a diagnostic's context — and content that must remain
/// pasteable cannot be reduced to a truncated line.
pub(crate) fn record(
    log: &mut SurfaceLog,
    severity: NoticeSeverity,
    topic: impl Into<String>,
    message: impl Into<String>,
    detail: Option<String>,
    at_ms: u64,
) -> Option<&SurfaceLine> {
    let message = message.into();
    if message.trim().is_empty() {
        return None;
    }
    let mut line =
        SurfaceLine::disclosure(LineStatus::from_notice_severity(severity), message, at_ms)
            .with_label(topic);
    if let Some(detail) = detail.filter(|d| !d.trim().is_empty()) {
        line = line.with_detail(detail);
    }
    log.push(line);
    log.lines().back()
}

/// The retained notices as one paste-ready block.
///
/// Rendered through the same diagnostic shape as a transcript entry
/// (`model::diagnostic`): a header naming what this is, then the notices
/// verbatim, one per line, severity-tagged and time-stamped. A user pasting this
/// into a report has the sequence — including errors that never had a transcript
/// row at all.
pub(crate) fn render(
    log: &SurfaceLog,
    thread_id: Option<&str>,
    log_path: Option<&str>,
) -> Option<String> {
    if log.is_empty() {
        return None;
    }
    let mut header = String::from("[nuo diagnostic] kind=notices");
    if let Some(thread) = thread_id.filter(|t| !t.is_empty()) {
        header.push_str(&format!(" thread={thread}"));
    }
    header.push_str(&format!(" count={}", log.len()));
    if let Some(log) = log_path.filter(|p| !p.is_empty()) {
        header.push_str(&format!(" log=\"{log}\""));
    }
    let mut body = String::new();
    for line in log.lines() {
        if !body.is_empty() {
            body.push('\n');
        }
        let tag = match line.status {
            LineStatus::Failed => "error",
            LineStatus::Warn => "warning",
            LineStatus::Ok => "ok",
            LineStatus::Info => "info",
        };
        let time = nuo_ui_primitives::time::sent_time_label(line.at_ms);
        match line.label.as_deref() {
            Some(topic) => body.push_str(&format!("[{tag}] {time} {topic}: {}", line.text)),
            None => body.push_str(&format!("[{tag}] {time} {}", line.text)),
        }
    }
    Some(format!("{header}\n\n{body}"))
}

impl App {
    /// The notice log's surface id — the one address every reader and writer
    /// agrees on.
    pub(crate) const fn notice_surface() -> SurfaceId {
        SurfaceId::NOTICES
    }

    /// The notice log, created with [`CAPACITY`] on first use.
    ///
    /// The default `SurfaceLog` cap (256) is tuned for a dialog's working
    /// receipts; the notice log is the client's whole disclosure history, so it
    /// states its own bound at creation (`entry_with_cap`) rather than
    /// inheriting one.
    pub(crate) fn notice_log(&mut self) -> &mut SurfaceLog {
        self.logs.entry_with_cap(Self::notice_surface(), CAPACITY)
    }

    /// The retained notices, oldest first.
    pub(crate) fn notice_lines(&self) -> &std::collections::VecDeque<SurfaceLine> {
        static EMPTY: std::collections::VecDeque<SurfaceLine> = std::collections::VecDeque::new();
        self.logs
            .get(Self::notice_surface())
            .map_or(&EMPTY, |log| log.lines())
    }

    /// How many notices are retained.
    pub(crate) fn notice_count(&self) -> usize {
        self.logs
            .get(Self::notice_surface())
            .map_or(0, SurfaceLog::len)
    }

    /// Whether anything is retained at all (the viewer/copy verbs need a
    /// non-empty log to act on).
    pub(crate) fn has_notices(&self) -> bool {
        self.notice_count() > 0
    }

    /// How many notices arrived since the viewer last acknowledged the log.
    pub(crate) fn unread_notices(&self) -> usize {
        self.logs
            .get(Self::notice_surface())
            .map_or(0, SurfaceLog::unread)
    }

    /// Whether an unread notice is a fault — the badge's only trigger.
    pub(crate) fn has_unread_notice_fault(&self) -> bool {
        self.logs
            .get(Self::notice_surface())
            .is_some_and(SurfaceLog::has_unread_fault)
    }

    /// Whether the log holds any fault at all.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn has_notice_fault(&self) -> bool {
        self.logs
            .get(Self::notice_surface())
            .is_some_and(SurfaceLog::has_fault)
    }

    /// Acknowledge everything retained: the viewer was opened.
    pub(crate) fn mark_notices_read(&mut self) {
        self.logs.entry(Self::notice_surface()).mark_read();
    }

    /// Forget every retained notice (the viewer's clear verb).
    pub(crate) fn clear_notices(&mut self) {
        self.logs.entry(Self::notice_surface()).clear();
        if let Some(viewer) = self.notice_viewer.as_mut() {
            *viewer = crate::shell::app::notices::NoticeViewer::open_at_newest(0);
        }
    }

    /// The retained notices as a paste-ready diagnostic block.
    pub(crate) fn render_notices(&self, thread_id: Option<&str>) -> Option<String> {
        let log_path = crate::shell::copy::client_log_path_for_report();
        let log = self.logs.get(Self::notice_surface())?;
        render(log, thread_id, log_path.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log() -> SurfaceLog {
        SurfaceLog::with_cap(CAPACITY)
    }

    #[test]
    fn a_recorded_notice_keeps_its_severity_topic_and_time() {
        let mut log = log();
        record(
            &mut log,
            NoticeSeverity::Error,
            "backend",
            "could not reach the backend",
            None,
            1_700_000_000_000,
        );
        assert_eq!(log.len(), 1);
        let line = log.lines().front().expect("one retained notice");
        assert_eq!(line.status, LineStatus::Failed);
        assert_eq!(line.label.as_deref(), Some("backend"));
        assert_eq!(line.text, "could not reach the backend");
        assert_eq!(line.at_ms, 1_700_000_000_000);
    }

    #[test]
    fn an_empty_message_is_not_a_notice() {
        let mut log = log();
        assert!(
            record(&mut log, NoticeSeverity::Warning, "copy", "   ", None, 0).is_none(),
            "a blank row is a row the user cannot act on"
        );
        assert!(log.is_empty());
    }

    #[test]
    fn the_window_keeps_the_newest_notices_and_evicts_the_oldest() {
        let mut log = SurfaceLog::with_cap(3);
        for i in 0..6 {
            record(
                &mut log,
                NoticeSeverity::Error,
                "t",
                format!("fault {i}"),
                None,
                i,
            );
        }
        assert_eq!(log.len(), 3);
        assert_eq!(
            log.lines().front().map(|l| l.text.as_str()),
            Some("fault 3"),
            "the oldest notices are the ones evicted"
        );
        assert_eq!(log.lines().back().map(|l| l.text.as_str()), Some("fault 5"));
    }

    #[test]
    fn an_info_notice_is_retained_but_does_not_raise_the_fault_badge() {
        let mut log = log();
        record(
            &mut log,
            NoticeSeverity::Info,
            "command",
            "delegated mode on",
            None,
            0,
        );
        assert_eq!(log.len(), 1, "an acknowledgement is still readable later");
        assert!(!log.has_unread_fault(), "but it is not news");
        assert!(!log.has_fault());
    }

    #[test]
    fn rendering_carries_a_header_and_every_notice_verbatim() {
        let mut log = log();
        record(
            &mut log,
            NoticeSeverity::Error,
            "link",
            "Failed to open link https://x.test: permission denied",
            None,
            1_700_000_000_000,
        );
        record(
            &mut log,
            NoticeSeverity::Warning,
            "copy",
            "copy failed: clipboard busy",
            None,
            1_700_000_060_000,
        );

        let rendered = render(&log, Some("01HXYZ"), Some("/state/log/client.log*"))
            .expect("non-empty log renders");
        let (header, body) = rendered.split_once("\n\n").expect("header/body");
        assert!(header.starts_with("[nuo diagnostic] kind=notices"));
        assert!(header.contains("thread=01HXYZ"));
        assert!(header.contains("count=2"));
        assert!(header.contains("log=\"/state/log/client.log*\""));
        assert!(body.contains("[error] "));
        assert!(body.contains("link: Failed to open link https://x.test: permission denied"));
        assert!(body.contains("[warning] "));
        assert!(body.contains("copy: copy failed: clipboard busy"));
        assert_eq!(body.lines().count(), 2);
        for line in body.lines() {
            // `[severity] HH:MM topic: message` — the stamp is what lets a user
            // correlate a pasted report with the rolling client log.
            let stamp = line.split_whitespace().nth(1).expect("stamp token");
            assert_eq!(stamp.len(), 5, "HH:MM stamp in {line:?}");
            assert_eq!(stamp.as_bytes()[2], b':', "HH:MM stamp in {line:?}");
        }
    }

    #[test]
    fn an_empty_log_renders_nothing() {
        assert!(render(&log(), None, None).is_none());
    }
}
