//! Universal Reusable Floating Dropdown / Popover Component for TUI (Scheme A).
//!
//! Provides a zero-compromise, fully generic, anchor-aware floating popover overlay.
//! Supports arbitrary payload types `T`, keyboard navigation, fuzzy query filtering,
//! adaptive auto-flip geometry (Above/Below/CenterScreen), status indicators,
//! badges, shortcuts, multi-line descriptions, and scroll indicators.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nuotc::anchor::{
    AnchorAlignment, AnchorConstraints, AnchorPlacement, compute_anchored_rect,
};
use nuotc::{
    Alignment, Block as RtBlock, BorderType, Borders, Clear, Frame, Line, Modifier, Paragraph,
    Rect, Span, Style,
};

use crate::render::Theme;

/// Visual status indicator for a dropdown entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropdownIndicator {
    /// Green/Active indicator (●)
    Ready,
    /// Warning/Pending indicator (▲)
    Warning,
    /// Inactive/Disabled indicator (○)
    Inactive,
    /// Custom character indicator
    Custom(char),
}

/// A single selectable entry in a dropdown, carrying typed payload `T`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropdownItem<T> {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub badge: Option<String>,
    pub indicator: Option<DropdownIndicator>,
    pub shortcut: Option<String>,
    pub disabled: bool,
    pub payload: T,
}

impl<T> DropdownItem<T> {
    /// Create a new basic dropdown item with payload.
    pub fn new(id: impl Into<String>, title: impl Into<String>, payload: T) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: None,
            badge: None,
            indicator: None,
            shortcut: None,
            disabled: false,
            payload,
        }
    }

    /// Attach a secondary description line.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Attach a tag/badge (e.g. `"[default]"`, `"[active]"`).
    pub fn with_badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    /// Attach a status indicator dot.
    pub fn with_indicator(mut self, indicator: DropdownIndicator) -> Self {
        self.indicator = Some(indicator);
        self
    }

    /// Attach a shortcut hint (e.g. "⌥1", "^R").
    pub fn with_shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Set whether the item is disabled.
    pub fn with_disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// Placement rule for positioning the dropdown relative to its anchor target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DropdownPlacement {
    /// Automatically flip Above or Below based on available screen space.
    #[default]
    Auto,
    /// Force popup above the anchor target.
    Above,
    /// Force popup below the anchor target.
    Below,
    /// Center the popup in the middle of the available screen area.
    CenterScreen,
}

impl From<DropdownPlacement> for AnchorPlacement {
    fn from(p: DropdownPlacement) -> Self {
        match p {
            DropdownPlacement::Auto => AnchorPlacement::AutoVertical,
            DropdownPlacement::Above => AnchorPlacement::Top,
            DropdownPlacement::Below => AnchorPlacement::Bottom,
            DropdownPlacement::CenterScreen => AnchorPlacement::CenterViewport,
        }
    }
}

/// Geometry and anchoring configuration for the dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropdownAnchor {
    pub target_rect: Rect,
    pub placement: DropdownPlacement,
    pub min_width: u16,
    pub max_width: u16,
    pub max_height: u16,
}

impl DropdownAnchor {
    /// Create an anchor configuration relative to a target rectangle.
    pub fn anchored(target_rect: Rect, placement: DropdownPlacement) -> Self {
        Self {
            target_rect,
            placement,
            min_width: 36,
            max_width: 80,
            max_height: 20,
        }
    }

    /// Create a center-screen anchor configuration.
    pub fn center_screen() -> Self {
        Self {
            target_rect: Rect::default(),
            placement: DropdownPlacement::CenterScreen,
            min_width: 44,
            max_width: 84,
            max_height: 22,
        }
    }

    pub fn with_width_bounds(mut self, min: u16, max: u16) -> Self {
        self.min_width = min;
        self.max_width = max;
        self
    }

    pub fn with_max_height(mut self, max_height: u16) -> Self {
        self.max_height = max_height;
        self
    }
}

/// The result of processing a key event on the dropdown state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropdownEventOutcome<T> {
    /// The event was not applicable to the dropdown.
    Ignored,
    /// The event was handled internally (e.g. selection moved, filter updated).
    Handled,
    /// The user confirmed the selection.
    Confirmed(T),
    /// The user cancelled/dismissed the dropdown.
    Cancelled,
}

