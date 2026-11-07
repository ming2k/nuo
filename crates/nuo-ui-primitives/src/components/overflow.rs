//! Universal text overflow: the one implementation of "this string does not fit".
//!
//! ADR-0152 `[INV-TEXT-OVERFLOW-01]`. Before this module the substrate had a
//! single truncation shape — cut the head, append `…` — applied to every field
//! it drew. That is right for prose and wrong for identity strings: a wire id
//! like `claude-3-7-sonnet-20250219` head-truncated renders `claude-3-7-so…`,
//! destroying the version and date, which are the only parts that distinguish
//! it from its predecessor. The same holds for git hashes, semver tags, token
//! prefixes and file paths.
//!
//! So the strategy is chosen by the **semantics** of the field, not by the call
//! site's convenience:
//!
//! | Field | Strategy | Why |
//! | :--- | :--- | :--- |
//! | prose, display names, titles | [`Overflow::End`] | the meaning lives at the head |
//! | wire ids, hashes, semver, tokens | [`Overflow::Middle`] | both ends discriminate |
//! | paths, breadcrumbs | [`Overflow::Start`] | the leaf is the identity |
//! | the focused row's overflowing cell | [`Overflow::Window`] | reveal, don't destroy |
//!
//! Three properties are guaranteed and tested:
//!
//! 1. **Never over budget.** The fitted text's display width is `<= budget` for
//!    every strategy, including budgets of 0 and 1 and including wide (CJK)
//!    glyphs, which are never split.
//! 2. **Never breaks a row.** Control graphemes are flattened to one space, so a
//!    fitted cell cannot smuggle a `\n` into the grid and dump the rest of the
//!    row at column 0 of the screen. ([`fit_line`] is the one deliberate
//!    exception: it stops at the first break instead of flattening.)
//! 3. **Evidence, not just a string.** [`Fitted`] reports what it hid and where,
//!    so a fuzzy-match highlight can be rebased onto the painted text instead of
//!    silently highlighting the wrong glyph.

use std::borrow::Cow;
use std::collections::HashSet;

use nuotc::{Span, Style};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// The default ellipsis glyph, used when a caller has no theme in hand.
///
/// Surfaces that *do* hold a theme pass `theme.glyphs.ellipsis` instead, so an
/// ASCII terminal degrades to `...` along with the rest of the chrome
/// ([`fit_with`]).
pub const ELLIPSIS: &str = "…";

/// How a string degrades when it does not fit its column.
///
/// Chosen by the field's semantics — see the module docs for the mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overflow {
    /// Keep the head, drop the tail: `Deployment Configur…`. Prose, display
    /// names, titles, descriptions.
    #[default]
    End,
    /// Keep both ends, drop the middle: `claude…20250219`. Identity strings
    /// whose head names the family and whose tail names the version.
    Middle,
    /// Keep the tail, drop the head: `…/components/overflow.rs`. Paths and
    /// breadcrumbs, where the leaf is the identity.
    Start,
    /// Slide a window of `budget` columns through the string, starting `offset`
    /// columns in. Paints no ellipsis: the text is not being shortened, it is
    /// being **revealed**. Reserved for the focused row
    /// (`[INV-TUI-MARQUEE-01]`).
    Window { offset: usize },
}

impl Overflow {
    /// The narrowest budget at which this strategy still says something true.
    ///
    /// Below this a column should drop rather than paint a fragment: `de…` on a
    /// wire id reads as a *different* model, not as an abbreviation of
    /// `deepseek-flash`. A [`ColumnSpec`](super::columns::ColumnSpec) floor is
    /// the caller's own (usually stricter) answer; this is the strategy's.
    pub const fn min_width(self) -> usize {
        match self {
            Overflow::End => 6,
            Overflow::Middle => 8,
            Overflow::Start => 6,
            Overflow::Window { .. } => 1,
        }
    }
}

/// The result of fitting a string into a budget: the text to paint, plus the
/// evidence of what was hidden — enough to rebase highlight indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fitted {
    /// Exactly what to paint. Its display width is `<= budget`.
    pub text: String,
    /// Columns the source occupies that the painted text does not show.
    /// `0` means the text fits and nothing was hidden.
    pub hidden: usize,
    /// The strategy that produced this fit.
    pub overflow: Overflow,
    /// Characters kept at the head of the painted text.
    pub head_chars: usize,
    /// Characters kept at the tail of the painted text.
    pub tail_chars: usize,
    /// Characters dropped between head and tail (middle ellipsis), or skipped
    /// from the head ([`Overflow::Window`]).
    pub skipped_chars: usize,
    /// Characters in the source string.
    pub total_chars: usize,
    /// Whether a leading marker is painted because text is hidden *before* the
    /// window ([`Overflow::Window`] only). A window that hides text without
    /// saying so is a lie: the reader would take a mid-string fragment for the
    /// whole value.
    pub lead_marker: bool,
}

