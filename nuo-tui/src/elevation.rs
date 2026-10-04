//! The declarative visual elevation and structural framing system (ADR-0181).
//!
//! # Architecture
//!
//! Rather than hardcoding raw background colors or ad-hoc border checks scattered across
//! view components, visual hierarchy is modeled through formal [`ElevationLevel`]s and
//! rendered via [`ElevationContainer`].
//!
//! Depending on the active [`nuotc::ElevationArchetype`]:
//! - **Chromatic (TrueColor):** Surfaces are distinguished by subtle background luminance deltas
//!   (`app_bg` -> `surface` -> `code_bg` -> `panel_bg`). Zero border/margin overhead.
//! - **Structured (Monochrome / Linux VT / getty):** Background fills are forbidden or clamped.
//!   Boundaries are rendered as explicit ASCII/CP437 boxes (`+--+`, `|`), and active focus
//!   strictly uses DEC VT100 standard **reverse video (`Modifier::REVERSE` / SGR 7)**.
//! - **Hybrid (Ansi16):** Employs standard 16-color high-contrast foregrounds and clean dividers.

use nuotc::{
    Block as RtBlock, Clear, Color, Constraint, Direction, Frame, Layout, Line, Margin, Modifier,
    Paragraph, Rect, Span, Style,
};

use crate::design::{MODAL_INNER_H_PADDING, MODAL_INNER_V_PADDING};
use crate::render::Theme;

/// Semantic hierarchy level of a visual surface (ADR-0181).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ElevationLevel {
    /// Base application background (lowest level).
    #[default]
    Surface,
    /// Elevated cards, tools, message disclosures, code regions.
    Card,
    /// Prominent panels, inputs, docks, sidebars.
    Panel,
    /// Floating sheets, modals, dialogs, overlays (highest level).
    Overlay,
}

/// Declarative elevation container managing background and framing per archetype (ADR-0181).
pub struct ElevationContainer {
    level: ElevationLevel,
    focused: bool,
}

impl ElevationContainer {
    /// Create a new container with the specified elevation level.
    pub fn new(level: ElevationLevel) -> Self {
        Self {
            level,
            focused: false,
        }
    }

    /// Convenience constructor for Card elevation (code blocks, tool steps).
    pub fn card() -> Self {
        Self::new(ElevationLevel::Card)
    }

    /// Convenience constructor for Panel elevation (docks, sidebars).
    pub fn panel() -> Self {
        Self::new(ElevationLevel::Panel)
    }

    /// Convenience constructor for Overlay elevation (floating modals, dialogs).
    pub fn overlay() -> Self {
        Self::new(ElevationLevel::Overlay)
    }

    /// Mark this container as currently focused (triggers DEC VT100 reverse video in Structured mode).
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Render container into frame and return the safe inner content area.
    pub fn render(self, frame: &mut Frame, area: Rect, theme: &Theme) -> Rect {
        if area.width == 0 || area.height == 0 {
            return area;
        }

        match theme.elevation {
            nuotc::ElevationArchetype::Chromatic => {
                let bg = match self.level {
                    ElevationLevel::Surface => theme.surface(),
                    ElevationLevel::Card => theme.code_bg,
                    ElevationLevel::Panel => theme.panel(),
                    ElevationLevel::Overlay => theme.panel(),
                };
                let style = if self.focused {
                    Style::default().bg(bg).fg(theme.text)
                } else {
                    Style::default().bg(bg)
                };
                frame.render_widget(RtBlock::default().style(style), area);
                area
            }
            nuotc::ElevationArchetype::Structured => {
                let mut style = Style::default().fg(theme.text).bg(theme.surface());
                if self.focused {
                    style = style.add_modifier(Modifier::REVERSE);
                }
                let block = RtBlock::default()
                    .borders(nuotc::Borders::ALL)
                    .border_type(theme.glyphs.border_type())
                    .style(style);
                let inner = block.inner(area);
                frame.render_widget(block, area);
                inner
            }
            nuotc::ElevationArchetype::Hybrid => {
                let bg = match self.level {
                    ElevationLevel::Surface => theme.surface(),
                    ElevationLevel::Card => theme.code_bg,
                    ElevationLevel::Panel => theme.panel(),
                    ElevationLevel::Overlay => theme.panel(),
                };
                let mut style = Style::default().bg(bg).fg(theme.text);
                if self.focused {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if self.level == ElevationLevel::Overlay || self.level == ElevationLevel::Panel {
                    let block = RtBlock::default()
                        .borders(nuotc::Borders::ALL)
                        .border_type(nuotc::BorderType::Plain)
                        .style(style);
                    let inner = block.inner(area);
                    frame.render_widget(block, area);
                    inner
                } else {
                    frame.render_widget(RtBlock::default().style(style), area);
                    area
                }
            }
        }
    }
}

/// An elevated panel container: under `Chromatic`, a borderless panel with a single left bar;
/// under `Structured` (Monochrome/Linux VT), an explicit full ASCII/Unicode bounding box (ADR-0181).
pub(crate) fn panel_block(theme: &Theme, bar_color: Color, bg: Color) -> RtBlock<'static> {
    match theme.elevation {
        nuotc::ElevationArchetype::Structured => RtBlock::default()
            .borders(nuotc::Borders::ALL)
            .border_type(theme.glyphs.border_type())
            .border_style(Style::default().fg(theme.text))
            .style(Style::default().bg(bg)),
        nuotc::ElevationArchetype::Hybrid => RtBlock::default()
            .borders(nuotc::Borders::ALL)
            .border_type(nuotc::BorderType::Plain)
            .border_style(Style::default().fg(bar_color))
            .style(Style::default().bg(bg)),
        nuotc::ElevationArchetype::Chromatic => RtBlock::default()
            .borders(nuotc::Borders::LEFT)
            .border_type(nuotc::BorderType::Thick)
            .glyph_v(theme.glyphs.border_v)
            .border_style(Style::default().fg(bar_color))
            .style(Style::default().bg(bg)),
    }
}

