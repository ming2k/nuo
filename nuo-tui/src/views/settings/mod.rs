//! Modular Settings View (`/settings`): first-class, full-screen configuration center (ADR-0141).
//!
//! Layout: the shared two-row head band (row 1 the session identity, row 2 the
//! scene row naming `settings` with the category breadcrumb, ADR-0024), then a
//! two-pane body — a left category nav (`panel` tone) and the right detail pane
//! (a sunken body tone). There is no footer band: the exits live on the scene
//! row. There is also no per-pane prose header naming the selected category —
//! the highlighted nav item already says which pane is active, so the right pane
//! is pure content.
//!
//! Subdivided into dedicated per-category modules:
//! - [`appearance`]: Themes and palette swatches
//! - [`components`]: Interactive component styles, default disclosure states, and auto-scroll
//! - [`web`]: singleton Web Search and Web Reader provider selection
//! - [`system`]: Paths, runtime info, version

pub mod appearance;
pub mod components;
pub mod system;
pub mod web;

pub use web::{build_websearch_provider_dropdown, build_websearch_reader_dropdown};

use nuo_wire::ColorSchemeConfig;
use nuotc::{
    Block as RtBlock, Clear, Constraint, Direction, Frame, Layout, Line, Modifier, Paragraph, Rect,
    Span, Style, Wrap,
};

use crate::primitives::{ElevationContainer, SCROLL_EDGE_MARGIN, draw_scrollbar, resolve_scroll};
use crate::render::Theme;
use crate::view_header::{
    SessionHead, ViewHints, ViewKind, draw_view_header, draw_view_header_hints,
};
/// Width of the left navigation pane (its outer rect, before inner padding).
const NAV_WIDTH: u16 = 22;

/// Inner padding applied to each pane: 1 row top/bottom and 2 columns
/// left/right. Symmetric so the two panes' content aligns, neither hugs the
/// pane border, and the wider column inset gives the identity columns room to
/// breathe against the pane edge.
const BODY_PAD_ROWS: u16 = 1;
const BODY_PAD_COLS: u16 = 2;

/// Shrink a rect by the shared pane padding (`rows` vertically, `cols`
/// horizontally, saturating).
fn inset_body(rect: Rect) -> Rect {
    Rect {
        x: rect.x.saturating_add(BODY_PAD_COLS),
        y: rect.y.saturating_add(BODY_PAD_ROWS),
        width: rect.width.saturating_sub(BODY_PAD_COLS.saturating_mul(2)),
        height: rect.height.saturating_sub(BODY_PAD_ROWS.saturating_mul(2)),
    }
}

/// Which pane of the Settings View currently owns keyboard focus.
#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub enum ConfigFocus {
    /// Left pane: settings category navigation.
    #[default]
    Categories,
    /// Right pane: Detail configuration options and controls.
    Detail,
}

/// The top-level settings categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigCategory {
    Appearance = 0,
    Components = 1,
    WebSearch = 2,
    WebReader = 3,
    System = 4,
}

impl ConfigCategory {
    pub const ALL: [ConfigCategory; 5] = [
        ConfigCategory::Appearance,
        ConfigCategory::Components,
        ConfigCategory::WebSearch,
        ConfigCategory::WebReader,
        ConfigCategory::System,
    ];

    pub fn from_index(index: usize) -> Self {
        match index % Self::ALL.len() {
            0 => ConfigCategory::Appearance,
            1 => ConfigCategory::Components,
            2 => ConfigCategory::WebSearch,
            3 => ConfigCategory::WebReader,
            _ => ConfigCategory::System,
        }
    }

    /// Return the exact dynamic count of selectable items in this category's detail pane.
    pub fn detail_item_count(
        self,
        ws_path: Option<&std::path::Path>,
        websearch: Option<&nuo_wire::WebSearchConfigView>,
        profile: &nuotc::TerminalProfile,
    ) -> usize {
        match self {
            ConfigCategory::Appearance => appearance::item_count(ws_path, profile),
            ConfigCategory::Components => components::item_count(),
            ConfigCategory::WebSearch => web::search_item_count(websearch),
            ConfigCategory::WebReader => web::reader_item_count(websearch),
            ConfigCategory::System => system::item_count(),
        }
    }

