//! The copyable **diagnostic contract**: one entry, one paste-ready record.
//!
//! # Why a canonical record instead of "just copy the text"
//!
//! A user who copies an error has exactly one follow-up: paste it somewhere
//! else to have it read (a colleague, an issue tracker, a search box). The body
//! text alone answers "what went wrong" but not "where, with which route, on
//! which attempt, in which thread" — the facts a reader needs first, and the
//! facts that are on screen in *chrome* (the entry head, the model bar) rather
//! than in the body. So a diagnostic copy is `header + "\n\n" + body`:
//!
//! ```text
//! [nuo diagnostic] kind=notice severity=error topic=provider provider=openai model=gpt-5 at=2026-10-28T02:12:33Z thread=01HXYZ attempt=2/6
//!
//! OpenAI HTTP 429 Too Many Requests: rate limit reached
//! {
//!   "error": { "code": "1305" }
//! }
//! ```
//!
//! # Format rules (frozen)
//!
//! - One header line, introduced by [`HEADER`]. A paste destination can find
//!   it with a literal string search and strip it if it wants the body only.
//! - `key=value` pairs, space-separated, in declaration order. Values
//!   containing whitespace are double-quoted with `\` escaping, so the header
//!   stays line-oriented and space-splittable.
//! - Unknown-to-a-reader keys are ignored, so adding a field later never
//!   invalidates an older paste. Removing or renaming one is a breaking change.
//! - The body is the entry's own verbatim text — the exact bytes the entry
//!   displays and the user can drag-select. Nothing is re-wrapped, re-escaped,
//!   or pretty-printed here.
//!
//! The record itself carries no I/O and no paths: facts the shell owns (thread
//! id, the client log path) arrive through [`DiagnosticContext`], so this
//! module stays pure and testable.

use crate::model::document::{MessageKind, NoticeSeverity, ToolStepStatus, TranscriptMessage};

/// Line prefix that marks the machine-readable header. Frozen: readers key on
/// this literal.
pub const HEADER: &str = "[nuo diagnostic]";

/// What the record was extracted from. Drives the `kind=` field and lets a
/// reader tell an error entry from a failed tool step without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Notice,
    ProviderRetry,
    ToolStep,
    Reasoning,
    CommandResult,
    CompactedCard,
    Text,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::Notice => "notice",
            EntryKind::ProviderRetry => "provider-retry",
            EntryKind::ToolStep => "tool-step",
            EntryKind::Reasoning => "reasoning",
            EntryKind::CommandResult => "command",
            EntryKind::CompactedCard => "compacted",
            EntryKind::Text => "text",
        }
    }
}

/// Facts the shell owns and the record cannot know: which thread the entry
/// belongs to, and where the client writes its log. Both are optional so a
/// view rendered without a session (storybooks, tests) still produces a valid
/// record.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiagnosticContext<'a> {
    pub thread_id: Option<&'a str>,
    pub log_path: Option<&'a str>,
}

/// A paste-ready diagnostic: structured facts (header) + verbatim text (body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticRecord {
    pub kind: EntryKind,
    pub severity: Option<NoticeSeverity>,
    /// The subsystem label the entry head shows (`provider`, `trust`, `retry`, …).
    pub topic: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// `(attempt, max_attempts)` for provider-retry entries.
    pub attempt: Option<(usize, usize)>,
    /// When the entry was stamped, in Unix-epoch milliseconds.
    pub sent_at_ms: Option<u64>,
    /// Kind-specific facts, in header order (e.g. `tool=read_text exit=1`).
    pub extra: Vec<(&'static str, String)>,
    /// The entry's verbatim text — what the user sees and can select.
    pub body: String,
}