/// Section rects produced by [`modal_frame`]: the header and footer are
/// `Option`al (omitted when the modal asked for none), and `body` is always
/// present and flexes to fill whatever the header/footer leave behind.
pub(crate) struct ModalFrame {
    pub header: Option<Rect>,
    pub body: Rect,
    pub footer: Option<Rect>,
}

/// Render the unified modal title into the header rect produced by
/// [`modal_frame`]. This is the single place every centered modal's
/// `brand + BOLD` title is painted, so the header style no longer needs to be
/// repeated per-component. The two-line variants (a muted breadcrumb followed
/// by a brand title, e.g. `Configuration › Layout`) pass the parts via
/// [`HeaderPart`]; the common case is a single [`HeaderPart::title`].
pub(crate) fn modal_header(frame: &mut Frame, header: Option<Rect>, title: &str, theme: &Theme) {
    modal_header_parts(frame, header, &[HeaderPart::title(title)], theme);
}

/// A styled segment of a modal header line, laid out left-to-right.
#[derive(Clone, Copy)]
pub(crate) enum HeaderPart<'a> {
    /// The primary title: `brand` color, bold.
    Title(&'a str),
    /// A leading/trailing muted segment (e.g. the `Configuration › ` breadcrumb
    /// or `← ` back affordance). `accent` makes it the brand tone instead.
    Text { text: &'a str, accent: bool },
}

impl<'a> HeaderPart<'a> {
    pub(crate) const fn title(text: &'a str) -> Self {
        HeaderPart::Title(text)
    }
}

/// Paint a multi-part modal header line (`[prefix, title, suffix]`) into
/// `header` left-to-right. Empty spans and `None` headers are no-ops.
pub(crate) fn modal_header_parts(
    frame: &mut Frame,
    header: Option<Rect>,
    parts: &[HeaderPart<'_>],
    theme: &Theme,
) {
    let Some(header) = header else { return };
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(parts.len());
    for part in parts {
        match *part {
            HeaderPart::Title(title) => {
                spans.push(Span::styled(
                    title.to_string(),
                    Style::default()
                        .fg(theme.brand())
                        .add_modifier(Modifier::BOLD),
                ));
            }
            HeaderPart::Text { text, accent } => {
                let fg = if accent { theme.brand() } else { theme.muted() };
                spans.push(Span::styled(text.to_string(), Style::default().fg(fg)));
            }
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), header);
}

/// Every centered modal goes through this so the panel style lives in one
/// place: under Chromatic archetype, a borderless solid-bg panel with
/// `MODAL_INNER_H_PADDING`/`MODAL_INNER_V_PADDING` inner padding; under Structured
/// and Hybrid archetypes, an explicit standard border plus padding (ADR-0181).
/// Then a vertical split into optional `header` (1 row) / `body` (flex) / optional
/// 1-row gap + `footer` (1 row). The caller renders its own header / body /
/// footer content into the returned rects.
pub(crate) fn modal_frame(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    header: bool,
    footer: bool,
) -> ModalFrame {
    frame.render_widget(Clear, area);

    let inner = match theme.elevation {
        nuotc::ElevationArchetype::Structured => {
            let block = RtBlock::default()
                .borders(nuotc::Borders::ALL)
                .border_type(theme.glyphs.border_type())
                .style(Style::default().fg(theme.text).bg(theme.panel()));
            let framed = block.inner(area);
            frame.render_widget(block, area);
            framed.inner(Margin {
                horizontal: 1,
                vertical: 0,
            })
        }
        nuotc::ElevationArchetype::Hybrid => {
            let block = RtBlock::default()
                .borders(nuotc::Borders::ALL)
                .border_type(nuotc::BorderType::Plain)
                .style(Style::default().fg(theme.text).bg(theme.panel()));
            let framed = block.inner(area);
            frame.render_widget(block, area);
            framed.inner(Margin {
                horizontal: 1,
                vertical: 0,
            })
        }
        nuotc::ElevationArchetype::Chromatic => {
            frame.render_widget(
                RtBlock::default().style(Style::default().bg(theme.panel())),
                area,
            );
            area.inner(Margin {
                horizontal: MODAL_INNER_H_PADDING,
                vertical: MODAL_INNER_V_PADDING,
            })
        }
    };

    // Tagged constraints so we can map split chunks back to sections:
    // 0 = header, 4 = gap after header, 1 = body, 2 = gap before footer,
    // 3 = footer. Both gaps are 1 row so the body always sits one blank line
    // below the header and one above the footer — regardless of which sections
    // a modal asks for.
    let mut tagged: Vec<(u8, Constraint)> = Vec::new();
    if header {
        tagged.push((0, Constraint::Length(1)));
        tagged.push((4, Constraint::Length(1)));
    }
    tagged.push((1, Constraint::Min(0)));
    if footer {
        tagged.push((2, Constraint::Length(1)));
        tagged.push((3, Constraint::Length(1)));
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(tagged.iter().map(|(_, c)| *c))
        .split(inner);

    let mut out = ModalFrame {
        header: None,
        body: inner,
        footer: None,
    };
    for (i, (tag, _)) in tagged.iter().enumerate() {
        match tag {
            0 => out.header = Some(chunks[i]),
            1 => out.body = chunks[i],
            3 => out.footer = Some(chunks[i]),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuotc::{ElevationArchetype, Rect};

    #[test]
    fn elevation_container_chromatic_vs_structured() {
        let theme_chromatic = Theme::default();
        assert_eq!(theme_chromatic.elevation, ElevationArchetype::Chromatic);
        let mut grid = nuotc::Grid::new(20, 10);
        let mut frame = Frame::new(&mut grid);
        let area = Rect::new(0, 0, 20, 10);
        let inner = ElevationContainer::panel().render(&mut frame, area, &theme_chromatic);
        assert_eq!(inner, area);

        let theme_mono = Theme::monochrome();
        assert_eq!(theme_mono.elevation, ElevationArchetype::Structured);
        let mut grid_mono = nuotc::Grid::new(20, 10);
        let mut frame_mono = Frame::new(&mut grid_mono);
        let inner_mono = ElevationContainer::panel().render(&mut frame_mono, area, &theme_mono);
        assert_eq!(inner_mono, Rect::new(1, 1, 18, 8));

        assert_eq!(grid_mono.get(0, 0).unwrap().symbol(), "+");
        assert_eq!(grid_mono.get(1, 0).unwrap().symbol(), "-");
        assert_eq!(grid_mono.get(0, 1).unwrap().symbol(), "|");
    }

    #[test]
    fn elevation_container_focused_reverse_video() {
        let theme_mono = Theme::monochrome();
        let mut grid = nuotc::Grid::new(10, 5);
        let mut frame = Frame::new(&mut grid);
        let area = Rect::new(0, 0, 10, 5);
        ElevationContainer::card()
            .focused(true)
            .render(&mut frame, area, &theme_mono);

        let tl_cell = grid.get(0, 0).unwrap();
        assert!(tl_cell.style.add.contains(Modifier::REVERSE));
    }

    #[test]
    fn modal_frame_renders_structured_borders_on_monochrome() {
        let theme_mono = Theme::monochrome();
        let mut grid = nuotc::Grid::new(40, 15);
        let mut frame = Frame::new(&mut grid);
        let area = Rect::new(5, 2, 30, 10);

        let mf = modal_frame(&mut frame, area, &theme_mono, true, true);
        assert!(mf.header.is_some());
        assert!(mf.footer.is_some());

        assert_eq!(grid.get(5, 2).unwrap().symbol(), "+");
        assert_eq!(grid.get(6, 2).unwrap().symbol(), "-");
        assert_eq!(grid.get(5, 3).unwrap().symbol(), "|");
        assert_eq!(grid.get(34, 11).unwrap().symbol(), "+");
    }
}