/// Comprehensive state controller for the dropdown.
#[derive(Debug, Clone)]
pub struct DropdownState<T> {
    pub title: Option<String>,
    pub items: Vec<DropdownItem<T>>,
    pub filtered_indices: Vec<usize>,
    pub selected_idx: usize,
    pub scroll_top: usize,
    pub query: String,
    pub context: Option<String>,
    pub filterable: bool,
}

impl<T> DropdownState<T> {
    /// Create a new dropdown state from an item collection.
    pub fn new(title: Option<impl Into<String>>, items: Vec<DropdownItem<T>>) -> Self {
        let count = items.len();
        let filtered_indices: Vec<usize> = (0..count).collect();
        let mut state = Self {
            title: title.map(Into::into),
            items,
            filtered_indices,
            selected_idx: 0,
            scroll_top: 0,
            query: String::new(),
            context: None,
            filterable: true,
        };
        state.ensure_valid_selection();
        state
    }

    /// Select item by ID if it exists.
    pub fn select_by_id(&mut self, id: &str) -> bool {
        if let Some(pos) = self
            .filtered_indices
            .iter()
            .position(|&idx| self.items.get(idx).map(|it| it.id.as_str()) == Some(id))
        {
            self.selected_idx = pos;
            self.ensure_visible();
            true
        } else {
            false
        }
    }

    /// Set an optional context identifier string.
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Enable or disable interactive text filtering.
    pub fn with_filterable(mut self, filterable: bool) -> Self {
        self.filterable = filterable;
        self
    }

    /// Total number of currently visible (filtered) items.
    pub fn visible_count(&self) -> usize {
        self.filtered_indices.len()
    }

    /// Get currently selected item reference.
    pub fn selected_item(&self) -> Option<&DropdownItem<T>> {
        let raw_idx = *self.filtered_indices.get(self.selected_idx)?;
        self.items.get(raw_idx)
    }

    /// Get currently selected payload reference.
    pub fn selected_payload(&self) -> Option<&T> {
        self.selected_item().map(|it| &it.payload)
    }

    /// Move selection to previous item, wrapping around.
    pub fn select_prev(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        let len = self.filtered_indices.len();
        let mut next = if self.selected_idx == 0 {
            len.saturating_sub(1)
        } else {
            self.selected_idx - 1
        };

        // Skip disabled items if possible
        for _ in 0..len {
            if let Some(&raw_idx) = self.filtered_indices.get(next)
                && let Some(it) = self.items.get(raw_idx)
                && !it.disabled
            {
                self.selected_idx = next;
                self.ensure_visible();
                return;
            }
            next = if next == 0 {
                len.saturating_sub(1)
            } else {
                next - 1
            };
        }
        self.selected_idx = next;
        self.ensure_visible();
    }

    /// Move selection to next item, wrapping around.
    pub fn select_next(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        let len = self.filtered_indices.len();
        let mut next = if self.selected_idx + 1 >= len {
            0
        } else {
            self.selected_idx + 1
        };

        // Skip disabled items if possible
        for _ in 0..len {
            if let Some(&raw_idx) = self.filtered_indices.get(next)
                && let Some(it) = self.items.get(raw_idx)
                && !it.disabled
            {
                self.selected_idx = next;
                self.ensure_visible();
                return;
            }
            next = if next + 1 >= len { 0 } else { next + 1 };
        }
        self.selected_idx = next;
        self.ensure_visible();
    }

    /// Jump selection to first item.
    pub fn select_first(&mut self) {
        if !self.filtered_indices.is_empty() {
            self.selected_idx = 0;
            self.ensure_visible();
        }
    }

    /// Jump selection to last item.
    pub fn select_last(&mut self) {
        if !self.filtered_indices.is_empty() {
            self.selected_idx = self.filtered_indices.len().saturating_sub(1);
            self.ensure_visible();
        }
    }

