//! Tiny shared render helpers: viewport math, modal centering/recess, panel
//! chrome, and color arithmetic. Kept in one place so the per-component
//! modules do not need to depend on each other for these primitives.

/// Background recess policy when rendering an overlay.
#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug, Default)]
pub enum Recess {
    #[default]
    None,
    Dim,
    Takeover,
}
use nuotc::{
    Alignment, Constraint, Direction, Frame, Layout, Line, Rect,
    {Block as RtBlock, Clear, Paragraph}, {Color, Style},
};

use super::Theme;
pub(crate) use super::components::footer::{
    FooterHint, FooterHintWithBand, modal_footer_text, render_modal_footer,
    render_modal_footer_with_extra,
};
#[allow(unused_imports)]
pub use super::components::inline_layout::{InlineSlot, SemanticLine};
#[allow(unused_imports)]
pub use super::components::path::{
    PathFormatStrategy, PathStyle, PathView, format_path_str, tilde_shorten,
};
use super::design::{MODAL_INNER_V_PADDING, SCROLLBAR_GAP};
/// Canonical key-display vocabulary: named `&'static str` constants for the
/// glyphs footers and legends repeat (`keyvocab::ESC`, `keyvocab::ARROWS_UD`,
/// …). Re-exported here because every overlay already imports this module for
/// `FooterHint`, so a footer's key + label both come from one place.
pub(crate) use super::keymap::keyvocab;
pub(crate) use crate::elevation::*;

/// 2-tier responsive layout breakpoint for TUI views (ADR-0097 evolution, ADR-0181 capability pipeline).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutTier {
    /// Wide display (>= 90 columns): dual-pane side-by-side (Master-Detail).
    Wide,
    /// Compact / narrow display (< 90 columns, e.g. tiled tmux / 80-col): vertical stack.
    Compact,
}

impl LayoutTier {
    /// Threshold width in columns where dual-pane layout makes sense.
    pub const WIDE_THRESHOLD: u16 = 90;

    /// Resolve the layout tier from effective usable width after archetype spatial deduction (ADR-0181).
    pub fn from_effective_width(effective_width: u16) -> Self {
        if effective_width >= Self::WIDE_THRESHOLD {
            Self::Wide
        } else {
            Self::Compact
        }
    }

    /// Resolve the layout tier given raw rect and elevation archetype (ADR-0181).
    /// Deducts archetype spatial cost (borders, structural margins) before evaluating breakpoint.
    pub fn from_rect(rect: Rect, archetype: nuotc::ElevationArchetype) -> Self {
        let inner = archetype.inner_bounds(rect);
        Self::from_effective_width(inner.width)
    }

    /// Resolution from physical column width without capability awareness.
    /// Prefer [`Self::from_rect`] in new code to account for capability spatial insets.
    #[allow(dead_code)]
    pub fn from_width(width: u16) -> Self {
        Self::from_effective_width(width)
    }

    /// True if the layout is Wide.
    pub fn is_wide(self) -> bool {
        matches!(self, Self::Wide)
    }
}

/// Global viewport margins. One row of breathing room is reserved at the
/// top; horizontally every component spans the full terminal width. The
/// bottom margin is 0: the hint bar pins flush against the terminal's bottom
/// edge — an empty `app_bg` row below it only wasted a transcript row.
pub(crate) const VIEWPORT_H_MARGIN: u16 = 0;
pub(crate) const VIEWPORT_TOP_MARGIN: u16 = 1;
pub(crate) const VIEWPORT_BOTTOM_MARGIN: u16 = 0;

/// The usable area after reserving the global viewport margins (1 cell top,
/// 0 bottom). The full `frame.area()` is only used to paint the app
/// background and the modal backdrop.
pub(crate) fn viewport_rect(frame: &Frame) -> Rect {
    let area = frame.area();
    Rect::new(
        area.x + VIEWPORT_H_MARGIN,
        area.y + VIEWPORT_TOP_MARGIN,
        area.width.saturating_sub(2 * VIEWPORT_H_MARGIN),
        area.height
            .saturating_sub(VIEWPORT_TOP_MARGIN + VIEWPORT_BOTTOM_MARGIN),
    )
}

pub(crate) fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    let area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1];
    // Snap the width to an even column count. A modal insets its body by
    // `MODAL_INNER_H_PADDING` on each side (an even total), so an even outer
    // width yields an even body width — which CJK / full-width glyphs (each 2
    // columns wide) can tile without leaving a stranded trailing column that
    // forces every wrap line short by one glyph. The odd column is shed from
    // the right margin; the rect stays centered because `Layout` already
    // divided the margins evenly.
    even_width(area)
}