impl Fitted {
    /// Whether the fit hid anything.
    pub fn truncated(&self) -> bool {
        self.hidden > 0
    }

    /// Map a character index in the **source** string onto the painted string.
    ///
    /// Returns `None` when that character is not painted at all — the caller
    /// then highlights nothing rather than the wrong glyph. Indices are counted
    /// in `char`s, the same unit
    /// [`RowGroup::matched_text`](super::row::RowGroup::matched_text) enumerates.
    pub fn rebase(&self, source_index: usize) -> Option<usize> {
        if self.hidden == 0 {
            return (source_index < self.total_chars).then_some(source_index);
        }
        match self.overflow {
            Overflow::Window { .. } => {
                let lo = self.skipped_chars;
                let hi = lo + self.head_chars;
                let lead = usize::from(self.lead_marker);
                (source_index >= lo && source_index < hi).then(|| lead + source_index - lo)
            }
            Overflow::End => (source_index < self.head_chars).then_some(source_index),
            Overflow::Start => {
                let tail_start = self.total_chars.saturating_sub(self.tail_chars);
                (source_index >= tail_start).then(|| 1 + source_index - tail_start)
            }
            Overflow::Middle => {
                if source_index < self.head_chars {
                    Some(source_index)
                } else {
                    let tail_start = self.total_chars.saturating_sub(self.tail_chars);
                    (source_index >= tail_start)
                        .then(|| self.head_chars + 1 + source_index - tail_start)
                }
            }
        }
    }
}

/// One grapheme of the source, with the width and char count it contributes.
struct Unit<'a> {
    text: &'a str,
    width: usize,
    chars: usize,
    /// A control grapheme (newline, tab, …) is painted as one space so it can
    /// never break the row it lands in.
    control: bool,
}

fn units(text: &str) -> Vec<Unit<'_>> {
    text.graphemes(true)
        .map(|g| {
            let control = g.chars().all(char::is_control);
            Unit {
                text: g,
                width: if control { 1 } else { g.width() },
                chars: g.chars().count(),
                control,
            }
        })
        .collect()
}

fn push_unit(out: &mut String, u: &Unit<'_>) {
    if u.control {
        out.push(' ');
    } else {
        out.push_str(u.text);
    }
}