    /// Page up by step count.
    pub fn page_up(&mut self, step: usize) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.selected_idx = self.selected_idx.saturating_sub(step);
        self.ensure_visible();
    }

    /// Page down by step count.
    pub fn page_down(&mut self, step: usize) {
        if self.filtered_indices.is_empty() {
            return;
        }
        let max_idx = self.filtered_indices.len().saturating_sub(1);
        self.selected_idx = (self.selected_idx + step).min(max_idx);
        self.ensure_visible();
    }

    /// Update the search filter query and recompute filtered items.
    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_string();
        self.recompute_filter();
    }

    /// Clear the search filter query.
    pub fn clear_query(&mut self) {
        self.query.clear();
        self.recompute_filter();
    }

    fn recompute_filter(&mut self) {
        let q = self.query.trim().to_lowercase();
        if q.is_empty() {
            self.filtered_indices = (0..self.items.len()).collect();
        } else {
            self.filtered_indices = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    item.title.to_lowercase().contains(&q)
                        || item
                            .description
                            .as_ref()
                            .map(|d| d.to_lowercase().contains(&q))
                            .unwrap_or(false)
                        || item
                            .badge
                            .as_ref()
                            .map(|b| b.to_lowercase().contains(&q))
                            .unwrap_or(false)
                })
                .map(|(idx, _)| idx)
                .collect();
        }
        self.ensure_valid_selection();
    }

    fn ensure_valid_selection(&mut self) {
        if self.filtered_indices.is_empty() {
            self.selected_idx = 0;
            self.scroll_top = 0;
            return;
        }
        if self.selected_idx >= self.filtered_indices.len() {
            self.selected_idx = self.filtered_indices.len().saturating_sub(1);
        }
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        if self.selected_idx < self.scroll_top {
            self.scroll_top = self.selected_idx;
        }
    }

    /// Adjust scroll position given the max displayable item count.
    pub fn sync_viewport(&mut self, visible_capacity: usize) {
        if visible_capacity == 0 || self.filtered_indices.is_empty() {
            return;
        }
        if self.selected_idx < self.scroll_top {
            self.scroll_top = self.selected_idx;
        } else if self.selected_idx >= self.scroll_top + visible_capacity {
            self.scroll_top = self.selected_idx + 1 - visible_capacity;
        }
    }
}

impl<T: Clone> DropdownState<T> {
    /// Process a keyboard event against the dropdown.
    pub fn handle_key(&mut self, key: KeyEvent) -> DropdownEventOutcome<T> {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
                self.select_prev();
                DropdownEventOutcome::Handled
            }
            (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
                self.select_next();
                DropdownEventOutcome::Handled
            }
            (KeyModifiers::NONE, KeyCode::PageUp) => {
                self.page_up(5);
                DropdownEventOutcome::Handled
            }
            (KeyModifiers::NONE, KeyCode::PageDown) => {
                self.page_down(5);
                DropdownEventOutcome::Handled
            }
            (KeyModifiers::NONE, KeyCode::Home) => {
                self.select_first();
                DropdownEventOutcome::Handled
            }
            (KeyModifiers::NONE, KeyCode::End) => {
                self.select_last();
                DropdownEventOutcome::Handled
            }
            (KeyModifiers::NONE, KeyCode::Enter) | (KeyModifiers::NONE, KeyCode::Tab) => {
                if let Some(item) = self.selected_item()
                    && !item.disabled
                {
                    return DropdownEventOutcome::Confirmed(item.payload.clone());
                }
                DropdownEventOutcome::Handled
            }
            (KeyModifiers::NONE, KeyCode::Esc) => DropdownEventOutcome::Cancelled,
            (KeyModifiers::NONE, KeyCode::Backspace) if self.filterable => {
                if !self.query.is_empty() {
                    let mut q = self.query.clone();
                    q.pop();
                    self.set_query(&q);
                    DropdownEventOutcome::Handled
                } else {
                    DropdownEventOutcome::Ignored
                }
            }
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c))
                if self.filterable && !c.is_control() =>
            {
                let mut q = self.query.clone();
                q.push(c);
                self.set_query(&q);
                DropdownEventOutcome::Handled
            }
            _ => DropdownEventOutcome::Ignored,
        }
    }
}