impl DiagnosticRecord {
    /// Project a transcript entry into a diagnostic record.
    ///
    /// Returns `None` for an entry with no text: there is nothing to paste, and
    /// a successful-looking copy of an empty string is worse than no copy.
    pub fn from_message(message: &TranscriptMessage) -> Option<Self> {
        let body = crate::model::interactive::copy_text(message)?;
        if body.trim().is_empty() {
            return None;
        }

        let mut extra: Vec<(&'static str, String)> = Vec::new();
        let severity = match &message.kind {
            MessageKind::Notice { severity, .. } => Some(*severity),
            MessageKind::ProviderRetry { .. } => Some(NoticeSeverity::Warning),
            _ => None,
        };
        let attempt = match &message.kind {
            MessageKind::ProviderRetry {
                attempt,
                max_attempts,
                ..
            } => Some((*attempt, *max_attempts)),
            _ => message
                .notice_parts()
                .and_then(|parts| match parts.origin.as_ref() {
                    Some(crate::model::document::NoticeOrigin::Provider { attempt, .. }) => {
                        *attempt
                    }
                    _ => None,
                }),
        };
        match &message.kind {
            MessageKind::ToolStep {
                name,
                status,
                output,
                structured,
                ..
            } => {
                extra.push(("tool", name.clone()));
                extra.push(("status", tool_status_label(*status).to_string()));
                if let Some(code) = command_exit(name, output.as_deref(), structured.as_deref()) {
                    extra.push(("exit", code.to_string()));
                }
                if let Some(bytes) = output.as_ref().map(|o| o.len()) {
                    extra.push(("bytes", bytes.to_string()));
                }
            }
            MessageKind::CommandResult { invocation, .. } => {
                extra.push(("command", invocation.name.clone()));
            }
            MessageKind::CompactedCard {
                archived_messages,
                window_tokens_before,
                window_tokens_after,
                ..
            } => {
                extra.push(("archived", archived_messages.to_string()));
                extra.push(("tokens-before", window_tokens_before.to_string()));
                extra.push(("tokens-after", window_tokens_after.to_string()));
            }
            MessageKind::Reasoning {
                duration_ms: Some(ms),
                ..
            } => {
                extra.push(("duration-ms", ms.to_string()));
            }
            MessageKind::Reasoning {
                duration_ms: None, ..
            } => {}
            _ => {}
        }

        Some(Self {
            kind: entry_kind(&message.kind),
            severity,
            topic: message
                .notice_parts()
                .and_then(|parts| parts.topic.clone())
                .or_else(|| match &message.kind {
                    MessageKind::ProviderRetry { .. } => Some("retry".to_string()),
                    _ => None,
                }),
            provider: message.provider.clone(),
            model: message.model.clone(),
            attempt,
            sent_at_ms: message.sent_at_ms,
            extra,
            body,
        })
    }

    /// The header line, without the body.
    pub fn header(&self, ctx: &DiagnosticContext<'_>) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(10 + self.extra.len());
        parts.push(format!("kind={}", self.kind.as_str()));
        if let Some(severity) = self.severity {
            parts.push(format!("severity={}", severity_label(severity)));
        }
        if let Some(topic) = self.topic.as_deref().filter(|t| !t.is_empty()) {
            parts.push(format!("topic={}", quote(topic)));
        }
        if let Some(provider) = self.provider.as_deref().filter(|p| !p.is_empty()) {
            parts.push(format!("provider={}", quote(provider)));
        }
        if let Some(model) = self.model.as_deref().filter(|m| !m.is_empty()) {
            parts.push(format!("model={}", quote(model)));
        }
        if let Some((attempt, max)) = self.attempt {
            parts.push(format!("attempt={attempt}/{max}"));
        }
        if let Some(ms) = self.sent_at_ms {
            parts.push(format!("at={}", utc_timestamp(ms)));
        }
        if let Some(thread) = ctx.thread_id.filter(|t| !t.is_empty()) {
            parts.push(format!("thread={}", quote(thread)));
        }
        for (key, value) in &self.extra {
            parts.push(format!("{key}={}", quote(value)));
        }
        if let Some(log) = ctx.log_path.filter(|p| !p.is_empty()) {
            parts.push(format!("log={}", quote(log)));
        }
        format!("{HEADER} {}", parts.join(" "))
    }

    /// The full paste payload: header, blank line, verbatim body.
    pub fn render(&self, ctx: &DiagnosticContext<'_>) -> String {
        format!("{}\n\n{}", self.header(ctx), self.body)
    }
}

fn entry_kind(kind: &MessageKind) -> EntryKind {
    match kind {
        MessageKind::Notice { .. } => EntryKind::Notice,
        MessageKind::ProviderRetry { .. } => EntryKind::ProviderRetry,
        MessageKind::ToolStep { .. } => EntryKind::ToolStep,
        MessageKind::Reasoning { .. } => EntryKind::Reasoning,
        MessageKind::CommandResult { .. } => EntryKind::CommandResult,
        MessageKind::CompactedCard { .. } => EntryKind::CompactedCard,
        MessageKind::Text => EntryKind::Text,
    }
}

/// The step's lifecycle rung, as a stable token (`ok`, `failed`, `denied`, …).
///
/// A token rather than the renderer's display label: the header is read by
/// machines and people searching logs, so it must not follow presentation.
fn tool_status_label(status: ToolStepStatus) -> &'static str {
    match status {
        ToolStepStatus::Ingesting => "ingesting",
        ToolStepStatus::Executing => "running",
        ToolStepStatus::Ok => "ok",
        ToolStepStatus::Failed => "failed",
        ToolStepStatus::Denied => "denied",
        ToolStepStatus::Cancelled => "cancelled",
        ToolStepStatus::Interrupted => "interrupted",
    }
}

