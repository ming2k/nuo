//! Shared helpers used across multiple overlay renderers.

use nuotc::{
    {Color, Style}, {Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::shell::render::Theme;
use nuo_ui_primitives::primitives::contrast_fg;
/// Compact relative time for space-constrained surfaces (e.g. the sessions
/// picker's meta column): `now` / `3m` / `2h` / `5d` / `3w` — no "ago" suffix.
/// Takes a caller-supplied `now` (seconds since the epoch) so a renderer can
/// read the wall clock once per frame and format many timestamps against it,
/// instead of paying a `SystemTime::now()` syscall (plus a heap `String`) for
/// every row. Used by the sessions picker, which can render hundreds of rows
/// per frame.
pub fn relative_time_at(ts: u64, now: u64) -> String {
    let diff = now.saturating_sub(ts);
    if diff < 60 {
        "now".to_string()
    } else if diff < 3600 {
        format!("{}m", diff / 60)
    } else if diff < 86_400 {
        format!("{}h", diff / 3600)
    } else if diff < 7 * 86_400 {
        format!("{}d", diff / 86_400)
    } else {
        format!("{}w", diff / (7 * 86_400))
    }
}

/// Flatten a string to a single visual line: every control character
/// (newline, carriage return, tab, …) becomes a space. Without this, a `\n` or
/// `\r` embedded in row text (e.g. a session overview built from a multi-line
/// first user message) is painted verbatim by the terminal as a carriage
/// return, dumping the rest of the row at column 0 of the *screen* — so the row
/// spills out the left edge of the modal. Collapsing to spaces keeps the row
/// inside its column budget.
pub fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Truncate prose to fit `max` display columns, keeping its head.
///
/// A thin alias over [`fit`](nuo_ui_primitives::components::overflow::fit) for
/// the many call sites whose field is prose — a session title, a description, a
/// key hint. It exists so those sites read as "this is prose", and so that any
/// field whose meaning lives at its *tail* is forced to name a different
/// strategy rather than inheriting the wrong one by habit
/// (ADR-0152 `[INV-TEXT-OVERFLOW-01]`).
///
/// A field that is an identity — a path, a wire id, a hash — must call `fit`
/// with `Start`/`Middle` instead.
pub fn truncate_ellipsis(s: &str, max: usize) -> String {
    nuo_ui_primitives::components::overflow::fit(
        s,
        max,
        nuo_ui_primitives::components::overflow::Overflow::End,
    )
    .text
}

/// Display column of the caret within a rendered input field, given its char
/// index. Each modal field renders its own masked/verbatim `display` string, so
/// mapping through chars (not bytes) keeps wide glyphs and `•` masks right. The
/// final byte→column step goes through the engine's `cursor_column`, the same
/// primitive the composer uses, so both caret sites can never disagree with the
/// grid's paint width.
pub fn caret_column(display: &str, cursor_position: usize) -> u16 {
    let byte = cursor_position.min(display.len());
    nuotc::cursor_column(display, byte).min(u16::MAX as usize) as u16
}

/// A single-line input's horizontal viewport (caret-following): the mechanism
/// that keeps long API-key / Token values from overflowing the modal. Returns
/// `(offset, text)` where `text` is the substring of `display` visible within
/// `width` display columns and `offset` is the leftmost visible column (the
/// screen caret column is the caret column minus this offset). Scrolling only
/// kicks in once the caret would leave the window, and the caret is anchored at
/// 3/4 across the window so typing forward stays as jitter-free as a normal
/// text input. Width-aware so wide glyphs never desync the budget.
pub fn field_viewport(display: &str, cursor_position: usize, width: usize) -> (usize, String) {
    if width == 0 {
        return (0, String::new());
    }
    let total = nuotc::str_len(display);
    if total <= width {
        return (0, display.to_string());
    }
    let caret_byte = cursor_position.min(display.len());
    let caret_col = nuotc::cursor_column(display, caret_byte);

    let desired_offset = caret_col.saturating_sub(width * 3 / 4);

    // Snap the left edge backward to a grapheme boundary. A viewport may
    // contain an entire grapheme or none of it; starting in the middle of a
    // combining sequence or ZWJ emoji would make the rendered text and caret
    // use different column models.
    let mut start_byte = 0usize;
    let mut offset = 0usize;
    for (byte, grapheme) in display.grapheme_indices(true) {
        let grapheme_width = nuotc::grapheme_width(grapheme) as usize;
        if offset + grapheme_width > desired_offset {
            start_byte = byte;
            break;
        }
        offset += grapheme_width;
        start_byte = byte + grapheme.len();
    }
    // A backward snap over a wide grapheme can leave the caret exactly one
    // column beyond the viewport. Advance by whole graphemes until the caret
    // itself occupies a real field cell. This intentionally allows unused
    // cells at the right edge near end-of-input; caret visibility is more
    // important than filling the viewport with preceding text.
    while caret_col.saturating_sub(offset) >= width && start_byte < display.len() {
        let Some(grapheme) = display[start_byte..].graphemes(true).next() else {
            break;
        };
        offset += nuotc::grapheme_width(grapheme) as usize;
        start_byte += grapheme.len();
    }

    let mut text = String::new();
    let mut used = 0usize;
    for grapheme in display[start_byte..].graphemes(true) {
        let grapheme_width = nuotc::grapheme_width(grapheme) as usize;
        if used + grapheme_width > width {
            break;
        }
        text.push_str(grapheme);
        used += grapheme_width;
    }
    (offset, text)
}

/// Draw the unified provider editor Two fields — API key
/// (masked) and model id — with `Tab` cycling focus. The composer input line
/// is borrowed for the focused field's value; `key_buf` / `model_buf` hold the
/// other field while it is unfocused. `field` is `0` for key, `1` for model id.
///
/// One MCP server row, unpacked for rendering. `Connected` carries the
/// per-server tool names so the MCP pane can list them rather than just a count.
#[allow(dead_code)]
pub enum McpRow {
    Connected(Vec<String>),
    Connecting,
    Disabled,
    Failed(String),
}

#[allow(dead_code)]
impl McpRow {
    pub fn connected(tools: Vec<String>) -> Self {
        Self::Connected(tools)
    }
    pub fn connecting() -> Self {
        Self::Connecting
    }
    pub fn disabled() -> Self {
        Self::Disabled
    }
    pub fn failed(reason: String) -> Self {
        Self::Failed(reason)
    }

    /// Compact one-line status for the session dashboard: a status glyph, a
    /// short word, and the color shared by both. The connected variant folds
    /// the tool count into the word so the dashboard needs no second line.
    pub fn dashboard_summary(&self, theme: &Theme) -> (String, Color, &'static str) {
        let (conn_glyph, conn_dot, fail_dot) = if !theme.elevation.is_chromatic() {
            ("...", "*", "!")
        } else {
            ("◌", "●", "●")
        };
        match self {
            Self::Connected(tools) => (
                format!("connected ({} tools)", tools.len()),
                theme.ok(),
                conn_dot,
            ),
            Self::Connecting => ("connecting…".to_string(), theme.muted(), conn_glyph),
            Self::Disabled => (
                "disabled".to_string(),
                theme.muted(),
                if !theme.elevation.is_chromatic() {
                    " "
                } else {
                    "○"
                },
            ),
            Self::Failed(reason) => (format!("failed — {reason}"), theme.err(), fail_dot),
        }
    }
}

/// Empty-list placeholder: a muted message, tuned to whether the snapshot has
/// arrived (`loaded` = true → genuinely empty; false → still loading).
pub fn placeholder(message: &str, loaded: bool, muted: Color) -> Line<'static> {
    let text = if loaded {
        message.to_string()
    } else {
        "Loading…".to_string()
    };
    Line::from(Span::styled(text, Style::default().fg(muted)))
}

/// Build a selectable list row: `▣ name  hint` with the selected row taking
/// the brand background. `state_on`/`state_off` label the enabled state shown
/// at the row's right edge; an empty `state_off` hides the badge entirely.
///
/// `max_width` caps the total row width (in display columns). When > 0 the
/// `name` is truncated with `…` so the full row fits within that budget,
/// leaving room for the mark, hint, and state badge. Pass 0 to disable
/// truncation (legacy behaviour).
#[allow(clippy::too_many_arguments)]
pub fn selectable_row(
    i: usize,
    selected: usize,
    name: &str,
    hint: &str,
    enabled: bool,
    state_on: &str,
    state_off: &str,
    max_width: usize,
    theme: &Theme,
) -> Line<'static> {
    let is_selected = i == selected;
    let mark = if !theme.elevation.is_chromatic() {
        if enabled { "[*]" } else { "[ ]" }
    } else if enabled {
        "●"
    } else {
        "○"
    };
    let state = if enabled { state_on } else { state_off };

    let mark_w = mark.width() + 1; // mark + space
    let state_w = if state.is_empty() {
        0
    } else {
        4 + state.width() // "  [" + state + "]"
    };

    let name_budget = if max_width > 0 {
        max_width.saturating_sub(mark_w + state_w)
    } else {
        name.width()
    };
    let name_text = if max_width > 0 {
        truncate_ellipsis(name, name_budget.max(1))
    } else {
        name.to_string()
    };

    let hint_budget = name_budget
        .saturating_sub(name_text.width())
        .saturating_sub(2);
    let hint_text = if max_width > 0 {
        truncate_ellipsis(hint, hint_budget)
    } else {
        hint.to_string()
    };

    let (row_bg, row_fg, row_muted) = if is_selected {
        if !theme.elevation.is_chromatic() {
            (theme.panel(), theme.fg(), theme.fg())
        } else {
            (
                theme.brand(),
                contrast_fg(theme.brand()),
                contrast_fg(theme.brand()),
            )
        }
    } else {
        (theme.panel(), theme.fg(), theme.muted())
    };

    let mut row_style = Style::default().bg(row_bg).fg(row_fg);
    let mut muted_style = Style::default().bg(row_bg).fg(row_muted);
    if is_selected && !theme.elevation.is_chromatic() {
        row_style = row_style.add_modifier(nuotc::Modifier::REVERSE | nuotc::Modifier::BOLD);
        muted_style = muted_style.add_modifier(nuotc::Modifier::REVERSE);
    }

    let mut spans = vec![
        Span::styled(format!("{} ", mark), row_style),
        Span::styled(name_text, row_style),
    ];
    if !hint_text.is_empty() {
        spans.push(Span::styled(format!("  {}", hint_text), muted_style));
    }
    if !state.is_empty() {
        spans.push(Span::styled(format!("  [{}]", state), muted_style));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_column_floors_char_positions_inside_graphemes() {
        let combining = "e\u{301}x";
        assert_eq!(caret_column(combining, 1), 0);
        assert_eq!(caret_column(combining, 3), 1);

        let family = "👨‍👩‍👧‍👦";
        assert_eq!(caret_column(family, 1), 0);
        assert_eq!(caret_column(family, family.len()), 2);
    }

    #[test]
    fn field_viewport_never_splits_a_zwj_grapheme() {
        let family = "👨‍👩‍👧‍👦";
        let display = format!("abcd{family}xyz");
        let cursor = format!("abcd{family}").len();
        let (offset, visible) = field_viewport(&display, cursor, 4);

        assert_eq!(offset, 3);
        assert_eq!(visible, format!("d{family}x"));
        assert_eq!(nuotc::str_len(&visible), 4);
    }

    #[test]
    fn field_viewport_keeps_end_caret_inside_narrow_cjk_field() {
        let display = "abcd中文";
        let (offset, visible) = field_viewport(display, display.len(), 3);
        let local_caret = caret_column(display, display.len()) as usize - offset;

        assert_eq!(offset, 6);
        assert_eq!(visible, "文");
        assert!(local_caret < 3, "caret must occupy a real field cell");
    }

    #[test]
    fn field_viewport_preserves_combining_cluster_at_left_edge() {
        let display = "abcde\u{301}xyz";
        let cursor = display.len();
        let (_, visible) = field_viewport(display, cursor, 4);
        assert!(
            !visible.starts_with('\u{301}'),
            "viewport must never begin with a detached combining mark"
        );
    }

    #[test]
    fn selectable_row_monochrome_uses_brackets_and_reverse_video() {
        let theme_mono = Theme::monochrome();
        let selected_line = selectable_row(
            0,
            0,
            "read_text",
            "Read file contents",
            true,
            "enabled",
            "disabled",
            60,
            &theme_mono,
        );
        let joined_text: String = selected_line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            joined_text.contains("[*]"),
            "monochrome uses [*] mark: {joined_text}"
        );
        assert!(!joined_text.contains('●'), "no unicode dot: {joined_text}");

        // Selected line must have REVERSE video
        for span in &selected_line.spans {
            assert!(
                span.style.add.contains(nuotc::Modifier::REVERSE),
                "selected span must have REVERSE video: {span:?}"
            );
        }

        let unselected_line = selectable_row(
            1,
            0,
            "write_file",
            "Write file contents",
            false,
            "enabled",
            "disabled",
            60,
            &theme_mono,
        );
        let unsel_text: String = unselected_line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            unsel_text.contains("[ ]"),
            "monochrome disabled uses [ ]: {unsel_text}"
        );
        for span in &unselected_line.spans {
            assert!(
                !span.style.add.contains(nuotc::Modifier::REVERSE),
                "unselected span must NOT have REVERSE: {span:?}"
            );
        }
    }
}