    /// Match a category by name, slug, or numeric index string (case-insensitive).
    pub fn from_name(name: &str) -> Option<Self> {
        let trimmed = name.trim().to_ascii_lowercase();
        match trimmed.as_str() {
            "0" | "appearance" | "theme" | "themes" | "look" => Some(ConfigCategory::Appearance),
            "1" | "components" | "component" | "interactive" | "disclosure" | "widgets" => {
                Some(ConfigCategory::Components)
            }
            "2" | "search" | "websearch" | "web-search" => Some(ConfigCategory::WebSearch),
            "3" | "reader" | "webreader" | "web-reader" | "web" => Some(ConfigCategory::WebReader),
            "4" | "system" | "info" | "about" | "paths" | "runtime" => Some(ConfigCategory::System),
            _ => None,
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            ConfigCategory::Appearance => "appearance",
            ConfigCategory::Components => "components",
            ConfigCategory::WebSearch => "search",
            ConfigCategory::WebReader => "reader",
            ConfigCategory::System => "system",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            ConfigCategory::Appearance => "Appearance",
            ConfigCategory::Components => "Components",
            ConfigCategory::WebSearch => "Web Search",
            ConfigCategory::WebReader => "Web Reader",
            ConfigCategory::System => "System & Info",
        }
    }
}

impl std::fmt::Display for ConfigCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.slug())
    }
}

impl std::str::FromStr for ConfigCategory {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_name(s).ok_or_else(|| {
            format!(
                "unknown settings category '{s}' (expected appearance, components, search, web, system, or 0..4)"
            )
        })
    }
}

/// Geometry sub-rects returned by [`draw_settings_view`].
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ConfigRects {
    pub area: Rect,
    pub category_body: Rect,
    pub detail_body: Rect,
    pub selected_row_rect: Option<Rect>,
    /// Every visible detail row as `(detail_index, rect)`, mounted as pointer
    /// targets so the row under the mouse can light up.
    pub row_rects: Vec<(usize, Rect)>,
    /// Bounding rects for each rendered tab in the top tab bar: `(tab_index, rect)`.
    pub tab_rects: Vec<(usize, Rect)>,
}

/// Properties passed to render the complete Settings View.
pub struct SettingsProps<'a> {
    pub category_index: usize,
    pub detail_index: usize,
    /// Detail row currently under the mouse pointer, if any. Drives the row's
    /// hover band so the pointer and the keyboard cursor share one affordance;
    /// `None` when the pointer is elsewhere.
    pub hover_index: Option<usize>,
    pub focus: ConfigFocus,
    pub color_scheme: &'a str,
    pub custom_color_scheme: &'a ColorSchemeConfig,
    pub websearch: Option<&'a nuo_wire::WebSearchConfigView>,
    pub workspace: &'a str,
    pub category_scroll: &'a mut usize,
    pub detail_scroll: &'a mut usize,
    pub breadcrumbs: Option<&'a str>,
    pub theme: &'a Theme,
    pub profile: &'a nuotc::TerminalProfile,
    pub tui_config: &'a crate::config::TuiConfig,
    /// The session identity for the head band's top row (ADR-0024): the band is
    /// uniform across scenes, so Settings draws the same `SESSION` row as the
    /// thread. `None` hides row 1. Crate-visible only (`SessionHead` is
    /// crate-private; the shell is the sole caller).
    pub(crate) session_head: Option<SessionHead<'a>>,
    /// The session's persistent run-mode flags for the head band's scene row.
    pub(crate) unattended: bool,
    pub(crate) confined: bool,
}

