//! Full-width selectable list rows with Gestalt spacing.
//!
//! This is the **standard** for a single-line, columnar selectable row in a
//! centered modal list (Connections, Models, MCP, Tools, Sessions, …). Three
//! rules govern every row built here, and all are enforced by [`ListRow`]:
//!
//! ## 1. Fill the full width
//!
//! A row always paints edge-to-edge across `body_width`. The selected row's
//! brand fill is therefore an unbroken band, and unselected rows share one
//! panel background. This is achieved with explicit background-filled pad
//! spans — never by leaving cells at the default terminal background (which
//! would show a seam under a brand fill). [`ListRow::finish`] appends the
//! trailing pad for you.
//!
//! ## 2. Start one column in (the gutter)
//!
//! Text never hugs the left edge: every row begins with a [`GUTTER`]-column
//! (1) background indent before its first content. This gives the leading
//! glyph or word room to breathe against the modal border, the same way a
//! paragraph isn't flush with the page margin. The gutter is part of the row
//! fill, so the brand band still runs edge-to-edge — it is *space*, not a
//! missing cell.
//!
//! ## 3. Separate columns by whitespace, anchored to structure
//!
//! Content is grouped into [`RowGroup`]s. Groups are separated by
//! background-filled space, **never** by a `·`, `—`, `|`, or `/` glyph — a
//! glyph spends a column to say what the whitespace already says. How a group
//! is placed:
//!
//! - **[`RowGroup::fixed`]** — left-aligned, in source order, separated from
//!   the previous group by [`GROUP_GAP`]. Use for the leading status glyphs
//!   and the primary identity (e.g. the model name).
//! - **[`RowGroup::midpoint`]** — anchored at the *horizontal center* of the
//!   row (`body_width / 2`). Use for the SECOND column of a two-column row
//!   (e.g. the provider type / provider label) so the two columns are spread
//!   across the available width and read as distinct columns, not a run-on
//!   phrase. This is the law of proximity applied *horizontally*: a column
//!   that starts at the midpoint maximizes the gap between columns without a
//!   separator glyph.
//! - **[`RowGroup::trailing`]** — right-aligned to the trailing edge. Use for
//!   a count or state badge that should hug the right margin.
//!
//! The eye groups fields *within* a group more tightly than the gaps *between*
//! groups, so a row reads as a few clearly separated columns.
//!
//! ## When to use this vs. other patterns
//!
//! - **Use [`ListRow`]** for any single-line, columnar, fill-the-width
//!   selectable row. It is the one builder that guarantees the three rules.
//! - **Use [`super::options::ChoiceOptionRow`]** for a *wrapped* option row
//!   (a label + multi-line description, e.g. the question modal). That path
//!   centralizes color but keeps its own wrap layout.
//! - Inline status prose, footer hints, and the turn-header meta strip keep
//!   their own `·`-joined style — that is a *different* design language
//!   (compact, non-selectable) and is deliberately out of scope here.
//!
//! ## Example (a two-column Models row)
//!
//! ```ignore
//! use crate::components::row::{ListRow, RowGroup};
//! use crate::components::options::{ChoiceTone, choice_style};
//!
//! let style = choice_style(ChoiceTone::Filled, is_selected, theme);
//! let row = ListRow::new(style, body_width)
//!     // Fixed: the primary identity (model name), starting after the gutter.
//!     .group(RowGroup::fixed().text("gpt-4o", style.fg, 0))
//!     // Midpoint: the SECOND column (provider) starts at the horizontal
//!     // center, cleanly separating the two columns across the row width.
//!     .group(RowGroup::midpoint().text("OpenAI", style.dim, 0))
//!     // Trailing: an optional right-pinned badge.
//!     .group(RowGroup::trailing().text("think on", info, 0));
//! lines.push(row.finish());
//! ```

use nuotc::{Color, Line, Span, Style};
use unicode_width::UnicodeWidthStr;

use super::super::primitives::contrast_fg;
use super::options::ChoiceStyle;

/// A pre-styled atom — a `text`/`style` pair the builder renders verbatim,
/// used when a single group needs per-character styling (e.g. fuzzy-match
/// highlighting). Otherwise prefer the simpler `.text`/`.glyph` builders,
/// which derive the style from a plain foreground color.
#[derive(Clone)]
pub(crate) struct RowStyledAtom {
    pub(crate) text: String,
    pub(crate) style: Style,
}