/// Like [`centered_rect`] but the vertical extent is an explicit row count
/// instead of a percentage, so a modal can size to its content rather than
/// reserve a fixed slab of the viewport. `height` is clamped to `r`'s height
/// and the band is centered vertically; the width is still a percentage so the
/// modal keeps a consistent horizontal footprint regardless of how tall it is.
pub(crate) fn centered_rect_h(percent_x: u16, height: u16, r: Rect) -> Rect {
    let height = height.min(r.height);
    let top = r.y + r.height.saturating_sub(height) / 2;
    let band = Rect {
        x: r.x,
        y: top,
        width: r.width,
        height,
    };
    let area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(band)[1];
    // See `centered_rect`: an even width gives the body an even usable width so
    // full-width (CJK) glyphs tile flush on every wrap line.
    even_width(area)
}

/// Floor a rect's width to the nearest even column count, clamped to ≥ 0.
///
/// This is the single lever that makes every modal body an even-width surface.
/// Because [`modal_frame`] insets the body symmetrically by `MODAL_INNER_H_PADDING`
/// (an even total), an even panel width propagates to an even body width, so a
/// run of 2-column CJK glyphs fills every row end-to-end instead of stranding
/// one empty column that costs a full glyph on each wrapped line.
fn even_width(mut rect: Rect) -> Rect {
    rect.width &= !1;
    rect
}

#[derive(Clone, Copy)]
pub(crate) struct ModalSpec {
    pub width_percent: u16,
    pub header: bool,
    pub footer: bool,
}

/// Geometry for a modal whose height is a fixed percentage of the viewport.
///
/// Keeping this distinct from [`ContentModalSpec`] makes invalid combinations
/// unrepresentable: a fixed renderer cannot accidentally request content
/// sizing, and neither API needs to return `Option` for a structural invariant.
#[derive(Clone, Copy)]
pub(crate) struct FixedModalSpec {
    spec: ModalSpec,
    height_percent: u16,
}

impl FixedModalSpec {
    const fn new(width_percent: u16, height_percent: u16) -> Self {
        Self {
            spec: ModalSpec {
                width_percent,
                header: true,
                footer: true,
            },
            height_percent,
        }
    }

    // The preset chooser shares the provider list's footprint.
    pub const PROVIDER: Self = Self::new(76, 80);
    pub const SESSIONS: Self = Self::new(82, 78);
}

/// Geometry for a modal whose height follows its rendered content up to max bounds.
#[derive(Clone, Copy)]
pub(crate) struct ContentModalSpec {
    spec: ModalSpec,
    min_rows: u16,
    max_viewport_percent: u16,
    max_height_rows: Option<u16>,
    max_width_cols: Option<u16>,
}

impl ContentModalSpec {
    const fn new(width_percent: u16, min_rows: u16, max_viewport_percent: u16) -> Self {
        Self {
            spec: ModalSpec {
                width_percent,
                header: true,
                footer: true,
            },
            min_rows,
            max_viewport_percent,
            max_height_rows: None,
            max_width_cols: None,
        }
    }

    pub const TOOLS: Self = Self::new(64, 11, 84);
    pub const MCP: Self = Self::new(64, 9, 84);
    pub const QUEUE: Self = Self::new(66, 9, 84);
    /// The `/btw` asides list (ADR-0103 §5). One row per live aside; sized
    /// like the queue overview it mirrors (list + footer legend).
    pub const BTW: Self = Self::new(66, 9, 84);
    /// Unified session telemetry inspector (Context Usage & Performance).
    pub const TELEMETRY: Self = Self::new(76, 11, 84);
    /// The usage-statistics overlay (`/usage`): three stacked sections
    /// (summary, daily chart + table, model breakdown, event log) in one
    /// scrolling body. Wider than the context-usage modal so the four-column
    /// tables breathe; the viewport ceiling keeps long histories scrolled
    /// rather than full-height.
    pub const USAGE_STATS: Self = Self::new(76, 12, 86);
    /// The unified provider/model editor (`draw_model_editor`). Sizes to its
    /// content — at most three rows (API key, reasoning effort, extended
    /// thinking) — instead of reserving a fixed 30% slab that left most of
    /// the panel empty. Width 66% gives long API keys more room than the old
    /// 60% while staying comfortably inside the viewport. `max_viewport_percent`
    /// is a generous 60% purely as a ceiling; the real height is the content
    /// row count plus chrome, which never approaches it.
    pub const MODEL_EDITOR: Self = Self::new(66, 6, 60);
    pub const OAUTH_PENDING: Self = Self::new(76, 7, 80);
    pub const CUSTOM_PROVIDER: Self = Self::new(72, 8, 80);
    pub const PERMISSIONS: Self = Self::new(72, 7, 80);
    pub const SKILLS: Self = Self::new(72, 7, 80);

    pub const fn modal_spec(self) -> ModalSpec {
        self.spec
    }
}