/// Draw the full-screen Settings View.
pub fn draw_settings_view(frame: &mut Frame, mut props: SettingsProps<'_>) -> ConfigRects {
    let area = frame.area();
    frame.render_widget(Clear, area);

    // Fill full background with canvas tone
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(props.theme.surface())),
        area,
    );

    // 3 vertical zones: the two-row head band (session identity + scene row,
    // ADR-0024), then the body (flexible). There is deliberately **no footer
    // band**: the Settings center's own affordances — including its exit —
    // already live on the head band's scene row, so a redundant bottom keycap
    // strip only stole vertical space from the panes it described.
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(6),
        ])
        .split(area);

    let header_rect = vertical_chunks[0];
    let subhead_rect = vertical_chunks[1];
    let body_rect = vertical_chunks[2];

    let category = ConfigCategory::from_index(props.category_index);

    // 1. Top Header Row (session identity, shared across scenes)
    let tab_rects = if let Some(head) = props.session_head {
        draw_view_header(frame, header_rect, &head, props.theme)
    } else {
        Vec::new()
    };

    // 2. Scene row: the `settings` scene name, the category breadcrumb context,
    //    the run-mode flags, and the namespace pair (ADR-0024).
    let view_hints = ViewHints {
        kind: ViewKind::Settings,
        context: props.breadcrumbs.map(str::trim),
        context_warn: false,
        unattended: props.unattended,
        confined: props.confined,
        workspace: None,
        breadcrumbs: None,
        can_back: false,
        can_forward: false,
    };
    draw_view_header_hints(frame, subhead_rect, &view_hints, props.theme);

    // 3. Center Body. The two panes each carry a 1-row / 2-column inner padding,
    // and the two panes are colour-differentiated from each other: the left nav
    // sits on `panel()`, the right detail body on the deeper `pane_sunken()`
    // rung — neither collapses onto the `raised()` head band above them.
    let inner_body = body_rect;

    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(NAV_WIDTH), Constraint::Min(20)])
        .split(inner_body);

    let category_rect = body_chunks[0];
    let detail_rect = body_chunks[1];

    // Left pane contrasting surface (panel tone, distinct from view surface)
    let category_pane = ElevationContainer::panel().render(frame, category_rect, props.theme);

    // Right pane main canvas body (sunken tone, distinct from the head band
    // above and the left nav beside it).
    let detail_pane = Rect {
        x: detail_rect.x,
        y: detail_rect.y,
        width: detail_rect.width,
        height: detail_rect.height,
    };
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(props.theme.pane_sunken())),
        detail_pane,
    );

    // Left pane nav body, inset by the shared pane padding.
    let category_inner = inset_body(category_pane);
    draw_categories_pane(frame, category_inner, &mut props);

    // Right pane: the detail body fills the pane beneath the shared padding.
    // There is deliberately **no per-pane header band**: the selected category
    // is already named by the highlighted left-nav item, so a prose header
    // restating it ("Theme selection and color palette customization.") only
    // repeated the navigation and spent a strip of chrome saying nothing. The
    // body's own `pane_sunken` tone already separates this pane from the left
    // nav (`panel`) and from the head band above (`raised`).
    let detail_content = inset_body(detail_pane);

    // The detail body carries the sunken tone, so its rows rest on it.
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(props.theme.pane_sunken())),
        detail_content,
    );

    let detail_inner_rect = detail_content;

    let focused = props.focus == ConfigFocus::Detail;
    let detail = match category {
        ConfigCategory::Appearance => {
            appearance::draw_appearance_detail(frame, detail_inner_rect, &mut props, focused)
        }
        ConfigCategory::Components => {
            components::draw_components_detail(frame, detail_inner_rect, &mut props, focused)
        }
        ConfigCategory::WebSearch => {
            web::draw_search_detail(frame, detail_inner_rect, &mut props, focused)
        }
        ConfigCategory::WebReader => {
            web::draw_reader_detail(frame, detail_inner_rect, &mut props, focused)
        }
        ConfigCategory::System => {
            system::draw_system_detail(frame, detail_inner_rect, &mut props, focused)
        }
    };

    // No footer band: the Settings center has no bottom key strip. Its own
    // affordances (and its exit) already live on the head band's namespace row.

    ConfigRects {
        area,
        category_body: category_rect,
        detail_body: detail_rect,
        selected_row_rect: detail.selected_row,
        row_rects: detail.rows,
        tab_rects,
    }
}