/// The leading indent before a row's first content, in columns. Text never
/// starts flush against the modal border — see rule 2 in the module docs.
///
/// Kept a project-wide constant so every modal's rows share one left margin.
pub(crate) const GUTTER: usize = 1;

/// The default gap between two *fixed* (left-aligned) groups, in columns.
/// Larger than any *intra*-group gap (caller-chosen, typically 1) so the law
/// of proximity groups atoms within a group more tightly than it groups the
/// groups themselves.
///
/// Keep this a single, project-wide constant: the Gestalt effect only works
/// when the inter-group gap is visibly larger than the intra-group gap across
/// *every* modal, so the rows speak one language. Do not tune it per modal.
pub(crate) const GROUP_GAP: usize = 2;

/// Where a column-anchored group aligns within the row width.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    /// Left-aligned, in source order (the default).
    #[default]
    Fixed,
    /// Anchored so the group's first column sits at a proportional fraction
    /// of `body_width` (`body_width * numerator / denominator`).
    Ratio {
        numerator: usize,
        denominator: usize,
    },
    /// Anchored at an explicit column index relative to the row's inner start.
    Column(usize),
    /// Right-aligned to the trailing edge.
    Trailing,
}

/// One cluster of a row: one or more atoms rendered with small intra-group
/// gaps, then placed according to its [`Anchor`].
///
/// Atoms come in two flavors:
/// - plain foreground-color text via [`Self::text`] (the common case — the
///   whole atom shares one color);
/// - pre-styled atoms via [`Self::styled`] for per-character styling such as
///   fuzzy-match highlighting.
#[derive(Default)]
pub(crate) struct RowGroup {
    atoms: Vec<(GroupAtom, usize)>,
    anchor: Anchor,
}

/// One renderable atom within a group. Carries either a plain `(text, fg,
/// bold)` — rendered with the row's background — or a fully pre-styled span
/// (for per-character highlighting, where each char may differ).
#[derive(Clone)]
enum GroupAtom {
    Plain { text: String, fg: Color, bold: bool },
    Styled { text: String, style: Style },
}

impl GroupAtom {
    fn width(&self) -> usize {
        match self {
            GroupAtom::Plain { text, .. } | GroupAtom::Styled { text, .. } => text.width(),
        }
    }
}

impl RowGroup {
    /// A left-aligned group. Its atoms render in source order, each preceded
    /// by `gap` columns of background (pass `0` for the group's first atom so
    /// it starts flush; pass the intra-group spacing on later atoms).
    pub(crate) fn fixed() -> Self {
        Self {
            atoms: Vec::new(),
            anchor: Anchor::Fixed,
        }
    }

    /// A group anchored at a proportional ratio across the row width
    /// (`body_width * numerator / denominator`).
    /// Use to allocate custom column weights (e.g. 60% or 65% for primary
    /// identity columns) while preserving crisp tabular alignment across rows.
    pub(crate) fn ratio(numerator: usize, denominator: usize) -> Self {
        Self {
            atoms: Vec::new(),
            anchor: Anchor::Ratio {
                numerator,
                denominator: denominator.max(1),
            },
        }
    }

    /// A group anchored at the row's horizontal midpoint (`body_width / 2`).
    /// A convenience shorthand for [`Self::ratio(1, 2)`].
    pub(crate) fn midpoint() -> Self {
        Self::ratio(1, 2)
    }

    /// A right-aligned group, pinned to the row's trailing edge. Use for a
    /// trailing count or state badge so it hugs the right margin.
    pub(crate) fn trailing() -> Self {
        Self {
            atoms: Vec::new(),
            anchor: Anchor::Trailing,
        }
    }

    /// A group anchored at an explicit 0-based column index across the row.
    pub(crate) fn column(col: usize) -> Self {
        Self {
            atoms: Vec::new(),
            anchor: Anchor::Column(col),
        }
    }

