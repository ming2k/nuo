//! Modular Settings View (`/settings`): first-class, full-screen configuration center (ADR-0141).
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
    Alignment, Block as RtBlock, Clear, Constraint, Direction, Frame, Layout, Line, Modifier,
    Paragraph, Rect, Span, Style, Wrap,
};

use crate::primitives::{ElevationContainer, SCROLL_EDGE_MARGIN, draw_scrollbar, resolve_scroll};
use crate::render::Theme;
use crate::view_header::{
    ViewHeader, ViewHints, ViewKind, draw_view_header, draw_view_header_hints,
};

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

    pub fn subtitle(self) -> &'static str {
        self.description()
    }

    /// Concise, refined one-line summary for the category.
    pub fn description(self) -> &'static str {
        match self {
            ConfigCategory::Appearance => "Theme selection and color palette customization.",
            ConfigCategory::Components => {
                "Interactive component styles, default disclosure states, and expansion behaviors."
            }
            ConfigCategory::WebSearch => {
                "Choose how the agent discovers relevant pages and sources."
            }
            ConfigCategory::WebReader => {
                "Choose how the agent reads and extracts content from a URL."
            }
            ConfigCategory::System => {
                "Configuration file paths, runtime diagnostics, and system info."
            }
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
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct ConfigRects {
    pub area: Rect,
    pub category_body: Rect,
    pub detail_body: Rect,
    pub selected_row_rect: Option<Rect>,
}

/// Properties passed to render the complete Settings View.
pub struct SettingsProps<'a> {
    pub category_index: usize,
    pub detail_index: usize,
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

    // 4 vertical zones: Top Header (1 row), Breadcrumbs Subhead (1 row), Center Body (flexible), Bottom Footer (3 rows).
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(3),
        ])
        .split(area);

    let header_rect = vertical_chunks[0];
    let subhead_rect = vertical_chunks[1];
    let body_rect = vertical_chunks[2];
    let footer_rect = vertical_chunks[3];

    let category = ConfigCategory::from_index(props.category_index);

    // 1. Top Header Row (Settings only)
    let header = ViewHeader::Settings;
    draw_view_header(frame, header_rect, &header, props.theme);

    // 2. View Stack Breadcrumbs & Affordance
    let view_hints = ViewHints {
        kind: ViewKind::Settings,
        asides: None,
        breadcrumbs: props.breadcrumbs,
    };
    draw_view_header_hints(frame, subhead_rect, &view_hints, props.theme);

    // 3. Center Body (Inset by 2 columns horizontally and 1 row vertically)
    let inner_body = Rect {
        x: body_rect.x.saturating_add(2),
        y: body_rect.y.saturating_add(1),
        width: body_rect.width.saturating_sub(4),
        height: body_rect.height.saturating_sub(2),
    };

    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(22), Constraint::Min(20)])
        .split(inner_body);

    let category_rect = body_chunks[0];
    let detail_rect = body_chunks[1];

    // Left pane contrasting surface (panel tone, distinct from view surface)
    let category_inner = ElevationContainer::panel().render(frame, category_rect, props.theme);

    // Right pane main canvas body (body tone, distinct from view surface and left nav)
    let _ = ElevationContainer::card().render(frame, detail_rect, props.theme);

    // Left pane nav: top/bottom 1 row, left/right 2 cols
    draw_categories_pane(frame, category_inner, &mut props);

    // Right pane: split into Head (1 row, indented 1 col), 1 row gap, Detail Content (indented 1 col)
    let detail_vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Head row 1 row
            Constraint::Length(1), // Gap 1 row below head
            Constraint::Min(1),    // Content below
        ])
        .split(detail_rect);

    let head_row = detail_vertical_chunks[0];
    let content_row = detail_vertical_chunks[2];

    let head_inner_rect = Rect {
        x: head_row.x.saturating_add(1),
        y: head_row.y,
        width: head_row.width.saturating_sub(2),
        height: head_row.height,
    };

    let desc = category.description();
    let truncated_desc = truncate_ellipsis(desc, head_inner_rect.width as usize);
    let head_para = Paragraph::new(Line::from(Span::styled(
        truncated_desc,
        Style::default().fg(props.theme.muted()),
    )))
    .style(Style::default().bg(props.theme.body()));
    frame.render_widget(head_para, head_inner_rect);

    let detail_inner_rect = Rect {
        x: content_row.x.saturating_add(1),
        y: content_row.y,
        width: content_row.width.saturating_sub(2),
        height: content_row.height,
    };

    let focused = props.focus == ConfigFocus::Detail;
    let selected_row_rect = match category {
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

    // 4. Bottom Footer (3-Row Subagent-Style with raised background, centered flexible equal division)
    draw_footer(frame, footer_rect, props.focus, props.theme);

    ConfigRects {
        area,
        category_body: category_rect,
        detail_body: detail_rect,
        selected_row_rect,
    }
}