fn draw_categories_pane(frame: &mut Frame, area: Rect, props: &mut SettingsProps<'_>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let is_focused = props.focus == ConfigFocus::Categories;

    let selected_line = Some(props.category_index * 2);

    for (i, cat) in ConfigCategory::ALL.iter().enumerate() {
        let is_selected = i == props.category_index;

        let mut style = if is_selected && is_focused {
            Style::default()
                .fg(props.theme.brand())
                .add_modifier(Modifier::BOLD)
        } else if is_selected {
            Style::default()
                .fg(props.theme.fg())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(props.theme.muted())
        };
        if is_selected && props.theme.elevation.is_structured() {
            style = style.add_modifier(Modifier::REVERSE);
        }

        lines.push(Line::from(vec![Span::styled(cat.title(), style)]));
        lines.push(Line::from(""));
    }

    // The pane's 1-row / 2-column padding is applied by the caller, so the nav
    // body fills this rect as-is.
    let inner_area = area;

    let visible_rows = inner_area.height as usize;
    let content_len = lines.len();

    let (content_offset, max_scroll) = resolve_scroll(
        props.category_scroll,
        visible_rows,
        content_len,
        selected_line,
        SCROLL_EDGE_MARGIN,
    );

    let p = Paragraph::new(lines)
        .scroll(content_offset as u16, 0)
        .style(Style::default().bg(props.theme.panel()));
    frame.render_widget(p, inner_area);

    if max_scroll > 0 && area.width > 5 {
        // Keep the track inside the nav pane's own right edge (its last column,
        // always blank) instead of the adjacent detail pane.
        let track_host = Rect {
            x: area.x,
            y: area.y,
            width: area.width.saturating_sub(2),
            height: area.height,
        };
        draw_scrollbar(frame, track_host, content_offset, max_scroll, props.theme);
    }
}

/// Screen rects reported by a scrollable detail pane.
pub(super) struct ScrollableRects {
    /// The keyboard cursor's row (used to anchor dropdown popovers).
    pub selected_row: Option<Rect>,
    /// Every *visible* selectable row, as `(detail_index, rect)`. Mounted as
    /// pointer hit targets so a row can light up under the mouse.
    pub rows: Vec<(usize, Rect)>,
}