    /// Append text with optional matched character indices highlighted using `match_style`.
    pub(crate) fn matched_text(
        mut self,
        text: &str,
        normal_style: Style,
        match_style: Style,
        matched: &std::collections::HashSet<usize>,
        gap: usize,
    ) -> Self {
        for (idx, c) in text.chars().enumerate() {
            let s = if matched.contains(&idx) {
                match_style
            } else {
                normal_style
            };
            self = self.styled(
                RowStyledAtom {
                    text: c.to_string(),
                    style: s,
                },
                if idx == 0 { gap } else { 0 },
            );
        }
        self
    }

    /// Append a text atom, preceded by `gap` columns of background within the
    /// group. Pass `0` for the group's first atom (start flush at its anchor)
    /// and the intra-group spacing for later ones. The gap is background-filled
    /// so the grouping reads cleanly under a brand fill.
    pub(crate) fn text(mut self, content: impl Into<String>, fg: Color, gap: usize) -> Self {
        self.atoms.push((
            GroupAtom::Plain {
                text: content.into(),
                fg,
                bold: false,
            },
            gap,
        ));
        self
    }

    /// Append a status-glyph atom (e.g. `●`, `★`), bolded so it reads as an
    /// icon. `gap` is the intra-group spacing before it, same as [`Self::text`].
    #[cfg(test)]
    pub(crate) fn glyph(mut self, glyph: &str, fg: Color, gap: usize) -> Self {
        self.atoms.push((
            GroupAtom::Plain {
                text: glyph.to_string(),
                fg,
                bold: true,
            },
            gap,
        ));
        self
    }

    /// Append a pre-styled atom (for per-character highlighting). The atom's
    /// style is painted verbatim, so the caller is responsible for setting the
    /// background (typically `style.bg`) so the fill stays unbroken. `gap` is
    /// the intra-group spacing before it.
    pub(crate) fn styled(mut self, atom: RowStyledAtom, gap: usize) -> Self {
        self.atoms.push((
            GroupAtom::Styled {
                text: atom.text,
                style: atom.style,
            },
            gap,
        ));
        self
    }

    /// The visible width of this group's atoms plus their intra-group gaps.
    fn width(&self) -> usize {
        self.atoms.iter().map(|(a, gap)| gap + a.width()).sum()
    }
}

/// A full-width selectable list row, assembled from [`RowGroup`]s placed by
/// their anchors. Enforces all three row rules: edge-to-edge fill, the leading
/// gutter, and gap-based (not glyph-based) column separation.
///
/// Build declaratively: [`ListRow::new`] fixes the palette and width, then
/// `.group(...)` appends clusters in display order, and [`ListRow::finish`]
/// emits the single [`Line`] with the leading gutter and trailing fill applied.
///
/// There must be at most one [`RowGroup::midpoint`] group; a second one is a
/// programmer error (two columns cannot both start at the center) and the
/// midpoint is clamped to keep the row within `body_width`.
pub(crate) struct ListRow {
    style: ChoiceStyle,
    body_width: usize,
    gutter: usize,
    groups: Vec<RowGroup>,
}

impl ListRow {
    /// Begin a row. `style` is the row palette (from
    /// [`super::options::choice_style`]); `body_width` is the available columns
    /// the row must fill. The row starts empty with default gutter.
    pub(crate) fn new(style: ChoiceStyle, body_width: usize) -> Self {
        Self {
            style,
            body_width,
            gutter: GUTTER,
            groups: Vec::new(),
        }
    }

    /// Override the leading gutter indent (default is `GUTTER = 1`).
    #[allow(dead_code)]
    pub(crate) fn gutter(mut self, gutter: usize) -> Self {
        self.gutter = gutter;
        self
    }

    /// Append a group in display order. Fixed groups render left-to-right from
    /// the gutter, separated by [`GROUP_GAP`]; anchored ratio/column groups
    /// position at their respective columns; trailing groups pin to the right edge.
    pub(crate) fn group(mut self, group: RowGroup) -> Self {
        self.groups.push(group);
        self
    }

    /// The contrast foreground for the row's background — what a brand-filled
    /// selected row paints its text in (white/black by panel luminance).
    /// Exposed so a caller can recolor a glyph that must stay legible on the
    /// fill without re-deriving the contrast rule.
    pub(crate) fn fill_fg(&self) -> Color {
        if self.style.bg == Color::default() {
            self.style.fg
        } else {
            contrast_fg(self.style.bg)
        }
    }