/// A command step's exit status, resolved through the same derivation the
/// renderer paints (`tools::shell_output`), so the header can never claim an
/// exit code the user does not see.
fn command_exit(
    name: &str,
    output: Option<&str>,
    structured: Option<&nuo_wire::ToolOutput>,
) -> Option<i32> {
    if !crate::tools::shell_output::is_command_body(name, structured) {
        return None;
    }
    crate::tools::shell_output::ShellOutput::derive(output.unwrap_or(""), structured).exit
}

fn severity_label(severity: NoticeSeverity) -> &'static str {
    match severity {
        NoticeSeverity::Info => "info",
        NoticeSeverity::Warning => "warning",
        NoticeSeverity::Error => "error",
    }
}

/// Quote a value that would otherwise break the space-separated header.
///
/// Only values containing whitespace (or the quote char itself) are quoted, so
/// the common header stays copy-friendly and `key=value` greps work unquoted.
fn quote(value: &str) -> String {
    if value
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\\')
    {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        value.to_string()
    }
}

fn utc_timestamp(epoch_ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(epoch_ms as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| format!("epoch-ms:{epoch_ms}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::document::TranscriptMessage;

    #[test]
    fn notice_header_carries_route_and_attempt_in_a_stable_order() {
        let mut msg = TranscriptMessage::provider_retry(
            2,
            6,
            std::time::Instant::now(),
            "OpenAI HTTP 429 Too Many Requests: {\"error\":{\"code\":\"1305\"}}",
        );
        msg.provider = Some("openai".into());
        msg.model = Some("gpt-5".into());
        msg.sent_at_ms = Some(1_790_000_000_000);

        let record = DiagnosticRecord::from_message(&msg).expect("retry entry has a body");
        let header = record.header(&DiagnosticContext::default());
        assert!(
            header.starts_with("[nuo diagnostic] kind=provider-retry severity=warning"),
            "unexpected header: {header}"
        );
        assert!(header.contains("provider=openai"));
        assert!(header.contains("model=gpt-5"));
        assert!(header.contains("attempt=2/6"));
        assert!(header.contains("at=2026-09-21T14:13:20Z"));
        assert_eq!(header.lines().count(), 1, "header must be one line");
    }

    #[test]
    fn render_separates_header_from_verbatim_body() {
        let msg = TranscriptMessage::notice(
            NoticeSeverity::Error,
            "OpenAI HTTP 500 Server Error\n{\"error\":{\"message\":\"boom\"}}",
        );
        let record = DiagnosticRecord::from_message(&msg).expect("notice has a body");
        let rendered = record.render(&DiagnosticContext {
            thread_id: Some("01HXYZ"),
            log_path: Some("/home/u/.local/share/nuo/logs/client.log"),
        });
        let (header, body) = rendered.split_once("\n\n").expect("header/body split");
        assert!(header.contains("thread=01HXYZ"));
        assert!(
            header.contains("log=/home/u/.local/share/nuo/logs/client.log"),
            "a path without whitespace stays unquoted: {header}"
        );
        assert!(body.contains("OpenAI HTTP 500 Server Error"));
    }

    #[test]
    fn values_with_spaces_are_quoted_and_escaped() {
        assert_eq!(quote("openai"), "openai");
        assert_eq!(quote("gpt-5"), "gpt-5");
        assert_eq!(quote("two words"), "\"two words\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
    }

    #[test]
    fn an_entry_without_text_yields_no_record() {
        let mut msg = TranscriptMessage::new(nuo_wire::Role::Assistant, "text");
        msg.raw = String::new();
        msg.blocks = Vec::new();
        assert!(DiagnosticRecord::from_message(&msg).is_none());
    }
}