/// Render a scrollable detail pane and report where its selectable rows landed.
///
/// `selectable` maps each selectable row's `detail_index` to the content line
/// where that row begins, in ascending line order. Each row's reported rect
/// spans every line up to the next row's start — the row's own label line, any
/// wrapped description lines, and the blank separator beneath it — so the whole
/// block responds to the pointer with no dead gaps. Rows scrolled out of the
/// viewport are omitted.
///
/// Rows that carry no highlight rest on the pane's own sunken body tone
/// (`theme.pane_sunken()`), so a detail pane reads as a recessed region distinct
/// from the `raised` head band above it.
pub(super) fn render_scrollable_indexed(
    frame: &mut Frame,
    rect: Rect,
    lines: Vec<Line<'static>>,
    scroll: &mut usize,
    selected_line: Option<usize>,
    selectable: &[(usize, usize)],
    theme: &Theme,
) -> ScrollableRects {
    let visible = rect.height as usize;
    let content_len = lines.len();
    let body_bg = theme.pane_sunken();

    let (content_offset, max_scroll) = resolve_scroll(
        scroll,
        visible,
        content_len,
        selected_line,
        SCROLL_EDGE_MARGIN,
    );

    // The detail pane owns the full pane width (no outer margin), so the
    // scrollbar has nowhere to live *outside* the content. Draw it in the
    // pane's own last column instead: every detail row pads itself to the full
    // width, so that column is always the row's own blank space and the track
    // never covers text. (Passing a body two columns narrower makes
    // `draw_scrollbar`'s `body.width + SCROLLBAR_GAP` land on the last column.)
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll(content_offset as u16, 0)
        .style(Style::default().bg(body_bg));
    frame.render_widget(p, rect);

    if max_scroll > 0 && rect.width > 4 {
        let track_host = Rect {
            x: rect.x,
            y: rect.y,
            width: rect.width.saturating_sub(2),
            height: rect.height,
        };
        draw_scrollbar(frame, track_host, content_offset, max_scroll, theme);
    }

    let rect_for_line = |line: usize| -> Option<Rect> {
        if line >= content_offset && line < content_offset + visible {
            let row_y = rect.y + (line - content_offset) as u16;
            Some(Rect::new(rect.x, row_y, rect.width, 1))
        } else {
            None
        }
    };

    let selected_row = selected_line.and_then(rect_for_line);
    let rows = selectable
        .iter()
        .enumerate()
        .filter_map(|(i, &(index, line))| {
            // Block extent: this row's start through the next row's start (the
            // last row runs to the end of the content).
            let end = selectable
                .get(i + 1)
                .map(|&(_, next)| next)
                .unwrap_or(content_len)
                .max(line + 1);
            let start_y = line.saturating_sub(content_offset).min(visible);
            let end_y = end.saturating_sub(content_offset).min(visible);
            if start_y >= visible || end_y <= start_y {
                return None;
            }
            let top = rect.y + start_y as u16;
            let height = (end_y - start_y) as u16;
            Some((index, Rect::new(rect.x, top, rect.width, height)))
        })
        .collect();

    ScrollableRects { selected_row, rows }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_category_from_name() {
        assert_eq!(
            ConfigCategory::from_name("appearance"),
            Some(ConfigCategory::Appearance)
        );
        assert_eq!(
            ConfigCategory::from_name("THEME"),
            Some(ConfigCategory::Appearance)
        );
        assert_eq!(
            ConfigCategory::from_name("0"),
            Some(ConfigCategory::Appearance)
        );

        assert_eq!(
            ConfigCategory::from_name("components"),
            Some(ConfigCategory::Components)
        );
        assert_eq!(
            ConfigCategory::from_name("interactive"),
            Some(ConfigCategory::Components)
        );
        assert_eq!(
            ConfigCategory::from_name("1"),
            Some(ConfigCategory::Components)
        );

        assert_eq!(
            ConfigCategory::from_name("search"),
            Some(ConfigCategory::WebSearch)
        );
        assert_eq!(
            ConfigCategory::from_name("websearch"),
            Some(ConfigCategory::WebSearch)
        );
        assert_eq!(
            ConfigCategory::from_name("2"),
            Some(ConfigCategory::WebSearch)
        );

        assert_eq!(
            ConfigCategory::from_name("web"),
            Some(ConfigCategory::WebReader)
        );
        assert_eq!(
            ConfigCategory::from_name("reader"),
            Some(ConfigCategory::WebReader)
        );
        assert_eq!(
            ConfigCategory::from_name("webreader"),
            Some(ConfigCategory::WebReader)
        );
        assert_eq!(
            ConfigCategory::from_name("3"),
            Some(ConfigCategory::WebReader)
        );

        assert_eq!(
            ConfigCategory::from_name("system"),
            Some(ConfigCategory::System)
        );
        assert_eq!(
            ConfigCategory::from_name("info"),
            Some(ConfigCategory::System)
        );
        assert_eq!(
            ConfigCategory::from_name("about"),
            Some(ConfigCategory::System)
        );
        assert_eq!(ConfigCategory::from_name("4"), Some(ConfigCategory::System));

        assert_eq!(ConfigCategory::from_name("invalid"), None);
    }

    #[test]
    fn test_config_category_detail_item_count() {
        let direct = nuotc::TerminalProfile::direct_color();
        assert!(ConfigCategory::Appearance.detail_item_count(None, None, &direct) >= 5);
        // The Components pane is derived from the declared tool registry plus
        // the reasoning / density / auto-scroll behaviour rows (ADR-0020), so
        // it must track `item_count()` rather than a frozen literal.
        assert_eq!(
            ConfigCategory::Components.detail_item_count(None, None, &direct),
            components::item_count()
        );
        assert_eq!(
            components::item_count(),
            crate::tools::TOOL_COMPONENTS.len() + 2,
            "one row per declared component, plus reasoning, auto-scroll"
        );
        assert_eq!(ConfigCategory::System.detail_item_count(None, None, &direct), 5);

        let mono = nuotc::TerminalProfile::dec_vt100_monochrome();
        assert_eq!(ConfigCategory::Appearance.detail_item_count(None, None, &mono), 1);
    }

    #[test]
    fn test_config_category_slug_and_display() {
        for cat in ConfigCategory::ALL {
            let slug = cat.slug();
            assert_eq!(ConfigCategory::from_name(slug), Some(cat));
            assert_eq!(format!("{cat}"), slug);
            let parsed: ConfigCategory = slug.parse().unwrap();
            assert_eq!(parsed, cat);
            assert!(!cat.title().is_empty());
        }
    }
}