fn truncate_ellipsis(text: &str, max_width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width <= 3 {
        return "...".chars().take(max_width).collect();
    }
    let target_width = max_width - 3;
    let mut current_width = 0;
    let mut result = String::new();
    for c in text.chars() {
        let cw = c.width().unwrap_or(0);
        if current_width + cw > target_width {
            break;
        }
        current_width += cw;
        result.push(c);
    }
    result.push_str("...");
    result
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

    let inner_area = Rect {
        x: area.x.saturating_add(1),
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };

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

    if max_scroll > 0 {
        draw_scrollbar(frame, area, content_offset, max_scroll, props.theme);
    }
}

fn draw_footer(frame: &mut Frame, rect: Rect, focus: ConfigFocus, theme: &Theme) {
    if rect.height == 0 {
        return;
    }

    let bg = theme.raised();
    let fill = Style::default().bg(bg);
    frame.render_widget(RtBlock::default().style(fill), rect);

    use crate::components::keycap::KeyAffordance;
    use crate::keymap::{Key, keyvocab};

    let pairs: Vec<KeyAffordance> = match focus {
        ConfigFocus::Categories => vec![
            KeyAffordance::from_glyph(keyvocab::ARROWS_UD, "select"),
            KeyAffordance::from_key(Key::ENTER, "enter panel"),
            KeyAffordance::from_key(Key::ESC, "close"),
        ],
        ConfigFocus::Detail => vec![
            KeyAffordance::from_glyph(keyvocab::ARROWS_UD, "navigate"),
            KeyAffordance::from_glyph("Enter/Space", "apply/toggle"),
            KeyAffordance::from_key(Key::ESC, "back to nav"),
        ],
    };

    let row_rect = Rect {
        x: rect.x,
        y: rect.y + 1,
        width: rect.width,
        height: 1,
    };

    let n = pairs.len();
    if n == 0 || row_rect.width == 0 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Min(0); n])
        .split(row_rect);

    for (i, affordance) in pairs.iter().enumerate() {
        let [key_span, label_span] = affordance.render_spans(theme, bg);
        let p = Paragraph::new(Line::from(vec![key_span, label_span]))
            .alignment(Alignment::Center)
            .style(fill);
        frame.render_widget(p, chunks[i]);
    }
}

pub(super) fn render_scrollable(
    frame: &mut Frame,
    rect: Rect,
    lines: Vec<Line<'static>>,
    scroll: &mut usize,
    selected_line: Option<usize>,
    theme: &Theme,
) -> Option<Rect> {
    let visible_rows = rect.height as usize;
    let content_len = lines.len();

    let (content_offset, max_scroll) = resolve_scroll(
        scroll,
        visible_rows,
        content_len,
        selected_line,
        SCROLL_EDGE_MARGIN,
    );

    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll(content_offset as u16, 0)
        .style(Style::default().bg(theme.body()));
    frame.render_widget(p, rect);

    if max_scroll > 0 {
        draw_scrollbar(frame, rect, content_offset, max_scroll, theme);
    }

    selected_line.and_then(|sel| {
        if sel >= content_offset && sel < content_offset + visible_rows {
            let row_y = rect.y + (sel - content_offset) as u16;
            Some(Rect::new(rect.x, row_y, rect.width, 1))
        } else {
            None
        }
    })
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
            crate::tools::TOOL_COMPONENTS.len() + 3,
            "one row per declared component, plus reasoning, density, auto-scroll"
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
            assert!(!cat.description().is_empty());
        }
    }

    #[test]
    fn test_truncate_ellipsis() {
        assert_eq!(truncate_ellipsis("short", 10), "short");
        assert_eq!(truncate_ellipsis("exact", 5), "exact");
        assert_eq!(truncate_ellipsis("longer text here", 10), "longer ...");
        assert_eq!(truncate_ellipsis("hello", 3), "...");
        assert_eq!(truncate_ellipsis("hello", 2), "..");
        assert_eq!(truncate_ellipsis("hello", 0), "");
    }
}