/// Compute the exact target rectangle for rendering the dropdown popup overlay.
pub fn compute_dropdown_rect(
    anchor: &DropdownAnchor,
    screen_area: Rect,
    item_count: usize,
    has_descriptions: bool,
) -> Rect {
    if screen_area.width == 0 || screen_area.height == 0 {
        return Rect::default();
    }

    let rows_per_item: u16 = if has_descriptions { 2 } else { 1 };
    // Content rows + border (2) + footer hint (1) + title breathing room (1)
    let content_height = (item_count as u16 * rows_per_item) + 4;
    let max_avail_height = anchor
        .max_height
        .min(screen_area.height.saturating_sub(2))
        .max(5);
    let height = content_height.min(max_avail_height).max(5);

    let max_avail_width = anchor
        .max_width
        .min(screen_area.width.saturating_sub(4))
        .max(20);
    let width = anchor
        .min_width
        .max(anchor.target_rect.width)
        .min(max_avail_width);

    let constraints = AnchorConstraints::new()
        .with_width_bounds(width, max_avail_width)
        .with_height_bounds(height, max_avail_height);

    compute_anchored_rect(
        anchor.target_rect,
        screen_area,
        (width, height),
        anchor.placement.into(),
        AnchorAlignment::Start,
        &constraints,
    )
}

