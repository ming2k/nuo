//! The single producer and reader of the model-facing tool-result header.
//!
//! Every tool result the model reads begins with `[<name> result]:`. Before this
//! module, five sites formatted that header inline and four sites re-parsed it
//! with `split_once(" result]:")` — including one provider that invented a sixth
//! spelling on a degraded path (ADR-0227).
//!
//! # Why a header at all
//!
//! Tool results are already protocol fields: the *role* (`Tool`) and the
//! `tool_call_id` identify the call, so the header is redundant metadata for a
//! native provider. It exists because two model-facing cases need it:
//!
//! 1. A **degraded** projection — a provider that cannot express a tool role for
//!    this turn falls back to a user-role message, and the header is the only
//!    thing that still names the call.
//! 2. **Human** readers — the transcript renderer and the Markdown export strip
//!    this header to recover the tool name.
//!
//! It is therefore presentation, not protocol: nothing may parse it to recover
//! state that the protocol already carries. [`parse`] exists solely for the two
//! human-facing readers above.

/// Suffix that terminates the header. Both halves are public so a reader never
/// spells them.
const HEADER_SUFFIX: &str = " result]:";

/// Render `body` under the canonical header for `tool`.
pub fn render(tool: &str, body: &str) -> String {
    format!("[{tool}{HEADER_SUFFIX}\n{body}")
}

/// Render `body` under the canonical header for `tool`, followed by a blank line
/// and a trailing note.
///
/// Used where a result carries a deterministic re-anchor or a spool pointer: the
/// note is part of the body, and the header still appears exactly once.
pub fn render_with_note(tool: &str, body: &str, note: &str) -> String {
    format!("[{tool}{HEADER_SUFFIX}\n{body}\n\n{note}")
}

/// Recover `(tool, body)` from a message that starts with the header.
///
/// Returns `None` for a message without one — a tool result the runtime never
/// formatted, or prose. The body has exactly one leading newline removed, which
/// is what [`render`] inserted; everything else is preserved byte for byte.
pub fn parse(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix('[')?;
    let (tool, body) = rest.split_once(HEADER_SUFFIX)?;
    Some((tool, body.strip_prefix('\n').unwrap_or(body)))
}

/// Whether `content` begins with a tool-result header.
pub fn is_tool_result(content: &str) -> bool {
    parse(content).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_and_parse_round_trip() {
        let rendered = render("read", "file contents");
        assert_eq!(rendered, "[read result]:\nfile contents");
        assert_eq!(parse(&rendered), Some(("read", "file contents")));
    }

    #[test]
    fn a_trailing_note_is_part_of_the_body() {
        let rendered = render_with_note("subagent", "summary", "[system] reanchor");
        assert_eq!(
            parse(&rendered),
            Some(("subagent", "summary\n\n[system] reanchor"))
        );
    }

    #[test]
    fn parse_rejects_anything_without_the_header() {
        assert_eq!(parse("plain prose"), None);
        assert_eq!(parse("[not a result]"), None);
        assert_eq!(parse(""), None);
        assert!(!is_tool_result("Read src/main.rs"));
        assert!(is_tool_result("[read result]:\nx"));
    }

    #[test]
    fn a_tool_name_may_contain_dashes_and_underscores() {
        let rendered = render("mcp__github__list_resources", "ok");
        assert_eq!(
            parse(&rendered),
            Some(("mcp__github__list_resources", "ok"))
        );
    }

    #[test]
    fn only_one_leading_newline_is_consumed() {
        let rendered = "[read result]:\n\nindented body";
        assert_eq!(parse(rendered), Some(("read", "\nindented body")));
    }
}