pub(crate) fn modal_chrome_rows(spec: ModalSpec) -> u16 {
    let mut rows = 2 * MODAL_INNER_V_PADDING;
    if spec.header {
        rows += 2; // header + gap after header
    }
    if spec.footer {
        rows += 2; // gap before footer + footer
    }
    rows
}

pub(crate) fn modal_area(frame: &Frame, geometry: FixedModalSpec) -> Rect {
    centered_rect(
        geometry.spec.width_percent,
        geometry.height_percent,
        frame.area(),
    )
}

pub(crate) fn content_modal_probe(frame: &Frame, geometry: ContentModalSpec) -> Rect {
    let area = frame.area();
    let mut rect = centered_rect(geometry.spec.width_percent, 100, area);
    if let Some(max_cols) = geometry.max_width_cols
        && rect.width > max_cols
    {
        let left = rect.x + (rect.width - max_cols) / 2;
        rect = Rect::new(left, rect.y, max_cols, rect.height);
    }
    even_width(rect)
}

pub(crate) fn content_modal_area(
    frame: &Frame,
    geometry: ContentModalSpec,
    desired_rows: u16,
) -> Rect {
    let area = frame.area();
    let mut max_h = ((area.height as u32 * geometry.max_viewport_percent as u32) / 100) as u16;
    if let Some(limit) = geometry.max_height_rows {
        max_h = max_h.min(limit);
    }
    let height = desired_rows.clamp(geometry.min_rows, max_h.max(geometry.min_rows));
    let mut rect = centered_rect_h(geometry.spec.width_percent, height, area);
    if let Some(max_cols) = geometry.max_width_cols
        && rect.width > max_cols
    {
        let left = rect.x + (rect.width - max_cols) / 2;
        rect = Rect::new(left, rect.y, max_cols, rect.height);
    }
    even_width(rect)
}

/// Recess the live surface behind a modal, per its [`Recess`] policy.
///
/// A terminal cannot alpha-blend, so the event loop calls this exactly once
/// per frame *after* the transcript and chrome are drawn and *before* the
/// centered modal panel — which then overpaints its own crisp area on top.
/// The three policies:
///
/// - [`Recess::None`] leaves the surface untouched (lightweight floats such as
///   Question / Permission that never take over).
/// - [`Recess::Dim`] darkens every cell in place by [`Theme::modal_dim_factor`]
///   so the background stays visible for context while the modal reads as the
///   focal layer. This replaces the old opaque full-screen fill: context no
///   longer vanishes behind a modal.
/// - [`Recess::Takeover`] clears + fills with [`Theme::backdrop`], fully
///   occluding the surface for a context switch (session selection).
///
/// [`Theme::modal_dim_factor`]: Theme::modal_dim_factor
pub fn recess_backdrop(frame: &mut Frame, recess: Recess, theme: &Theme) {
    match recess {
        Recess::None => {}
        Recess::Dim => dim_surface(frame, theme),
        Recess::Takeover => {
            let area = frame.area();
            frame.render_widget(Clear, area);
            frame.render_widget(
                RtBlock::default().style(Style::default().bg(theme.backdrop())),
                area,
            );
        }
    }
}

/// Darken the whole frame buffer in place by scaling each cell's RGB channels
/// toward black by `factor` (0.0 = invisible, 1.0 = unchanged). This is the
/// "dim-recess" effect: the surface is rendered normally first, then every
/// cell is multiplied by `factor`, so context stays visible while clearly
/// receding behind the modal drawn on top.
///
/// Only [`Color::Rgb`] is scaled (the entire palette is RGB, so this covers
/// every painted cell); named / Reset colors are left untouched so the dim is
/// additive rather than lossy where they appear.
fn dim_surface(frame: &mut Frame, theme: &Theme) {
    let factor = theme.modal_dim_factor();
    // Code already starts closer to the dark surface than prose. If foreground
    // and background are both multiplied by the modal factor, inline/code-block
    // text loses contrast first. Keep code text a little brighter while still
    // dimming its surface with the rest of the transcript.
    let code_fg_factor = (factor + 0.25).min(1.0);
    let buffer = frame.buffer_mut();
    let (w, h) = buffer.size();
    for y in 0..h {
        for x in 0..w {
            let cell = &mut buffer.content[(y as usize) * (w as usize) + x as usize];
            let fg_factor = if cell.fg == theme.code_text() {
                code_fg_factor
            } else {
                factor
            };
            cell.fg = scale_color(cell.fg, fg_factor);
            cell.bg = scale_color(cell.bg, factor);
            cell.style.fg = cell.fg;
            cell.style.bg = cell.bg;
        }
        // In-place mutation bypasses the write-marks-dirty contract
        // (`Grid::mark`); today a full-screen background fill already dirties
        // every row earlier in the frame, so the dim repaints correctly — but
        // that is a coupling, not a contract. Mark the row explicitly so the
        // diff stays honest even if the fill is ever scoped narrower.
        buffer.mark(0, y);
    }
}