fn unit_width(units: &[Unit<'_>]) -> usize {
    units.iter().map(|u| u.width).sum()
}

/// Fit `text` into `budget` display columns using `overflow`.
///
/// The returned text's width is always `<= budget`; wide glyphs are never
/// split; control graphemes are flattened to one space.
pub fn fit(text: &str, budget: usize, overflow: Overflow) -> Fitted {
    fit_with(text, budget, overflow, ELLIPSIS)
}

/// [`fit`] with a caller-chosen ellipsis glyph.
///
/// The marker belongs to the theme, not to this module: a terminal on the ASCII
/// glyph set must degrade `…` to `...` everywhere at once, so the one overflow
/// implementation takes the glyph as an argument rather than baking in a
/// character the theme cannot reach.
pub fn fit_with(text: &str, budget: usize, overflow: Overflow, ellipsis: &str) -> Fitted {
    let units = units(text);
    let total_chars: usize = units.iter().map(|u| u.chars).sum();
    let total_width = unit_width(&units);
    let plain = !units.iter().any(|u| u.control);

    if total_width <= budget {
        if plain {
            return Fitted {
                text: text.to_string(),
                hidden: 0,
                overflow,
                head_chars: total_chars,
                tail_chars: 0,
                skipped_chars: 0,
                total_chars,
                lead_marker: false,
            };
        }
        // It fits, but carries control graphemes: paint the flattened form so
        // the row can never break, and report nothing hidden.
        let mut flattened = String::new();
        for u in &units {
            push_unit(&mut flattened, u);
        }
        return Fitted {
            text: flattened,
            hidden: 0,
            overflow,
            head_chars: total_chars,
            tail_chars: 0,
            skipped_chars: 0,
            total_chars,
            lead_marker: false,
        };
    }

    if budget == 0 {
        return Fitted {
            text: String::new(),
            hidden: total_width,
            overflow,
            head_chars: 0,
            tail_chars: 0,
            skipped_chars: total_chars,
            total_chars,
            lead_marker: false,
        };
    }

    if let Overflow::Window { offset } = overflow {
        return window_fit(
            &units,
            budget,
            offset,
            total_width,
            total_chars,
            overflow,
            ellipsis,
        );
    }

    if budget == 1 {
        return Fitted {
            text: ellipsis.to_string(),
            hidden: total_width,
            overflow,
            head_chars: 0,
            tail_chars: 0,
            skipped_chars: total_chars,
            total_chars,
            lead_marker: false,
        };
    }

    let avail = budget.saturating_sub(ellipsis.width()); // the marker's columns
    let mut out = String::new();
    let (head_chars, tail_chars) = match overflow {
        Overflow::End => {
            let head = take_head(&units, avail, &mut out);
            out.push_str(ellipsis);
            (head, 0)
        }
        Overflow::Start => {
            let tail = take_tail(&units, avail, &mut out);
            out.insert_str(0, ellipsis);
            (0, tail)
        }
        Overflow::Middle => {
            let left_budget = avail / 2;
            let right_budget = avail - left_budget;
            let head = take_head(&units, left_budget, &mut out);
            let mut tail = String::new();
            let tail_chars = take_tail(&units, right_budget, &mut tail);
            out.push_str(ellipsis);
            out.push_str(&tail);
            (head, tail_chars)
        }
        Overflow::Window { .. } => unreachable!("handled above"),
    };

    Fitted {
        text: out,
        hidden: total_width.saturating_sub(avail),
        overflow,
        head_chars,
        tail_chars,
        skipped_chars: total_chars.saturating_sub(head_chars + tail_chars),
        total_chars,
        lead_marker: false,
    }
}

/// Take as many head graphemes as fit `budget`, appending them to `out`.
/// Returns the number of source characters consumed.
fn take_head(units: &[Unit<'_>], budget: usize, out: &mut String) -> usize {
    let mut width = 0usize;
    let mut chars = 0usize;
    for u in units {
        if width + u.width > budget {
            break;
        }
        push_unit(out, u);
        width += u.width;
        chars += u.chars;
    }
    chars
}

/// Take as many tail graphemes as fit `budget`, **prepending** them to `out` in
/// source order. Returns the number of source characters consumed.
fn take_tail(units: &[Unit<'_>], budget: usize, out: &mut String) -> usize {
    let mut width = 0usize;
    let mut chars = 0usize;
    let mut start = units.len();
    for (i, u) in units.iter().enumerate().rev() {
        if width + u.width > budget {
            break;
        }
        width += u.width;
        chars += u.chars;
        start = i;
    }
    let mut tail = String::new();
    for u in &units[start..] {
        push_unit(&mut tail, u);
    }
    out.push_str(&tail);
    chars
}

/// The largest useful [`Overflow::Window`] offset for `total` columns of text in
/// a `budget`-column cell.
///
/// One column is held back for the leading marker, so the last offset shows the
/// true tail *and* still says that the head is hidden. Without that reservation
/// the very end of a string would be the one position where the reader is not
/// told anything was skipped.
pub fn window_max_offset(total: usize, budget: usize, ellipsis_width: usize) -> usize {
    total.saturating_sub(budget.saturating_sub(ellipsis_width))
}

fn window_fit(
    units: &[Unit<'_>],
    budget: usize,
    offset: usize,
    total_width: usize,
    total_chars: usize,
    overflow: Overflow,
    ellipsis: &str,
) -> Fitted {
    let marker_w = ellipsis.width().min(budget);
    let offset = offset.min(window_max_offset(total_width, budget, marker_w));
    let lead_marker = offset > 0 && marker_w > 0;
    let lead_w = if lead_marker { marker_w } else { 0 };

    // Walk in from `offset`.
    let mut skipped_width = 0usize;
    let mut skipped_chars = 0usize;
    let mut start = 0usize;
    for (i, u) in units.iter().enumerate() {
        if skipped_width + u.width > offset {
            start = i;
            break;
        }
        skipped_width += u.width;
        skipped_chars += u.chars;
        start = i + 1;
    }

    let take = |budget: usize| {
        let mut content: Vec<&Unit<'_>> = Vec::new();
        let mut width = 0usize;
        for u in &units[start..] {
            if width + u.width > budget {
                break;
            }
            content.push(u);
            width += u.width;
        }
        (content, width)
    };

    // First pass: hold one marker column back, so a window that stops short of
    // the end can *say* it did.
    let (mut content, mut width) = take(budget.saturating_sub(lead_w + marker_w));
    // Second pass: if that reserve would have reached the end anyway, spend it on
    // real text instead — at the tail there is nothing left to disclose.
    let (extended, extended_width) = take(budget.saturating_sub(lead_w));
    if skipped_width + extended_width >= total_width {
        content = extended;
        width = extended_width;
    }

    let reached_end = skipped_width + width >= total_width;
    let tail_marker =
        !reached_end && marker_w > 0 && lead_w + width + marker_w <= budget && width > 0;

    let chars = content.iter().map(|u| u.chars).sum::<usize>();
    let mut text = String::new();
    if lead_marker {
        text.push_str(ellipsis);
    }
    for u in &content {
        push_unit(&mut text, u);
    }
    if tail_marker {
        text.push_str(ellipsis);
    }

    Fitted {
        text,
        hidden: total_width.saturating_sub(width),
        overflow,
        head_chars: chars,
        tail_chars: 0,
        skipped_chars,
        total_chars,
        lead_marker,
    }
}

/// Fit a **single-line** string into `budget`, stopping at the first line break
/// instead of flattening it.
///
/// This is the one place a break is not flattened: a transcript fold summary
/// that spans lines must be cut at the newline, because flattening it would
/// splice two unrelated sentences into one run-on. Everywhere else use
/// [`fit`], whose flattening guarantees a row can never break.
pub fn fit_line(text: &str, budget: usize, overflow: Overflow) -> Fitted {
    fit_line_with(text, budget, overflow, ELLIPSIS)
}

/// [`fit_line`] with a caller-chosen ellipsis glyph.
pub fn fit_line_with(text: &str, budget: usize, overflow: Overflow, ellipsis: &str) -> Fitted {
    if !text.contains(['\n', '\r']) {
        return fit_with(text, budget, overflow, ellipsis);
    }
    let head: String = text
        .char_indices()
        .take_while(|(_, c)| *c != '\n' && *c != '\r')
        .map(|(_, c)| c)
        .collect();
    let mut fitted = fit_with(&head, budget, overflow, ellipsis);
    // Cutting at the break is a truncation, and a truncation must say so: the
    // reader has to know the summary continues, otherwise the fold looks like it
    // is quoting the whole thing. One column for the ellipsis, if the budget can
    // spare it.
    if !fitted.truncated() && head.width() + ellipsis.width() <= budget {
        fitted.text.push_str(ellipsis);
        fitted.hidden = text.width() - head.width();
    }
    fitted
}

/// Timing for [`Overflow::Window`] reveal. Declarative and time-based, never
/// frame-count-based, so the cadence is identical on any frame rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarqueeCadence {
    /// Rest before the window starts moving, so the head is readable.
    pub hold_ms: u64,
    /// Milliseconds per column of travel.
    pub step_ms: u64,
    /// Rest at each end before reversing.
    pub tail_hold_ms: u64,
}

impl Default for MarqueeCadence {
    fn default() -> Self {
        Self {
            hold_ms: 900,
            step_ms: 180,
            tail_hold_ms: 700,
        }
    }
}

/// The window offset for a focused cell at `elapsed_ms`, ping-ponging between
/// the head and the tail so the reader sees both ends without the text ever
/// jumping.
///
/// Returns `0` when nothing overflows — a cell that fits never moves.
pub fn marquee_offset(max_offset: usize, elapsed_ms: u64, cadence: MarqueeCadence) -> usize {
    if max_offset == 0 {
        return 0;
    }
    let max = max_offset as u64;
    let step = cadence.step_ms.max(1);
    let travel = max * step;
    let forward = cadence.hold_ms + travel + cadence.tail_hold_ms;
    let cycle = forward + travel + cadence.tail_hold_ms;

    let t = elapsed_ms % cycle;
    if t < cadence.hold_ms {
        0
    } else if t < cadence.hold_ms + travel {
        (((t - cadence.hold_ms) / step) + 1).min(max) as usize
    } else if t < forward {
        max_offset
    } else {
        let back = t - forward;
        if back < travel {
            max.saturating_sub((back / step) + 1) as usize
        } else {
            0
        }
    }
}

/// A component that knows how to present itself inside whatever width it is
/// given — the terminal's container query (`[INV-ROW-GRID-02]`).
///
/// A parent hands a budget; the child decides what its own narrow form is. A
/// parent must never slice a child's text on the child's behalf, because the
/// parent cannot see the child's semantics (which end of the string matters).
pub trait WidthAware {
    /// The width this component would occupy if space were free.
    fn demand(&self) -> usize;
    /// The narrowest budget at which it still says something true.
    fn min_width(&self) -> usize;
    /// Render into **exactly** `budget` columns' worth of spans (never wider).
    fn render_width(&self, budget: usize) -> Vec<Span<'static>>;
}

/// The canonical [`WidthAware`] component: one semantic string that renders
/// itself into a column budget, keeping its fuzzy-match highlights aligned with
/// the text it actually paints.
#[derive(Clone, Debug)]
pub struct TextCell<'a> {
    text: Cow<'a, str>,
    overflow: Overflow,
    style: Style,
    matched_style: Style,
    matched: Option<&'a HashSet<usize>>,
    ellipsis: &'a str,
}

impl<'a> TextCell<'a> {
    /// A cell with no highlight positions.
    pub fn new(text: impl Into<Cow<'a, str>>, overflow: Overflow, style: Style) -> Self {
        Self {
            text: text.into(),
            overflow,
            style,
            matched_style: style,
            matched: None,
            ellipsis: ELLIPSIS,
        }
    }

    /// Paint this cell's marker with the theme's own ellipsis glyph, so an ASCII
    /// terminal degrades to `...` along with the rest of the chrome.
    pub fn ellipsis(mut self, ellipsis: &'a str) -> Self {
        self.ellipsis = ellipsis;
        self
    }

    /// Attach fuzzy-match positions (character indices into [`Self::text`]) and
    /// the style to paint them with.
    pub fn highlighted(mut self, matched: Option<&'a HashSet<usize>>, style: Style) -> Self {
        self.matched = matched.filter(|m| !m.is_empty());
        self.matched_style = style;
        self
    }

    /// The cell's source text, unmodified.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Fit the cell into `budget` columns.
    pub fn fitted(&self, budget: usize) -> Fitted {
        fit_with(&self.text, budget, self.overflow, self.ellipsis)
    }
}

impl WidthAware for TextCell<'_> {
    fn demand(&self) -> usize {
        self.text.width()
    }

    fn min_width(&self) -> usize {
        self.overflow.min_width()
    }

    fn render_width(&self, budget: usize) -> Vec<Span<'static>> {
        let fitted = self.fitted(budget);
        if fitted.text.is_empty() {
            return Vec::new();
        }
        let Some(matched) = self.matched else {
            return vec![Span::styled(fitted.text, self.style)];
        };

        // Paint character by character so each glyph can carry its own style,
        // with the source indices rebased onto the painted text.
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut pending = String::new();
        let mut pending_matched = false;
        for (painted_idx, ch) in fitted.text.chars().enumerate() {
            let is_matched = fitted
                .rebase_source_from_painted(painted_idx)
                .is_some_and(|src| matched.contains(&src));
            if is_matched != pending_matched && !pending.is_empty() {
                let style = if pending_matched {
                    self.matched_style
                } else {
                    self.style
                };
                spans.push(Span::styled(std::mem::take(&mut pending), style));
            }
            pending_matched = is_matched;
            pending.push(ch);
        }
        if !pending.is_empty() {
            let style = if pending_matched {
                self.matched_style
            } else {
                self.style
            };
            spans.push(Span::styled(pending, style));
        }
        spans
    }
}