    /// Finalize the row into a single [`Line`] that fills `body_width`
    /// edge-to-edge, starting with the gutter indent.
    pub(crate) fn finish(self) -> Line<'static> {
        let bg = self.style.bg;

        let mut trailing_w = 0usize;
        let mut trailing_count = 0usize;

        struct Positioned<'a> {
            group: &'a RowGroup,
            desired_col: usize,
        }
        let mut positioned: Vec<Positioned> = Vec::new();

        for g in &self.groups {
            match g.anchor {
                Anchor::Fixed => {}
                Anchor::Ratio {
                    numerator,
                    denominator,
                } => {
                    let col = (self.body_width * numerator) / denominator.max(1);
                    positioned.push(Positioned {
                        group: g,
                        desired_col: col,
                    });
                }
                Anchor::Column(col) => {
                    positioned.push(Positioned {
                        group: g,
                        desired_col: col,
                    });
                }
                Anchor::Trailing => {
                    if trailing_count > 0 {
                        trailing_w += GROUP_GAP;
                    }
                    trailing_w += g.width();
                    trailing_count += 1;
                }
            }
        }

        positioned.sort_by_key(|p| p.desired_col);

        let trailing_start = self.body_width.saturating_sub(trailing_w);

        let mut spans: Vec<Span> = Vec::new();

        if self.gutter > 0 {
            spans.push(Span::styled(
                " ".repeat(self.gutter),
                Style::default().bg(bg),
            ));
        }

        let mut col = self.gutter;
        let mut emitted_fixed = 0usize;
        for g in &self.groups {
            if !matches!(g.anchor, Anchor::Fixed) {
                continue;
            }
            if emitted_fixed > 0 {
                spans.push(pad(GROUP_GAP, bg));
                col += GROUP_GAP;
            }
            render_atoms(&mut spans, &g.atoms, bg);
            col += g.width();
            emitted_fixed += 1;
        }

        for pg in positioned {
            let min_start = col + (if col > self.gutter { 1 } else { 0 });
            let start = pg.desired_col.max(min_start);
            if start < trailing_start {
                if start > col {
                    spans.push(pad(start - col, bg));
                    col = start;
                }
                render_atoms(&mut spans, &pg.group.atoms, bg);
                col += pg.group.width();
            }
        }

        if trailing_count > 0 {
            if col < trailing_start {
                spans.push(pad(trailing_start - col, bg));
                col = trailing_start;
            }
            let mut emitted_trailing = 0usize;
            for g in &self.groups {
                if !matches!(g.anchor, Anchor::Trailing) {
                    continue;
                }
                if emitted_trailing > 0 {
                    spans.push(pad(GROUP_GAP, bg));
                    col += GROUP_GAP;
                }
                render_atoms(&mut spans, &g.atoms, bg);
                col += g.width();
                emitted_trailing += 1;
            }
        }

        if col < self.body_width {
            spans.push(pad(self.body_width - col, bg));
        }

        // An entirely empty row still fills the width with background.
        if spans.is_empty() {
            spans.push(Span::styled(
                " ".repeat(self.body_width.max(1)),
                Style::default().bg(bg),
            ));
        }

        Line::from(spans)
    }
}

/// A background-filled pad span of `n` columns.
fn pad(n: usize, bg: Color) -> Span<'static> {
    Span::styled(" ".repeat(n), Style::default().bg(bg))
}

/// Paint one group's atoms into `spans`. Plain atoms get the row background
/// (plus bold for glyphs); styled atoms are painted verbatim (their style
/// already carries the background so the fill stays unbroken). Each atom is
/// preceded by its intra-group gap of background-filled space.
fn render_atoms(spans: &mut Vec<Span>, atoms: &[(GroupAtom, usize)], bg: Color) {
    for (atom, gap) in atoms {
        if *gap > 0 {
            spans.push(pad(*gap, bg));
        }
        let style = match atom {
            GroupAtom::Plain { fg, bold, .. } => {
                let mut s = Style::default().bg(bg).fg(*fg);
                if *bold {
                    s = s.add_modifier(nuotc::Modifier::BOLD);
                }
                s
            }
            GroupAtom::Styled { style, .. } => *style,
        };
        let text = match atom {
            GroupAtom::Plain { text, .. } | GroupAtom::Styled { text, .. } => text.clone(),
        };
        spans.push(Span::styled(text, style));
    }
}