/// Multiply an RGB color's channels by `factor`, clamped to `[0, 1]`.
fn scale_color(color: Color, factor: f32) -> Color {
    let f = factor.clamp(0.0, 1.0);
    match color {
        Color::Rgb(r, g, b) => Color::Rgb(
            (r as f32 * f).round() as u8,
            (g as f32 * f).round() as u8,
            (b as f32 * f).round() as u8,
        ),
        other => other,
    }
}

/// The single separator glyph for a hierarchical (breadcrumb) modal header.
/// Keeps every drill-in sub-page — `Sessions › Info`, `Settings › Layout`,
/// `Settings › Appearance` — visually identical. Centralized (and `'static`)
/// so the glyph and spacing never drift between modals.
pub(crate) const BREADCRUMB_SEP: &str = " › ";

/// The standard hierarchical (breadcrumb) header for a modal sub-page: a muted
/// parent label, the [`BREADCRUMB_SEP`] separator, then the bold child title.
/// This is the component-level convention for **modal hierarchy**:
///
/// - A sub-page keeps the *same* `Modal` variant as its parent (it is one modal
///   drilling into a secondary view, not a separate modal), so the breadcrumb is
///   how the user sees where they are. Example: a Sessions picker drilled into
///   its info view renders `Sessions › Info`.
/// - `Esc` navigates one level up (handled in the event loop's `CloseModal`
///   arm): the first `Esc` returns from a sub-page to its parent view, a second
///   `Esc` closes the modal. The header flips back to the parent title on
///   back-out.
///
/// Pass the returned slice to `modal_header_parts`. All three segments borrow
/// their input `&str`s (the separator is a `'static` const), so no allocation.
pub(crate) fn breadcrumb_parts<'a>(parent: &'a str, child: &'a str) -> [HeaderPart<'a>; 3] {
    [
        HeaderPart::Text {
            text: parent,
            accent: false,
        },
        HeaderPart::Text {
            text: BREADCRUMB_SEP,
            accent: false,
        },
        HeaderPart::title(child),
    ]
}

/// The multi-level hierarchical breadcrumb builder with automatic front-truncation (`... › `).
///
/// When the available header width is insufficient to fit all segments (e.g. `Connections › Add › Google Antigravity`),
/// it drops leftmost levels and replaces them with `... › `, e.g. `... › Add › Google Antigravity` or `... › Google Antigravity`.
pub(crate) fn hierarchical_breadcrumb<'a>(
    levels: &[&'a str],
    max_width: usize,
) -> Vec<HeaderPart<'a>> {
    if levels.is_empty() {
        return Vec::new();
    }
    if levels.len() == 1 {
        return vec![HeaderPart::title(levels[0])];
    }

    let sep_w = 3; // " › "
    let full_width: usize =
        levels.iter().map(|s| s.chars().count()).sum::<usize>() + (levels.len() - 1) * sep_w;

    if full_width <= max_width {
        let mut parts = Vec::with_capacity(levels.len() * 2 - 1);
        for (i, level) in levels.iter().enumerate() {
            if i > 0 {
                parts.push(HeaderPart::Text {
                    text: BREADCRUMB_SEP,
                    accent: false,
                });
            }
            if i == levels.len() - 1 {
                parts.push(HeaderPart::title(level));
            } else {
                parts.push(HeaderPart::Text {
                    text: level,
                    accent: false,
                });
            }
        }
        return parts;
    }

    // Progressively drop leftmost levels and prepend `...`
    for start_idx in 1..levels.len() {
        let remaining = &levels[start_idx..];
        let rem_width: usize = 3 // "..."
            + sep_w
            + remaining.iter().map(|s| s.chars().count()).sum::<usize>()
            + (remaining.len() - 1) * sep_w;

        if rem_width <= max_width || start_idx == levels.len() - 1 {
            let mut parts = Vec::with_capacity(remaining.len() * 2 + 1);
            parts.push(HeaderPart::Text {
                text: "...",
                accent: false,
            });
            for (i, level) in remaining.iter().enumerate() {
                parts.push(HeaderPart::Text {
                    text: BREADCRUMB_SEP,
                    accent: false,
                });
                if i == remaining.len() - 1 {
                    parts.push(HeaderPart::title(level));
                } else {
                    parts.push(HeaderPart::Text {
                        text: level,
                        accent: false,
                    });
                }
            }
            return parts;
        }
    }

    vec![HeaderPart::title(levels[levels.len() - 1])]
}