/// Render the floating dropdown popup overlay on top of the frame.
pub fn draw_dropdown<T>(
    f: &mut Frame<'_>,
    state: &mut DropdownState<T>,
    anchor: &DropdownAnchor,
    theme: &Theme,
    screen_area: Rect,
) -> Rect {
    if screen_area.width < 10 || screen_area.height < 5 {
        return Rect::default();
    }

    let has_descriptions = state.items.iter().any(|it| it.description.is_some());
    let popup_area = compute_dropdown_rect(
        anchor,
        screen_area,
        state.visible_count().max(1),
        has_descriptions,
    );

    if popup_area.width == 0 || popup_area.height == 0 {
        return Rect::default();
    }

    // 1. Clear background under popup to avoid bleed-through
    f.render_widget(Clear, popup_area);

    // 2. Compute inner capacity for items
    let inner_height = popup_area.height.saturating_sub(3); // top border + bottom border + footer
    let rows_per_item: usize = if has_descriptions { 2 } else { 1 };
    let visible_capacity = (inner_height as usize) / rows_per_item;
    state.sync_viewport(visible_capacity.max(1));

    // 3. Build card block
    let block = RtBlock::default()
        .borders(Borders::LEFT | Borders::RIGHT | Borders::TOP | Borders::BOTTOM)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(theme.brand()))
        .style(Style::default().bg(theme.panel()));

    // 4. Render block chrome
    f.render_widget(block, popup_area);

    let inner_x = popup_area.x + 2;
    let inner_y = popup_area.y + 1;
    let inner_width = popup_area.width.saturating_sub(4);

    // 5. Title line
    let mut title_spans = vec![Span::styled("▼ ", Style::default().fg(theme.brand()))];

    if let Some(title) = &state.title {
        title_spans.push(Span::styled(
            title,
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
        ));
    }

    if !state.query.is_empty() {
        title_spans.push(Span::styled(
            format!(" [/{}]", state.query),
            Style::default().fg(theme.brand()),
        ));
    }

    // Render title
    let title_rect = Rect::new(inner_x, inner_y, inner_width, 1);
    f.render_widget(Paragraph::new(Line::from(title_spans)), title_rect);

    // 6. Render items
    let mut lines: Vec<Line<'_>> = Vec::new();

    if state.filtered_indices.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No matching items",
            Style::default().fg(theme.dim()),
        )]));
    } else {
        let start = state.scroll_top;
        let end = (start + visible_capacity).min(state.filtered_indices.len());

        for (rel_idx, &raw_idx) in state.filtered_indices[start..end].iter().enumerate() {
            let abs_filtered_idx = start + rel_idx;
            let is_selected = abs_filtered_idx == state.selected_idx;
            let item = &state.items[raw_idx];

            let cursor_span = if is_selected {
                Span::styled(
                    "› ",
                    Style::default()
                        .fg(theme.brand())
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("  ")
            };

            let indicator_span = match item.indicator {
                Some(DropdownIndicator::Ready) => {
                    Span::styled("● ", Style::default().fg(theme.ok()))
                }
                Some(DropdownIndicator::Warning) => {
                    Span::styled("▲ ", Style::default().fg(theme.warn()))
                }
                Some(DropdownIndicator::Inactive) => {
                    Span::styled("○ ", Style::default().fg(theme.err()))
                }
                Some(DropdownIndicator::Custom(c)) => {
                    Span::styled(format!("{c} "), Style::default().fg(theme.brand()))
                }
                None => Span::raw(""),
            };

            let title_style = if item.disabled {
                Style::default()
                    .fg(theme.dim())
                    .add_modifier(Modifier::STRIKETHROUGH)
            } else if is_selected {
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg())
            };

            let mut header_spans = vec![
                cursor_span,
                indicator_span,
                Span::styled(&item.title, title_style),
            ];

            if let Some(badge) = &item.badge {
                let badge_color = if item.disabled {
                    theme.dim()
                } else if is_selected {
                    theme.brand()
                } else {
                    theme.dim()
                };
                header_spans.push(Span::raw(" "));
                header_spans.push(Span::styled(
                    format!("[{badge}]"),
                    Style::default().fg(badge_color),
                ));
            }

            if let Some(sc) = &item.shortcut {
                header_spans.push(Span::raw(" "));
                header_spans.push(Span::styled(
                    format!("({sc})"),
                    Style::default().fg(theme.dim()),
                ));
            }

            lines.push(Line::from(header_spans));

            if has_descriptions {
                let desc_style = if is_selected {
                    Style::default().fg(theme.muted())
                } else {
                    Style::default().fg(theme.dim())
                };
                let desc_text = item.description.as_deref().unwrap_or("");
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(desc_text, desc_style),
                ]));
            }
        }
    }

    let items_rect = Rect::new(
        inner_x,
        inner_y + 1,
        inner_width,
        inner_height.saturating_sub(1).min(lines.len() as u16),
    );
    f.render_widget(Paragraph::new(lines), items_rect);

    // 7. Check scroll indicator flags
    let has_more_above = state.scroll_top > 0;
    let has_more_below = state.scroll_top + visible_capacity < state.visible_count();

    if has_more_above && popup_area.width > 6 {
        let up_rect = Rect::new(popup_area.x + popup_area.width - 4, popup_area.y, 2, 1);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "▲",
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ))),
            up_rect,
        );
    }
    if has_more_below && popup_area.width > 6 {
        let dn_rect = Rect::new(
            popup_area.x + popup_area.width - 4,
            popup_area.y + popup_area.height - 1,
            2,
            1,
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "▼",
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ))),
            dn_rect,
        );
    }

    // 8. Render footer key hints
    let footer_rect = Rect::new(
        popup_area.x + 2,
        popup_area.y + popup_area.height - 2,
        popup_area.width.saturating_sub(4),
        1,
    );

    let footer_line = Line::from(vec![
        Span::styled(
            "↑/↓",
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" Select  ", Style::default().fg(theme.dim())),
        Span::styled(
            "↵",
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" Confirm  ", Style::default().fg(theme.dim())),
        Span::styled(
            "Esc",
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" Cancel", Style::default().fg(theme.dim())),
    ]);

    f.render_widget(
        Paragraph::new(footer_line).alignment(Alignment::Right),
        footer_rect,
    );

    popup_area
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;
    use nuotc::TestTerminal;

    #[test]
    fn test_dropdown_state_navigation() {
        let items = vec![
            DropdownItem::new("1", "Item 1", 10),
            DropdownItem::new("2", "Item 2", 20),
            DropdownItem::new("3", "Item 3", 30),
        ];

        let mut state = DropdownState::new(Some("Test"), items);
        assert_eq!(state.selected_idx, 0);
        assert_eq!(state.selected_payload(), Some(&10));

        state.select_next();
        assert_eq!(state.selected_idx, 1);
        assert_eq!(state.selected_payload(), Some(&20));

        state.select_next();
        assert_eq!(state.selected_idx, 2);
        assert_eq!(state.selected_payload(), Some(&30));

        // Wrap around
        state.select_next();
        assert_eq!(state.selected_idx, 0);

        // Previous
        state.select_prev();
        assert_eq!(state.selected_idx, 2);
    }

    #[test]
    fn test_dropdown_state_skips_disabled() {
        let items = vec![
            DropdownItem::new("1", "Item 1", 10),
            DropdownItem::new("2", "Item 2", 20).with_disabled(true),
            DropdownItem::new("3", "Item 3", 30),
        ];

        let mut state = DropdownState::new(Some("Test"), items);
        assert_eq!(state.selected_idx, 0);

        state.select_next();
        assert_eq!(state.selected_idx, 2); // Skipped 1

        state.select_prev();
        assert_eq!(state.selected_idx, 0); // Skipped 1
    }

    #[test]
    fn test_dropdown_filtering() {
        let items = vec![
            DropdownItem::new("gpt4", "GPT-4o", "openai").with_description("Smartest model"),
            DropdownItem::new("claude", "Claude 3.5 Sonnet", "anthropic")
                .with_description("Great for coding"),
            DropdownItem::new("llama", "Llama 3.3", "meta").with_description("Open weights"),
        ];

        let mut state = DropdownState::new(Some("Models"), items);
        assert_eq!(state.visible_count(), 3);

        state.set_query("coding");
        assert_eq!(state.visible_count(), 1);
        assert_eq!(state.selected_item().unwrap().id, "claude");

        state.clear_query();
        assert_eq!(state.visible_count(), 3);
    }

    #[test]
    fn test_dropdown_key_handling() {
        let items = vec![
            DropdownItem::new("a", "Option A", "val_a"),
            DropdownItem::new("b", "Option B", "val_b"),
        ];

        let mut state = DropdownState::new(Some("Select"), items);

        // Down arrow
        let down_key = KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        let outcome = state.handle_key(down_key);
        assert_eq!(outcome, DropdownEventOutcome::Handled);
        assert_eq!(state.selected_idx, 1);

        // Enter
        let enter_key = KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        let outcome = state.handle_key(enter_key);
        assert_eq!(outcome, DropdownEventOutcome::Confirmed("val_b"));

        // Esc
        let esc_key = KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        let outcome = state.handle_key(esc_key);
        assert_eq!(outcome, DropdownEventOutcome::Cancelled);
    }

    #[test]
    fn test_compute_dropdown_rect_placements() {
        let screen = Rect::new(0, 0, 100, 40);
        let target = Rect::new(20, 30, 40, 3);
        let anchor_auto = DropdownAnchor::anchored(target, DropdownPlacement::Auto);
        let rect = compute_dropdown_rect(&anchor_auto, screen, 5, false);

        // Auto flips Above because below only has 40 - 33 = 7 rows while above has 30 rows
        assert!(rect.y < target.y);

        let anchor_center = DropdownAnchor::center_screen();
        let center_rect = compute_dropdown_rect(&anchor_center, screen, 4, false);
        assert_eq!(center_rect.x, (100 - center_rect.width) / 2);
        assert_eq!(center_rect.y, (40 - center_rect.height) / 2);
    }

    #[test]
    fn test_draw_dropdown_renders_cleanly() {
        let mut terminal = TestTerminal::new(80, 24);
        let theme = Theme::default();

        let items = vec![
            DropdownItem::new("1", "Provider 1", 1)
                .with_badge("active")
                .with_indicator(DropdownIndicator::Ready)
                .with_description("Primary provider"),
            DropdownItem::new("2", "Provider 2", 2)
                .with_shortcut("⌥2")
                .with_indicator(DropdownIndicator::Inactive)
                .with_description("Secondary fallback"),
        ];

        let mut state = DropdownState::new(Some("Select Provider"), items);
        let anchor = DropdownAnchor::center_screen();

        terminal.draw(|f| {
            let screen_area = f.area();
            draw_dropdown(f, &mut state, &anchor, &theme, screen_area);
        });

        let rendered_text: String = terminal
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();

        assert!(rendered_text.contains("Select Provider"));
        assert!(rendered_text.contains("Provider 1"));
        assert!(rendered_text.contains("[active]"));
        assert!(rendered_text.contains("Primary provider"));
        assert!(rendered_text.contains("Select"));
        assert!(rendered_text.contains("Confirm"));
    }
}
