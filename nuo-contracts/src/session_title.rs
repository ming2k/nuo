//! Session-level AI title: domain vocabulary for the titling subagent
//! (ADR-0022).
//!
//! Follows the bounded-subagent split of the retired `session_review` (ADR-0016):
//! domain types and pure helpers live here in `muta-contracts`, while the
//! LLM-backed subagent that drives the `TITLE` profile lives in
//! `muta-agent`. There is no trait here because a
//! title is a single concept rather than a set of extensible dimensions — the
//! only shared logic is the pure post-processing that turns a model's free-form
//! answer into a valid title string.
//!
//! ## Lifecycle
//!
//! Generation is *generate-once-then-stable*: the subagent fires automatically on
//! the first turn (when the transcript holds exactly one real user message)
//! and on demand via `/title`. Whether a stored title may be overwritten by AI
//! generation is a persistence concern, not a domain one — since ADR-0186 a
//! non-`NULL` stored title is terminal (the manual-title flag was retired), so
//! generation only fills `None`; the caller decides whether to keep it.

/// Maximum character length of a generated title. Matches the constraint
/// encoded in the `TITLE` profile's system prompt. Titles
/// longer than this are hard-truncated with an ellipsis so the session picker
/// row stays bounded regardless of what the model returns.
pub const TITLE_MAX_LEN: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTitle;

impl SessionTitle {
    /// Maximum character length of a generated title.
    pub const MAX_LEN: usize = TITLE_MAX_LEN;

    /// Clean a raw model title output.
    pub fn clean(raw: &str) -> Option<String> {
        clean_title(raw)
    }
}

/// Turn a model's raw title response into a valid title string, or `None` when
/// there is nothing usable.
///
/// Post-processing mirrors the cleanup opencode applies (`prompt.ts`
/// `ensureTitle`): strip a surrounding code fence if the model wrapped its
/// answer, drop `<think>…</think>` reasoning blocks some models emit, take the
/// first non-empty trimmed line, and hard-cap at [`TITLE_MAX_LEN`] characters
/// (appending `…` on truncation). Returns `None` for an empty result so the
/// caller can leave the stored title untouched rather than storing an empty
/// string.
pub fn clean_title(raw: &str) -> Option<String> {
    // Drop <think>…</think> blocks (non-greedy, multi-line). Some local models
    // prepend reasoning despite the instruction to output only the title.
    let no_think = strip_think_blocks(raw);
    // Strip a single surrounding ``` fence (with optional language tag) so a
    // model that wrapped its output is still parsed.
    let unfenced = strip_code_fence(&no_think);
    // The title is the first non-empty line: a model that added a trailing
    // newline or a second sentence keeps only the headline.
    let line = unfenced
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let trimmed = line.trim().trim_matches('"').trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_title(trimmed))
}

/// Hard-cap a title at [`TITLE_MAX_LEN`] characters, appending `…` when
/// truncated. Operates on `char` count so multi-byte content is not split
/// mid-codepoint.
fn truncate_title(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= TITLE_MAX_LEN {
        return text.to_string();
    }
    let mut out: String = chars
        .into_iter()
        .take(TITLE_MAX_LEN.saturating_sub(1))
        .collect();
    out.push('…');
    out
}

/// Remove non-greedy `<think>…</think>` spans. Repeated so multiple blocks are
/// all stripped. Leaves content outside the tags untouched.
fn strip_think_blocks(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    loop {
        if let Some(start) = rest.find("<think>") {
            out.push_str(&rest[..start]);
            let after_open = &rest[start + "<think>".len()..];
            match after_open.find("</think>") {
                Some(end) => {
                    rest = &after_open[end + "</think>".len()..];
                }
                // Unterminated <think>: drop the rest, keep what we have.
                None => break,
            }
        } else {
            out.push_str(rest);
            break;
        }
    }
    out
}

/// Strip a single surrounding ``` fence if present (with an optional language
/// tag like ```json). Only the outermost fence is removed so nested content is
/// preserved.
fn strip_code_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Skip an optional language tag on the opening fence (```json).
    let after_tag = match after_open.find('\n') {
        Some(idx) => &after_open[idx + 1..],
        None => after_open,
    };
    if let Some(end) = after_tag.rfind("```") {
        after_tag[..end].trim()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_title_keeps_a_plain_answer() {
        assert_eq!(
            clean_title("Fix login button on mobile").as_deref(),
            Some("Fix login button on mobile")
        );
    }

    #[test]
    fn clean_title_takes_first_non_empty_line() {
        assert_eq!(
            clean_title("\n\n  Fix login button  \nsecond line").as_deref(),
            Some("Fix login button")
        );
    }

    #[test]
    fn clean_title_returns_none_for_whitespace_only() {
        assert!(clean_title("   \n\t  ").is_none());
    }

    #[test]
    fn clean_title_strips_code_fence_with_language_tag() {
        assert_eq!(
            clean_title("```json\n\"Refactor auth middleware\"\n```").as_deref(),
            Some("Refactor auth middleware")
        );
    }

    #[test]
    fn clean_title_strips_surrounding_quotes() {
        assert_eq!(
            clean_title("\"Debug failing CI tests\"").as_deref(),
            Some("Debug failing CI tests")
        );
    }

    #[test]
    fn clean_title_strips_leading_think_block() {
        let raw = "<think>the user wants auth</think>Auth middleware refactor";
        assert_eq!(
            clean_title(raw).as_deref(),
            Some("Auth middleware refactor")
        );
    }

    #[test]
    fn clean_title_strips_multiple_think_blocks() {
        let raw = "<think>a</think>Rate limiting<think>b</think>\nimplementation";
        assert_eq!(clean_title(raw).as_deref(), Some("Rate limiting"));
    }

    #[test]
    fn clean_title_drops_content_after_unterminated_think() {
        // Unterminated <think>: the trailing text is discarded, but earlier
        // content survives.
        assert_eq!(clean_title("Done <think>rambling").as_deref(), Some("Done"));
    }

    #[test]
    fn clean_title_truncates_overlong_with_ellipsis() {
        let long = "a".repeat(TITLE_MAX_LEN + 20);
        let out = clean_title(&long).expect("a title");
        let chars: Vec<char> = out.chars().collect();
        assert_eq!(chars.len(), TITLE_MAX_LEN);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn clean_title_does_not_truncate_at_boundary() {
        let exact: String = "a".repeat(TITLE_MAX_LEN);
        assert_eq!(clean_title(&exact).as_deref(), Some(exact.as_str()));
    }
}