#[cfg(test)]
mod tests {
    //! These tests pin the **three rules** a selectable modal list row must
    //! obey (see the module docs): (1) fill the full width edge-to-edge,
    //! (2) start one column in (the gutter), and (3) separate columns by
    //! whitespace, never a glyph — with a midpoint-anchored second column for
    //! two-column rows. If any breaks, every modal built on [`ListRow`]
    //! regresses at once — so they live with the builder, not in each modal's
    //! own tests.

    use super::*;
    use crate::components::options::{ChoiceTone, choice_style};
    use crate::render::Theme;
    use nuotc::Span;
    use unicode_width::UnicodeWidthStr;

    /// Total visible width of a rendered line — the sum of every span's
    /// content width. Used to assert the row fills `body_width`.
    fn line_width(line: &nuotc::Line) -> usize {
        line.spans
            .iter()
            .map(|s: &Span| s.content.as_ref().width())
            .sum()
    }

    /// The concatenated text of a rendered line — used to assert no glyph
    /// separator (`·`, `—`, `|`) leaked into a column boundary.
    fn line_text(line: &nuotc::Line) -> String {
        line.spans
            .iter()
            .map(|s: &Span| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn row_fills_the_full_width_with_a_single_group() {
        let theme = Theme::default();
        let style = choice_style(ChoiceTone::Filled, false, &theme);
        let body_width = 60;
        let row = ListRow::new(style, body_width).group(
            RowGroup::fixed()
                .text("OpenAI", style.fg, 0)
                .text("OpenAI", style.dim, 1),
        );
        let line = row.finish();
        assert_eq!(line_width(&line), body_width, "row must fill body_width");
        assert!(
            !line_text(&line).contains('·'),
            "no glyph separator between groups"
        );
    }

    #[test]
    fn row_starts_with_the_gutter_indent() {
        // Rule 2: the very first span is a 1-column background pad, and the
        // first content column is at index GUTTER (1), not 0.
        let theme = Theme::default();
        let style = choice_style(ChoiceTone::Filled, false, &theme);
        let line = ListRow::new(style, 30)
            .group(RowGroup::fixed().text("name", style.fg, 0))
            .finish();
        // First span is the gutter pad.
        assert_eq!(line.spans[0].content.as_ref(), " ");
        assert_eq!(line.spans[0].content.as_ref().width(), GUTTER);
        // Second span is the content.
        assert_eq!(line.spans[1].content.as_ref(), "name");
    }

    #[test]
    fn midpoint_column_starts_at_the_horizontal_center() {
        // Rule 3: the second column of a two-column row anchors at body_width/2.
        let theme = Theme::default();
        let style = choice_style(ChoiceTone::Filled, false, &theme);
        let body_width = 40;
        let line = ListRow::new(style, body_width)
            .group(RowGroup::fixed().text("gpt-4o", style.fg, 0))
            .group(RowGroup::midpoint().text("OpenAI", style.dim, 0))
            .finish();
        assert_eq!(line_width(&line), body_width, "row fills body_width");

        // Reconstruct the column each non-pad atom starts at. The provider
        // label ("OpenAI") must start at column body_width / 2.
        let mut col = 0usize;
        let mut provider_col: Option<usize> = None;
        for span in &line.spans {
            let content: &str = span.content.as_ref();
            if content == "OpenAI" {
                provider_col = Some(col);
            }
            col += content.width();
        }
        assert_eq!(
            provider_col,
            Some(body_width / 2),
            "second column starts at the midpoint"
        );
    }

    #[test]
    fn ratio_column_starts_at_proportional_position() {
        let theme = Theme::default();
        let style = choice_style(ChoiceTone::Filled, false, &theme);
        let body_width = 50;
        let line = ListRow::new(style, body_width)
            .group(RowGroup::fixed().text("gpt-4o", style.fg, 0))
            .group(RowGroup::ratio(3, 5).text("OpenAI", style.dim, 0))
            .finish();
        assert_eq!(line_width(&line), body_width, "row fills body_width");

        let mut col = 0usize;
        let mut provider_col: Option<usize> = None;
        for span in &line.spans {
            let content: &str = span.content.as_ref();
            if content == "OpenAI" {
                provider_col = Some(col);
            }
            col += content.width();
        }
        assert_eq!(
            provider_col,
            Some((body_width * 3) / 5),
            "ratio column starts at 3/5 width (30)"
        );
    }

    #[test]
    fn row_fills_width_with_fixed_and_trailing_groups() {
        let theme = Theme::default();
        let style = choice_style(ChoiceTone::Filled, true, &theme);
        let body_width = 48;
        let row = ListRow::new(style, body_width)
            .group(
                RowGroup::fixed()
                    .text("gpt-4o", style.fg, 0)
                    .text("OpenAI", style.dim, 1),
            )
            .group(RowGroup::trailing().text("2 models", style.dim, 0));
        let line = row.finish();
        assert_eq!(line_width(&line), body_width, "row must fill body_width");
        let text = line_text(&line);
        assert!(text.contains("gpt-4o"), "identity group present");
        assert!(text.contains("2 models"), "trailing group present");
        assert!(!text.contains('·'), "no glyph separator");
    }

    #[test]
    fn row_separates_groups_by_a_wider_gap_than_within_a_group() {
        // Gestalt law of proximity: the inter-group gap (GROUP_GAP = 2) must be
        // visibly larger than the intra-group gap (1) so the eye clusters the
        // atoms within a group.
        let theme = Theme::default();
        let style = choice_style(ChoiceTone::Filled, false, &theme);
        let row = ListRow::new(style, 40)
            .group(
                RowGroup::fixed()
                    .text("a", style.fg, 0)
                    .text("b", style.dim, 1),
            )
            .group(RowGroup::fixed().text("c", style.fg, 0));
        let line = row.finish();

        let mut max_space_run = 0usize;
        let mut cur = 0usize;
        for span in &line.spans {
            let content: &str = span.content.as_ref();
            if content.chars().all(|c| c == ' ') && !content.is_empty() {
                cur += content.width();
                max_space_run = max_space_run.max(cur);
            } else {
                cur = 0;
            }
        }
        assert!(
            max_space_run >= GROUP_GAP,
            "inter-group gap ({max_space_run}) must be >= GROUP_GAP ({GROUP_GAP})"
        );
    }

    #[test]
    fn empty_row_still_fills_the_width() {
        let theme = Theme::default();
        let style = choice_style(ChoiceTone::Filled, false, &theme);
        let body_width = 20;
        let line = ListRow::new(style, body_width).finish();
        assert_eq!(line_width(&line), body_width, "even an empty row fills");
    }

    #[test]
    fn status_glyphs_are_bold_icons_not_text() {
        // A glyph atom renders bold; a text atom does not. This keeps the `●`
        // / `★` reading as icons rather than decorations.
        let theme = Theme::default();
        let style = choice_style(ChoiceTone::Filled, false, &theme);
        let line = ListRow::new(style, 10)
            .group(
                RowGroup::fixed()
                    .glyph("●", theme.ok(), 0)
                    .text("name", style.fg, 1),
            )
            .finish();
        // spans[0] is the gutter; the glyph is spans[1].
        let dot_span = &line.spans[1];
        assert_eq!(dot_span.content.as_ref(), "●");
        assert!(
            dot_span.style.add.contains(nuotc::Modifier::BOLD),
            "glyph atom is bold"
        );
    }

    #[test]
    fn multi_column_layout_anchors_columns_at_exact_positions() {
        let theme = Theme::default();
        let style = choice_style(ChoiceTone::Filled, false, &theme);
        let body_width = 80;
        let matched: std::collections::HashSet<usize> = [0, 1].into_iter().collect();
        let line = ListRow::new(style, body_width)
            .group(RowGroup::fixed().text("gpt-4o", style.fg, 0))
            .group(RowGroup::column(20).matched_text(
                "GPT 4o",
                Style::default().fg(style.dim),
                Style::default().fg(theme.brand()),
                &matched,
                0,
            ))
            .group(RowGroup::ratio(3, 5).text("OpenAI", style.dim, 0))
            .finish();
        let text = line_text(&line);
        assert_eq!(line_width(&line), body_width);
        assert_eq!(&text[1..7], "gpt-4o");
        assert_eq!(&text[20..26], "GPT 4o");
        assert_eq!(&text[48..54], "OpenAI");
    }
}