impl Fitted {
    /// Inverse of [`Fitted::rebase`]: map a painted character index back onto
    /// the source string.
    fn rebase_source_from_painted(&self, painted_index: usize) -> Option<usize> {
        if self.hidden == 0 {
            return Some(painted_index);
        }
        match self.overflow {
            Overflow::Window { .. } => {
                let lead = usize::from(self.lead_marker);
                (painted_index >= lead).then(|| self.skipped_chars + painted_index - lead)
            }
            Overflow::End => (painted_index < self.head_chars).then_some(painted_index),
            Overflow::Start => {
                let tail_start = self.total_chars.saturating_sub(self.tail_chars);
                (painted_index >= 1).then(|| tail_start + painted_index - 1)
            }
            Overflow::Middle => {
                if painted_index < self.head_chars {
                    Some(painted_index)
                } else if painted_index > self.head_chars {
                    let tail_start = self.total_chars.saturating_sub(self.tail_chars);
                    Some(tail_start + painted_index - self.head_chars - 1)
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> usize {
        s.width()
    }

    #[test]
    fn a_fitting_string_is_returned_verbatim() {
        let f = fit("gpt-4o", 20, Overflow::End);
        assert_eq!(f.text, "gpt-4o");
        assert_eq!(f.hidden, 0);
        assert!(!f.truncated());
    }

    #[test]
    fn end_keeps_the_head_and_middle_keeps_both_ends() {
        let src = "claude-3-7-sonnet-20250219";
        let end = fit(src, 14, Overflow::End);
        let mid = fit(src, 14, Overflow::Middle);
        assert!(end.text.starts_with("claude"), "{:?}", end.text);
        assert!(mid.text.starts_with("claude"), "{:?}", mid.text);
        assert!(
            mid.text.ends_with("20250219") || mid.text.ends_with("50219"),
            "middle keeps the version tail: {:?}",
            mid.text
        );
        assert!(end.text.ends_with(ELLIPSIS));
        assert!(mid.text.contains(ELLIPSIS));
    }

    #[test]
    fn start_keeps_the_leaf() {
        let f = fit(
            "crates/nuo-ui-primitives/src/components/overflow.rs",
            20,
            Overflow::Start,
        );
        assert!(f.text.starts_with(ELLIPSIS), "{:?}", f.text);
        assert!(f.text.ends_with("overflow.rs"), "{:?}", f.text);
    }

    #[test]
    fn every_strategy_stays_within_budget() {
        let src = "claude-3-7-sonnet-20250219";
        for budget in 0..=30 {
            for overflow in [Overflow::End, Overflow::Middle, Overflow::Start] {
                let f = fit(src, budget, overflow);
                assert!(
                    w(&f.text) <= budget,
                    "{overflow:?} at {budget} produced {:?} ({} cols)",
                    f.text,
                    w(&f.text)
                );
            }
            let f = fit(src, budget, Overflow::Window { offset: 5 });
            assert!(w(&f.text) <= budget, "window at {budget}");
        }
    }

    #[test]
    fn wide_glyphs_are_never_split_or_over_budget() {
        let src = "深度求索-模型-20250219";
        for budget in 0..=20 {
            let f = fit(src, budget, Overflow::Middle);
            assert!(w(&f.text) <= budget, "budget {budget}: {:?}", f.text);
        }
    }

    #[test]
    fn a_newline_is_flattened_so_a_row_can_never_break() {
        let f = fit("first\nsecond", 40, Overflow::End);
        assert!(!f.text.contains('\n'), "{:?}", f.text);
        assert_eq!(f.text, "first second");
    }

    #[test]
    fn fit_line_stops_at_the_break_and_discloses_that_it_did() {
        // Two guarantees in one: the run-on is never spliced (that would
        // misreport the source), and the cut is never silent (that would let a
        // fold summary look like a complete quotation).
        let f = fit_line("first\nsecond", 40, Overflow::End);
        assert_eq!(f.text, "first…");
        assert!(f.truncated(), "a cut at the break is still a truncation");
        // With no room to spare, the ellipsis yields to the text.
        let tight = fit_line("first\nsecond", 5, Overflow::End);
        assert_eq!(tight.text, "first");
    }

    #[test]
    fn rebase_maps_highlight_positions_onto_the_painted_text() {
        let src = "claude-3-7-sonnet-20250219";
        // End: only head positions survive.
        let end = fit(src, 10, Overflow::End);
        assert_eq!(end.rebase(0), Some(0));
        assert_eq!(end.rebase(30), None);

        // Window: positions shift by the skipped prefix *and* by the leading
        // marker that discloses it.
        let window = fit(src, 8, Overflow::Window { offset: 4 });
        assert_eq!(window.skipped_chars, 4);
        assert!(window.lead_marker);
        let painted = window.rebase(4).expect("first visible char is painted");
        assert_eq!(
            window.text.chars().nth(painted),
            src.chars().nth(4),
            "rebase must account for the leading marker"
        );
        assert_eq!(window.rebase(3), None);

        // Middle: the tail is shifted past the ellipsis.
        let mid = fit(src, 12, Overflow::Middle);
        let last = src.chars().count() - 1;
        let painted = mid.rebase(last).expect("tail char is painted");
        assert_eq!(
            mid.text.chars().nth(painted),
            src.chars().nth(last),
            "rebase must point at the same glyph"
        );
    }

    #[test]
    fn marquee_rests_then_slides_then_returns() {
        let cadence = MarqueeCadence::default();
        let max = 20usize;
        assert_eq!(marquee_offset(max, 0, cadence), 0);
        assert_eq!(marquee_offset(max, cadence.hold_ms - 1, cadence), 0);
        assert_eq!(marquee_offset(max, cadence.hold_ms, cadence), 1);
        assert_eq!(
            marquee_offset(max, cadence.hold_ms + cadence.step_ms * 4, cadence),
            5
        );
        // A cell that fits never moves.
        assert_eq!(marquee_offset(0, 99_999, cadence), 0);
    }

    #[test]
    fn marquee_never_exceeds_the_maximum_offset() {
        let cadence = MarqueeCadence::default();
        let max = 20usize;
        for t in (0..20_000).step_by(37) {
            assert!(marquee_offset(max, t, cadence) <= max, "t={t}");
        }
    }

    #[test]
    fn a_window_reserves_a_column_so_the_tail_is_still_disclosed() {
        // The last offset must show the true end *and* still admit that the head
        // is hidden; otherwise a string's final columns would be the one position
        // where the reader is told nothing was skipped.
        let text = "claude-3-7-sonnet-20250219";
        let budget = 12;
        let max = window_max_offset(text.width(), budget, 1);
        let f = fit(text, budget, Overflow::Window { offset: max });
        assert!(f.text.starts_with('…'), "{:?}", f.text);
        assert!(f.text.ends_with("20250219"), "{:?}", f.text);
        assert!(f.text.width() <= budget, "{:?} over budget", f.text);
        assert!(f.lead_marker);
    }

    #[test]
    fn a_window_at_the_head_paints_no_leading_marker_but_discloses_its_tail() {
        let f = fit(
            "claude-3-7-sonnet-20250219",
            12,
            Overflow::Window { offset: 0 },
        );
        assert!(!f.lead_marker);
        assert!(f.text.starts_with("claude"), "{:?}", f.text);
        assert!(
            f.text.ends_with('…'),
            "the hidden tail is disclosed: {:?}",
            f.text
        );
    }

    #[test]
    fn a_window_never_hides_text_without_saying_so() {
        let text = "claude-3-7-sonnet-20250219";
        for budget in 3..=20 {
            let max = window_max_offset(text.width(), budget, 1);
            for offset in (0..=max).step_by(3) {
                let f = fit(text, budget, Overflow::Window { offset });
                assert_eq!(
                    f.skipped_chars > 0,
                    f.text.starts_with('…'),
                    "budget {budget} offset {offset}: hidden head must be disclosed: {:?}",
                    f.text
                );
                if f.hidden > 0 && f.text.width() < budget {
                    assert!(
                        f.text.ends_with('…'),
                        "budget {budget} offset {offset}: hidden tail must be disclosed: {:?}",
                        f.text
                    );
                }
            }
        }
    }

    #[test]
    fn a_text_cell_renders_into_exactly_the_budget_it_is_given() {
        let style = Style::default();
        let cell = TextCell::new("claude-3-7-sonnet", Overflow::Middle, style);
        for budget in 0..=25 {
            let spans = cell.render_width(budget);
            let painted: String = spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(w(&painted) <= budget, "budget {budget}: {painted:?}");
        }
    }

    #[test]
    fn a_text_cell_keeps_highlights_on_the_glyphs_it_paints() {
        let mut matched = HashSet::new();
        matched.insert(0usize); // 'c'
        matched.insert(3usize); // 'u'
        let cell = TextCell::new("claude", Overflow::End, Style::default())
            .highlighted(Some(&matched), Style::default());
        let spans = cell.render_width(20);
        let painted: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(painted, "claude");
        let highlighted: String = spans
            .iter()
            .filter(|s| s.style == Style::default() && s.content.len() == 1)
            .map(|s| s.content.as_ref())
            .collect();
        assert!(highlighted.contains('c'), "{spans:?}");
    }
}
