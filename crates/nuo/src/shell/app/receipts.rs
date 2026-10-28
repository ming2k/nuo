//! The diagnostic receipt log: the last few error/warning toasts, kept
//! readable after they fade.
//!
//! # Why a receipt log exists at all
//!
//! Toasts are the client's only channel for a whole class of diagnostics —
//! "Failed to open link …: permission denied", "Could not refresh models:
//! backend disconnected", a clipboard write that timed out. They are painted
//! for a couple of seconds and then dropped. A user who looks up from the
//! keyboard one moment too late has lost the message entirely, and the message
//! is the thing they needed to copy.
//!
//! So the *display* stays transient (a toast must not accumulate on screen and
//! steal the conversation's attention) while the *record* is retained: the last
//! [`CAPACITY`] warning/error receipts, verbatim, each stamped with severity and
//! time. That turns "you had to be watching" into "press the verb and get the
//! last error".
//!
//! Info-level toasts ("3 images attached") are deliberately not retained: they
//! are acknowledgements of an action the user just took and can retake, and
//! keeping them would bury the actual faults.

use nuo_view::model::document::NoticeSeverity;

/// How many receipts are retained. Small on purpose: this is a recovery
/// affordance, not a log file — the rolling client log holds the full history,
/// and its path is on every diagnostic record.
pub(crate) const CAPACITY: usize = 8;

/// One retained disclosure, as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Receipt {
    pub severity: NoticeSeverity,
    pub message: String,
}

/// The last few warning/error disclosures the client surfaced.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReceiptLog {
    /// Newest last, so the most recent fault is a `last()` away and the
    /// retained window is the tail.
    entries: Vec<Receipt>,
    /// Total receipts ever recorded. Monotonic, so a "have I seen this?"
    /// comparison never has to reason about eviction.
    total: u64,
}

impl ReceiptLog {
    /// Record a disclosure. Info-level receipts are dropped at the door: they
    /// confirm an action rather than report a fault, and retaining them would
    /// evict the faults this log exists to keep.
    pub(crate) fn record(&mut self, severity: NoticeSeverity, message: impl Into<String>) {
        if severity == NoticeSeverity::Info {
            return;
        }
        let message = message.into();
        if message.trim().is_empty() {
            return;
        }
        if self.entries.len() == CAPACITY {
            self.entries.remove(0);
        }
        self.entries.push(Receipt { severity, message });
        self.total += 1;
    }

    /// The most recent retained receipt.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn last(&self) -> Option<&Receipt> {
        self.entries.last()
    }

    /// Every retained receipt, oldest first.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn entries(&self) -> &[Receipt] {
        &self.entries
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The retained receipts as one paste-ready block.
    ///
    /// Rendered through the same diagnostic shape as a transcript entry
    /// (`model::diagnostic`): a header naming what this is, then the receipts
    /// verbatim, one per line, severity-tagged. A user pasting this into a
    /// report has the sequence — including errors that never had a transcript
    /// row at all.
    pub(crate) fn render(&self, thread_id: Option<&str>, log_path: Option<&str>) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let mut header = String::from("[nuo diagnostic] kind=receipts");
        if let Some(thread) = thread_id.filter(|t| !t.is_empty()) {
            header.push_str(&format!(" thread={thread}"));
        }
        header.push_str(&format!(" count={}", self.entries.len()));
        if let Some(log) = log_path.filter(|p| !p.is_empty()) {
            header.push_str(&format!(" log=\"{log}\""));
        }
        let mut body = String::new();
        for receipt in &self.entries {
            if !body.is_empty() {
                body.push('\n');
            }
            let tag = match receipt.severity {
                NoticeSeverity::Error => "error",
                NoticeSeverity::Warning => "warning",
                NoticeSeverity::Info => "info",
            };
            body.push_str(&format!("[{tag}] {}", receipt.message));
        }
        Some(format!("{header}\n\n{body}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_receipts_are_not_retained() {
        let mut log = ReceiptLog::default();
        log.record(NoticeSeverity::Info, "3 images attached");
        assert!(log.is_empty());
    }

    #[test]
    fn the_window_keeps_the_newest_faults_and_evicts_the_oldest() {
        let mut log = ReceiptLog::default();
        for i in 0..CAPACITY + 3 {
            log.record(NoticeSeverity::Error, format!("fault {i}"));
        }
        assert_eq!(log.entries().len(), CAPACITY);
        assert_eq!(
            log.entries().first().map(|r| r.message.as_str()),
            Some("fault 3"),
            "the oldest receipts are the ones evicted"
        );
        assert_eq!(
            log.last().map(|r| r.message.as_str()),
            Some(format!("fault {}", CAPACITY + 2).as_str())
        );
    }

    #[test]
    fn an_empty_message_is_not_a_receipt() {
        let mut log = ReceiptLog::default();
        log.record(NoticeSeverity::Warning, "   ");
        assert!(log.is_empty());
    }

    #[test]
    fn rendering_carries_a_header_and_every_receipt_verbatim() {
        let mut log = ReceiptLog::default();
        log.record(
            NoticeSeverity::Error,
            "Failed to open link https://x.test: permission denied",
        );
        log.record(NoticeSeverity::Warning, "copy failed: clipboard busy");

        let rendered = log
            .render(Some("01HXYZ"), Some("/state/log/client.log*"))
            .expect("non-empty log renders");
        let (header, body) = rendered.split_once("\n\n").expect("header/body");
        assert!(header.starts_with("[nuo diagnostic] kind=receipts"));
        assert!(header.contains("thread=01HXYZ"));
        assert!(header.contains("count=2"));
        assert!(header.contains("log=\"/state/log/client.log*\""));
        assert!(body.contains("[error] Failed to open link https://x.test: permission denied"));
        assert!(body.contains("[warning] copy failed: clipboard busy"));
    }
}