/// Render a modal body with shared scroll mechanics. The `scroll` offset is
/// clamped to `[0, content_lines - visible]` (so it can never drift past the
/// last line) and, when `follow` is `Some(idx)`, nudged so row `idx` stays on
/// screen — that's how list modals keep their selection visible without a
/// separate scroll cursor. The body is rendered with `.scroll()` so anything
/// past the visible window is clipped rather than silently truncated.
///
/// `edge_margin` keeps the followed row away from the top/bottom edges by an
/// `edge_margin`-row band (when the viewport is tall enough), so `↑/↓`
/// navigation never pins the highlight to the last visible line — there is
/// always a buffer of context on the side being moved toward. Pass
/// [`SCROLL_EDGE_MARGIN`] for a pure list (provider/model/preset/skills/…
/// pickers) where every followed row is a peer and context on both sides is
/// meaningful; pass `0` for decision sheets and content viewers whose
/// `follow` is an absolute body line in mixed header+row content (the
/// question / permission sheets) or that scroll manually (activity),
/// where edge-pinning reads better. A viewport too short for the band falls
/// back to edge-pinning in either case.
/// Resolve the effective scroll offset for a body of `total` lines in a
/// viewport `visible` rows tall, honoring an optional follow index and the
/// same edge-margin band logic [`render_body`] applies. Returns `(scroll,
/// max_scroll)`. This is the pure scroll-resolution half of [`render_body`],
/// factored out so a caller can compute the visible window *before* building
/// lines — letting list modals build only the rows that will actually be
/// painted instead of the whole list every frame.
pub(crate) fn resolve_scroll(
    scroll: &mut usize,
    visible: usize,
    total: usize,
    follow: Option<usize>,
    edge_margin: usize,
) -> (usize, usize) {
    let max_scroll = total.saturating_sub(visible);
    *scroll = (*scroll).min(max_scroll);
    if let Some(idx) = follow
        && visible > 0
    {
        // Margin band kept clear above and below the selection. Capped at
        // `(visible - 1) / 2` so it never exceeds half the viewport — for a
        // short viewport this collapses to 0 and the edge-pinning fallback
        // below kicks in. `edge_margin == 0` selects pure edge-pinning.
        let margin = edge_margin.min((visible - 1) / 2);
        if margin > 0 {
            let top_band = *scroll + margin;
            let bottom_band = *scroll + visible - margin;
            if idx < top_band {
                *scroll = idx.saturating_sub(margin);
            } else if idx >= bottom_band {
                *scroll = idx - (visible - 1 - margin);
            }
        } else if idx < *scroll {
            *scroll = idx;
        } else if idx >= *scroll + visible {
            *scroll = idx.saturating_sub(visible.saturating_sub(1));
        }
        // Re-clamp: a follow nudge can overshoot when content is shorter than
        // the viewport, or when `idx` is near the very end.
        *scroll = (*scroll).min(max_scroll);
    }
    (*scroll, max_scroll)
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BodyRenderOptions {
    pub follow: Option<usize>,
    pub edge_margin: usize,
    pub wrap: bool,
}

impl BodyRenderOptions {
    pub fn new(follow: Option<usize>, edge_margin: usize, wrap: bool) -> Self {
        Self {
            follow,
            edge_margin,
            wrap,
        }
    }

    pub fn follow(follow: Option<usize>) -> Self {
        Self {
            follow,
            edge_margin: 0,
            wrap: false,
        }
    }
}

pub(crate) fn render_body(
    frame: &mut Frame,
    body_rect: Rect,
    lines: Vec<Line<'static>>,
    scroll: &mut usize,
    options: BodyRenderOptions,
    theme: &Theme,
) {
    let BodyRenderOptions {
        follow,
        edge_margin,
        wrap,
    } = options;
    let visible = body_rect.height as usize;
    let (_, max_scroll) = resolve_scroll(scroll, visible, lines.len(), follow, edge_margin);

    let mut para = Paragraph::new(lines).scroll(*scroll as u16, 0);
    if wrap {
        para = para.wrap(nuotc::Wrap { trim: false });
    }
    frame.render_widget(para, body_rect);

    // Scroll indicator: a one-cell scrollbar in the right margin showing
    // whether more content lies above and/or below the window. Only drawn
    // when content overflows the body height.
    draw_scrollbar(frame, body_rect, *scroll, max_scroll, theme);
}

/// Render a block of lines vertically and horizontally centered in `body_rect`.
///
/// Used by empty-state modal bodies (e.g. Connections and Models pickers when no
/// items exist) to place guidance copy in the visual center of the modal frame
/// rather than pinned to the top-left corner.
pub(crate) fn render_centered_body(frame: &mut Frame, body_rect: Rect, lines: Vec<Line<'static>>) {
    if body_rect.height == 0 || body_rect.width == 0 || lines.is_empty() {
        return;
    }
    let slack = body_rect.height.saturating_sub(lines.len() as u16) / 2;
    let top = body_rect.y + slack;
    let para = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(
        para,
        Rect::new(
            body_rect.x,
            top,
            body_rect.width,
            body_rect.height.saturating_sub(slack),
        ),
    );
}

/// The number of context rows kept above and below a followed selection before
/// the viewport begins to scroll. Keeps `↑/↓` movement from pinning the
/// highlight to the last visible line. Pass this as `render_body`'s
/// `edge_margin` for pure-list modals; only applies when the viewport is tall
/// enough (at least `2 * SCROLL_EDGE_MARGIN + 1` body rows), otherwise the
/// follow falls back to edge-pinning.
pub(crate) const SCROLL_EDGE_MARGIN: usize = 3;

/// Draw a minimal one-column scrollbar in the body's rightmost column when
/// the content overflows. Shows a thumb whose vertical position reflects the
/// `scroll / max_scroll` ratio, plus `▲` / `▼` caps when more content lies
/// above / below. The thumb uses `theme.muted()`; the caps use `theme.dim()`
/// so the bar reads as a subtle affordance, not a focal element.
pub(crate) fn draw_scrollbar(
    frame: &mut Frame,
    body: Rect,
    scroll: usize,
    max_scroll: usize,
    theme: &Theme,
) {
    if max_scroll == 0 || body.width == 0 || body.height < 2 {
        return;
    }
    let h = body.height as usize;
    // Thumb height scales with the visible-to-total ratio, floored at 1.
    let thumb_h = (h * h / (max_scroll + h)).max(1).min(h) as u16;
    let track = h as u16;
    let track_top = body.y;
    let track_x = body.x + body.width + SCROLLBAR_GAP;

    let more_above = scroll > 0;
    let more_below = scroll < max_scroll;

    // Caps (only when there is content in that direction). Coordinates are
    // within `body`, which is inside the buffer, so direct content indexing
    // is safe.
    let buf = frame.buffer_mut();
    let buf_area = buf.area();
    if more_above {
        let cell = cell_at_index(buf, buf_area, track_x, track_top);
        cell.set_symbol("▲");
        cell.set_fg(theme.dim());
        buf.mark(track_x, track_top);
    }
    if more_below {
        let cell = cell_at_index(buf, buf_area, track_x, track_top + track - 1);
        cell.set_symbol("▼");
        cell.set_fg(theme.dim());
        buf.mark(track_x, track_top + track - 1);
    }

    // Thumb position within the open track (between the two caps).
    let open_top = if more_above { 1 } else { 0 };
    let open_bottom = track as i32 - if more_below { 1 } else { 0 };
    let open_h = (open_bottom - open_top).max(1) as u16;
    let ratio = if max_scroll > 0 {
        scroll as f32 / max_scroll as f32
    } else {
        0.0
    };
    let thumb_y =
        track_top + open_top as u16 + (ratio * (open_h.saturating_sub(thumb_h)) as f32) as u16;

    for i in 0..thumb_h {
        let y = thumb_y + i;
        if y < track_top + track {
            let cell = cell_at_index(buf, buf_area, track_x, y);
            cell.set_symbol(" ");
            cell.set_bg(theme.muted());
            // Same dirty-tracking honesty as the caps: the thumb is written
            // in place, so the row must be re-marked or a stale thumb can
            // linger when the row was otherwise clean.
            buf.mark(track_x, y);
        }
    }
}

/// Index a buffer cell by absolute (x, y) via direct `content` indexing.
/// The caller guarantees the coordinate lies inside `area`.
fn cell_at_index(
    buf: &mut nuotc::Grid,
    area: Rect,
    x: u16,
    y: u16,
) -> &mut nuotc::Cell {
    let idx = (y as usize - area.y as usize) * area.width as usize + (x as usize - area.x as usize);
    &mut buf.content[idx]
}

/// Contrast foreground for a colored background (dark text on light fills).
pub(crate) fn contrast_fg(bg: Color) -> Color {
    let (r, g, b) = rgb(bg);
    let luminance = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    if luminance > 140.0 {
        Color::Black
    } else {
        Color::White
    }
}

pub(crate) fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::Red => (224, 108, 117),
        Color::Green => (127, 216, 143),
        Color::Yellow => (229, 192, 123),
        Color::Blue => (137, 180, 250),
        Color::Magenta => (203, 166, 247),
        Color::Cyan => (86, 182, 194),
        Color::Gray => (128, 128, 128),
        Color::DarkGray => (64, 64, 64),
        Color::LightGreen => (127, 216, 143),
        Color::LightRed => (243, 139, 168),
        _ => (128, 128, 128),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::MODAL_INNER_H_PADDING;
    use nuotc::{Frame, Rect, Style};

    #[test]
    fn fixed_and_content_modal_specs_preserve_their_sizing_modes() {
        let fixed = FixedModalSpec::PROVIDER;
        assert_eq!(fixed.spec.width_percent, 76);
        assert_eq!(fixed.height_percent, 80);

        let content = ContentModalSpec::TOOLS;
        assert_eq!(content.spec.width_percent, 64);
        assert_eq!(content.min_rows, 11);
        assert_eq!(content.max_viewport_percent, 84);
    }

    #[test]
    fn dim_surface_preserves_more_code_text_contrast() {
        let theme = Theme::default();
        let mut grid = nuotc::Grid::new(2, 1);
        grid.set(
            0,
            0,
            nuotc::Cell::narrow(
                "c",
                Style::default()
                    .fg(theme.code_text())
                    .bg(theme.code_surface()),
            ),
        );
        grid.set(
            1,
            0,
            nuotc::Cell::narrow("p", Style::default().fg(theme.fg()).bg(theme.surface())),
        );

        let mut frame = Frame::new(&mut grid);
        dim_surface(&mut frame, &theme);
        let code = frame.buffer_mut()[(0, 0)].clone();
        let prose = frame.buffer_mut()[(1, 0)].clone();

        assert_eq!(
            code.bg,
            scale_color(theme.code_surface(), theme.modal_dim_factor())
        );
        assert_eq!(
            code.fg,
            scale_color(theme.code_text(), theme.modal_dim_factor() + 0.25)
        );
        assert_eq!(prose.fg, scale_color(theme.fg(), theme.modal_dim_factor()));
    }

    #[test]
    fn modal_footer_degrades_by_width_and_priority() {
        let hints = [
            FooterHint::secondary("type", "filter"),
            FooterHint::navigation("↑↓", "navigate"),
            FooterHint::primary("Enter", "activate"),
            FooterHint::secondary("*", "favorite"),
            FooterHint::always("Esc", "close"),
        ];

        // Full width: every label kept, no `?` chip (show_more = false).
        // R2: peer affordances join with plain whitespace, not `·`.
        assert_eq!(
            modal_footer_text(&hints, 80),
            "type filter  ↑↓ navigate  Enter activate  * favorite  Esc close"
        );
        // Narrow widths drop lower-priority items. Assert invariants rather
        // than brittle full strings (the ladder depends on the budget).
        // Always keeps Esc; Primary keeps Enter; no `?` (default path).
        let mid = modal_footer_text(&hints, 30);
        assert!(
            mid.contains("Esc") || mid.starts_with('E') || mid.ends_with('…'),
            "narrow keeps Esc: {mid}"
        );
        assert!(!mid.contains('?'), "default path never appends ?: {mid}");
        assert_eq!(modal_footer_text(&hints, 3), "Esc");
        // 2 cols is too short for "Esc" — truncate with ellipsis.
        let tiny = modal_footer_text(&hints, 2);
        assert!(
            tiny.ends_with('…') || tiny == "E…",
            "tiny width truncates: {tiny}"
        );
    }

    #[test]
    fn even_width_floors_to_nearest_even_column() {
        assert_eq!(even_width(Rect::new(0, 0, 0, 5)), Rect::new(0, 0, 0, 5));
        assert_eq!(even_width(Rect::new(0, 0, 1, 5)), Rect::new(0, 0, 0, 5));
        assert_eq!(even_width(Rect::new(0, 0, 2, 5)), Rect::new(0, 0, 2, 5));
        assert_eq!(even_width(Rect::new(7, 3, 15, 9)), Rect::new(7, 3, 14, 9));
        assert_eq!(even_width(Rect::new(7, 3, 16, 9)), Rect::new(7, 3, 16, 9));
    }

    #[test]
    fn centered_rect_produces_even_width() {
        // `centered_rect` must shed the odd trailing column so the body it
        // encloses gets an even usable width. Check several host widths and
        // percentages — including odd host widths and odd splits. Centering
        // itself is the `Layout` engine's integer-division behaviour (which
        // already tolerates a column of asymmetry on percentage splits); the
        // guarantee we add is purely that the resulting width is even.
        for &host_w in &[79u16, 80, 81, 120, 121] {
            for &percent in &[50u16, 58, 64, 66, 72, 80] {
                let host = Rect::new(0, 0, host_w, 40);
                let area = centered_rect(percent, 50, host);
                assert_eq!(
                    area.width % 2,
                    0,
                    "width {w} for host {host_w}@{percent}% must be even",
                    w = area.width
                );
            }
        }
    }

    #[test]
    fn centered_rect_h_produces_even_width() {
        for &host_w in &[79u16, 80, 81, 120, 121] {
            for &percent in &[60u16, 64, 66] {
                let host = Rect::new(0, 0, host_w, 40);
                let area = centered_rect_h(percent, 12, host);
                assert_eq!(
                    area.width % 2,
                    0,
                    "width {w} for host {host_w}@{percent}% must be even",
                    w = area.width
                );
            }
        }
    }

    #[test]
    fn modal_area_body_is_even_width_for_cjk() {
        // The end-to-end invariant: a modal panel plus its symmetric inner
        // padding yields an even body width, so a run of full-width (2-col)
        // CJK glyphs tiles every row without stranding a trailing column.
        // Use a real grid + frame the way the renderers do, across odd and
        // even terminal widths.
        for &cols in &[79u16, 80, 81, 119, 120, 121, 200] {
            let mut grid = nuotc::Grid::new(cols, 50);
            let frame = Frame::new(&mut grid);
            let area = modal_area(&frame, FixedModalSpec::SESSIONS);
            assert_eq!(
                area.width % 2,
                0,
                "modal panel width must be even at {cols} cols"
            );
            // `modal_frame` insets by MODAL_INNER_H_PADDING on each side.
            let body_w = area.width.saturating_sub(2 * MODAL_INNER_H_PADDING);
            assert_eq!(
                body_w % 2,
                0,
                "modal body width must be even at {cols} cols (was {body_w})"
            );
        }
    }

    #[test]
    fn breadcrumb_parts_composes_parent_separator_child() {
        // The single breadcrumb convention for hierarchical modal headers.
        // A drill-in sub-page renders `Parent › Child`: muted parent, the
        // centralized `›` separator, bold child. Pins both the order/kind of
        // the parts and the exact separator glyph so all sub-pages stay uniform.
        let parts = breadcrumb_parts("Sessions", "Info");
        assert_eq!(parts.len(), 3);
        assert!(matches!(
            parts[0],
            HeaderPart::Text {
                text: "Sessions",
                accent: false
            }
        ));
        assert!(matches!(
            parts[1],
            HeaderPart::Text {
                text: " › ",
                accent: false
            }
        ));
        assert!(matches!(parts[2], HeaderPart::Title("Info")));
    }

    #[test]
    fn hierarchical_breadcrumb_handles_full_and_truncated_widths() {
        let levels = ["Connections", "Add", "Google Antigravity"];
        // Full width fitting
        let full = hierarchical_breadcrumb(&levels, 60);
        assert_eq!(full.len(), 5); // Connections, sep, Add, sep, Google Antigravity
        assert!(matches!(
            full[0],
            HeaderPart::Text {
                text: "Connections",
                ..
            }
        ));
        assert!(matches!(full[4], HeaderPart::Title("Google Antigravity")));

        // Tight width dropping "Connections"
        let truncated = hierarchical_breadcrumb(&levels, 32);
        assert_eq!(truncated.len(), 5); // ..., sep, Add, sep, Google Antigravity
        assert!(matches!(truncated[0], HeaderPart::Text { text: "...", .. }));
        assert!(matches!(
            truncated[4],
            HeaderPart::Title("Google Antigravity")
        ));

        // Very tight width dropping "Add" as well
        let tight = hierarchical_breadcrumb(&levels, 24);
        assert_eq!(tight.len(), 3); // ..., sep, Google Antigravity
        assert!(matches!(tight[0], HeaderPart::Text { text: "...", .. }));
        assert!(matches!(tight[2], HeaderPart::Title("Google Antigravity")));
    }

    #[test]
    fn modal_area_is_vertically_centered_within_frame() {
        for &rows in &[30u16, 40, 45, 50, 60] {
            let mut grid = nuotc::Grid::new(100, rows);
            let frame = Frame::new(&mut grid);
            let area = modal_area(&frame, FixedModalSpec::SESSIONS);
            let top_gap = area.y;
            let bot_gap = rows.saturating_sub(area.y + area.height);
            assert!(
                (top_gap as i32 - bot_gap as i32).abs() <= 1,
                "modal_area vertically centered at height {rows}: top_gap={top_gap}, bot_gap={bot_gap}"
            );
        }
    }

    #[test]
    fn content_modal_area_is_vertically_centered_within_frame() {
        for &rows in &[30u16, 40, 45, 50, 60] {
            let mut grid = nuotc::Grid::new(100, rows);
            let frame = Frame::new(&mut grid);
            let area = content_modal_area(&frame, ContentModalSpec::PERMISSIONS, 15);
            let top_gap = area.y;
            let bot_gap = rows.saturating_sub(area.y + area.height);
            assert!(
                (top_gap as i32 - bot_gap as i32).abs() <= 1,
                "content_modal_area vertically centered at height {rows}: top_gap={top_gap}, bot_gap={bot_gap}"
            );
        }
    }
}
