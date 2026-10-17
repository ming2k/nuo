//! Interactive Offline Storybook Workbench (ADR-0073).
//!
//! A zero-dependency, hermetic full-screen terminal workbench for
//! rapid visual inspection, viewport emulation, live theme switching, and interaction testing.

use std::io::{self, Stdout};
use std::path::Path;
use crossterm::event::{self, Event, KeyCode};
use nuotc::{
    Borders, Frame, Line, Modifier, Paragraph, Rect, Span, Style, Terminal,
    widgets::Block,
};

use crate::theme::Theme;
use super::ask_user::AskUser;
use super::notice_card::NoticeCard;
use super::story::{AnyStory, StorybookComponent, instantiate_story};
use super::testing::render_story_styled_snapshot;
use super::tool_card::ToolCard;

/// Viewport emulation bounding box presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportMode {
    Full,
    Size80x24,
    Size100x30,
    Size120x40,
}

impl ViewportMode {
    pub fn next(self) -> Self {
        match self {
            Self::Full => Self::Size80x24,
            Self::Size80x24 => Self::Size100x30,
            Self::Size100x30 => Self::Size120x40,
            Self::Size120x40 => Self::Full,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::Size80x24 => "80x24",
            Self::Size100x30 => "100x30",
            Self::Size120x40 => "120x40",
        }
    }

    pub fn clamp_rect(self, available: Rect) -> Rect {
        let (max_w, max_h) = match self {
            Self::Full => (available.width, available.height),
            Self::Size80x24 => (80, 24),
            Self::Size100x30 => (100, 30),
            Self::Size120x40 => (120, 40),
        };

        let target_w = max_w.min(available.width.saturating_sub(2));
        let target_h = max_h.min(available.height.saturating_sub(2));

        let x = available.x.saturating_add((available.width.saturating_sub(target_w)) / 2);
        let y = available.y.saturating_add((available.height.saturating_sub(target_h)) / 2);

        Rect::new(x, y, target_w, target_h)
    }
}

/// Workbench input modality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkbenchMode {
    /// Navigating the storybook catalog.
    Browsing,
    /// Searching stories by query.
    Searching,
    /// Keys pass directly to the active component.
    Interacting,
}

/// Available color schemes for live cycling.
const THEME_PRESETS: &[(&str, &str)] = &[
    ("zen", "Zen (Dark Charcoal)"),
    ("paper", "Paper (Warm Light)"),
    ("catppuccin", "Catppuccin (Mocha)"),
    ("nord", "Nord (Arctic Blue)"),
    ("ansi16", "ANSI 16 (High Contrast)"),
    ("monochrome", "Monochrome (VT100)"),
];

/// The interactive Storybook state carrier.
pub struct Workbench {
    stories: Vec<Box<dyn AnyStory>>,
    filtered_indices: Vec<usize>,
    selected_pointer: usize,
    viewport_mode: ViewportMode,
    mode: WorkbenchMode,
    search_query: String,
    theme_index: usize,
    theme: Theme,
}

impl Default for Workbench {
    fn default() -> Self {
        Self::new(default_catalog())
    }
}

impl Workbench {
    pub fn new(stories: Vec<Box<dyn AnyStory>>) -> Self {
        let len = stories.len();
        let mut wb = Self {
            stories,
            filtered_indices: (0..len).collect(),
            selected_pointer: 0,
            viewport_mode: ViewportMode::Full,
            mode: WorkbenchMode::Browsing,
            search_query: String::new(),
            theme_index: 0,
            theme: Theme::default(),
        };
        wb.update_theme();
        wb
    }

    fn update_theme(&mut self) {
        let (preset_id, _) = THEME_PRESETS[self.theme_index % THEME_PRESETS.len()];
        self.theme = Theme::from_color_scheme(preset_id, &Default::default());
    }

    fn cycle_theme(&mut self) {
        self.theme_index = (self.theme_index + 1) % THEME_PRESETS.len();
        self.update_theme();
    }

    fn recompute_filter(&mut self) {
        if self.search_query.trim().is_empty() {
            self.filtered_indices = (0..self.stories.len()).collect();
        } else {
            let q = self.search_query.to_lowercase();
            self.filtered_indices = self
                .stories
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    s.component_name().to_lowercase().contains(&q)
                        || s.story_name().to_lowercase().contains(&q)
                        || s.category().to_lowercase().contains(&q)
                })
                .map(|(i, _)| i)
                .collect();
        }
        if self.selected_pointer >= self.filtered_indices.len() {
            self.selected_pointer = self.filtered_indices.len().saturating_sub(1);
        }
    }

    fn current_story_index(&self) -> Option<usize> {
        self.filtered_indices.get(self.selected_pointer).copied()
    }

    /// Run the interactive storybook loop directly on the physical terminal.
    pub fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let profile = nuotc::TerminalProfile::detect();
        crate::terminal::enter_terminal(&profile)?;
        crate::ensure_panic_hook();

        let stdout = io::stdout();
        let backend = nuotc::Backend::with_profile(stdout, profile);
        let mut terminal = Terminal::new(backend);

        let result = self.run_loop(&mut terminal);

        crate::terminal::restore_terminal();
        result
    }

    fn run_loop(&mut self, terminal: &mut Terminal<Stdout>) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            terminal.draw(|frame| {
                self.draw_ui(frame);
            })?;

            if let Event::Key(key) = event::read()? {
                if key.kind != crossterm::event::KeyEventKind::Press {
                    continue;
                }

                match self.mode {
                    WorkbenchMode::Interacting => {
                        if key.code == KeyCode::Esc {
                            self.mode = WorkbenchMode::Browsing;
                        } else if let Some(story_idx) = self.current_story_index() {
                            if let Some(story) = self.stories.get_mut(story_idx) {
                                story.handle_key(&key);
                            }
                        }
                    }
                    WorkbenchMode::Searching => {
                        match key.code {
                            KeyCode::Esc | KeyCode::Enter => {
                                self.mode = WorkbenchMode::Browsing;
                            }
                            KeyCode::Backspace => {
                                self.search_query.pop();
                                self.recompute_filter();
                            }
                            KeyCode::Char(c) => {
                                self.search_query.push(c);
                                self.recompute_filter();
                            }
                            _ => {}
                        }
                    }
                    WorkbenchMode::Browsing => {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Char('/') => {
                                self.mode = WorkbenchMode::Searching;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if !self.filtered_indices.is_empty() {
                                    self.selected_pointer =
                                        (self.selected_pointer + 1) % self.filtered_indices.len();
                                }
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if !self.filtered_indices.is_empty() {
                                    self.selected_pointer = self
                                        .selected_pointer
                                        .checked_sub(1)
                                        .unwrap_or(self.filtered_indices.len() - 1);
                                }
                            }
                            KeyCode::Char(' ') | KeyCode::Enter => {
                                if !self.filtered_indices.is_empty() {
                                    self.mode = WorkbenchMode::Interacting;
                                }
                            }
                            KeyCode::Char('v') | KeyCode::Char('V') => {
                                self.viewport_mode = self.viewport_mode.next();
                            }
                            KeyCode::Char('t') | KeyCode::Char('T') => {
                                self.cycle_theme();
                            }
                            KeyCode::Char('r') | KeyCode::Char('R') => {
                                if let Some(idx) = self.current_story_index() {
                                    if let Some(story) = self.stories.get_mut(idx) {
                                        story.reset();
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn draw_ui(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        if area.width < 40 || area.height < 10 {
            return;
        }

        let header_rect = Rect::new(area.x, area.y, area.width, 1);
        let footer_rect = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let body_rect = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(2),
        );

        // 1. Header Bar
        let mode_badge = match self.mode {
            WorkbenchMode::Browsing => Span::styled(" [BROWSING] ", Style::default().fg(self.theme.system_fg)),
            WorkbenchMode::Searching => Span::styled(" [SEARCH] ", Style::default().fg(self.theme.app_bg).bg(self.theme.system_fg).add_modifier(Modifier::BOLD)),
            WorkbenchMode::Interacting => Span::styled(
                " [INTERACTING] ",
                Style::default().fg(self.theme.app_bg).bg(self.theme.warning).add_modifier(Modifier::BOLD),
            ),
        };

        let (_, theme_label) = THEME_PRESETS[self.theme_index % THEME_PRESETS.len()];
        let header_spans = vec![
            Span::styled(" Nuo Storybook ", Style::default().fg(self.theme.text).add_modifier(Modifier::BOLD)),
            mode_badge,
            Span::raw(" | Viewport: "),
            Span::styled(self.viewport_mode.label(), Style::default().fg(self.theme.warning)),
            Span::raw(" | Theme: "),
            Span::styled(theme_label, Style::default().fg(self.theme.system_fg)),
            Span::raw(" | Stories: "),
            Span::styled(format!("{}/{}", self.selected_pointer + 1, self.filtered_indices.len()), Style::default().fg(self.theme.text)),
        ];
        frame.render_widget(Paragraph::new(Line::from(header_spans)).style(Style::default().bg(self.theme.panel_bg)), header_rect);

        // 2. Footer Bar
        let footer_text = match self.mode {
            WorkbenchMode::Browsing => " [↑/↓] Select | [/] Search | [Space] Interact | [V] Box | [T] Theme | [R] Reset | [Q] Quit",
            WorkbenchMode::Searching => " Type query to filter | [Enter/Esc] Confirm search",
            WorkbenchMode::Interacting => " [ESC] Exit Component Interaction back to Storybook Navigation",
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(footer_text, Style::default().fg(self.theme.text_muted))))
                .style(Style::default().bg(self.theme.panel_bg)),
            footer_rect,
        );

        // 3. Body: Left Catalog (width 36) + Right Canvas
        let catalog_width = 36u16.min(body_rect.width / 3).max(26);
        let catalog_rect = Rect::new(body_rect.x, body_rect.y, catalog_width, body_rect.height);
        let canvas_outer_rect = Rect::new(
            body_rect.x.saturating_add(catalog_width),
            body_rect.y,
            body_rect.width.saturating_sub(catalog_width),
            body_rect.height,
        );

        // Draw Catalog
        let catalog_block = Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(self.theme.dim_fg))
            .style(Style::default().bg(self.theme.panel_bg));
        frame.render_widget(catalog_block, catalog_rect);

        let inner_catalog = Rect::new(
            catalog_rect.x.saturating_add(1),
            catalog_rect.y,
            catalog_rect.width.saturating_sub(2),
            catalog_rect.height,
        );

        let mut catalog_lines = Vec::new();

        // Search status banner if searching or query active
        if !self.search_query.is_empty() || self.mode == WorkbenchMode::Searching {
            catalog_lines.push(Line::from(vec![
                Span::styled("Filter: ", Style::default().fg(self.theme.warning).add_modifier(Modifier::BOLD)),
                Span::styled(&self.search_query, Style::default().fg(self.theme.text).add_modifier(Modifier::UNDERLINED)),
            ]));
            catalog_lines.push(Line::raw(""));
        }

        let mut last_category = "";
        for (pos, &story_idx) in self.filtered_indices.iter().enumerate() {
            let story = &self.stories[story_idx];
            if story.category() != last_category {
                last_category = story.category();
                catalog_lines.push(Line::from(Span::styled(
                    format!("▼ {}", last_category),
                    Style::default().fg(self.theme.warning).add_modifier(Modifier::BOLD),
                )));
            }

            let is_selected = pos == self.selected_pointer;
            let prefix = if is_selected { " ▶ " } else { "   " };
            let title = format!("{}: {}", story.component_name(), story.story_name());
            let line_style = if is_selected {
                Style::default().fg(self.theme.text).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.theme.text_muted)
            };

            catalog_lines.push(Line::from(vec![
                Span::styled(prefix, if is_selected { Style::default().fg(self.theme.system_fg) } else { Style::default() }),
                Span::styled(title, line_style),
            ]));
        }

        frame.render_widget(Paragraph::new(catalog_lines), inner_catalog);

        // Draw Canvas Stage
        let target_canvas_rect = self.viewport_mode.clamp_rect(canvas_outer_rect);

        if self.viewport_mode != ViewportMode::Full {
            let stage_box = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(self.theme.system_fg));
            let inner_target = stage_box.inner(target_canvas_rect);
            frame.render_widget(stage_box, target_canvas_rect);

            if let Some(story_idx) = self.current_story_index() {
                if let Some(story) = self.stories.get(story_idx) {
                    story.render(frame, inner_target, &self.theme);
                }
            }
        } else if let Some(story_idx) = self.current_story_index() {
            if let Some(story) = self.stories.get(story_idx) {
                let padded = Rect::new(
                    canvas_outer_rect.x.saturating_add(1),
                    canvas_outer_rect.y.saturating_add(1),
                    canvas_outer_rect.width.saturating_sub(2),
                    canvas_outer_rect.height.saturating_sub(2),
                );
                story.render(frame, padded, &self.theme);
            }
        }
    }

    /// Export all canonical story snapshots to a target directory for CI artifacts.
    pub fn export_all_snapshots(dir: &Path) -> Result<usize, io::Error> {
        std::fs::create_dir_all(dir)?;
        let catalog = default_catalog();
        let mut count = 0;

        for story in &catalog {
            let filename = format!(
                "{}_{}.snapshot.txt",
                story.component_name().to_lowercase(),
                story.story_name().to_lowercase().replace(' ', "_")
            );
            let snapshot = render_story_styled_snapshot(story.as_ref(), 100, 24);
            std::fs::write(dir.join(filename), snapshot)?;
            count += 1;
        }

        Ok(count)
    }
}

/// Build the full default canonical suite of stories.
pub fn default_catalog() -> Vec<Box<dyn AnyStory>> {
    let mut catalog: Vec<Box<dyn AnyStory>> = Vec::new();

    for story in NoticeCard::stories() {
        catalog.push(instantiate_story::<NoticeCard>(story));
    }

    for story in AskUser::stories() {
        catalog.push(instantiate_story::<AskUser>(story));
    }

    for story in ToolCard::stories() {
        catalog.push(instantiate_story::<ToolCard>(story));
    }

    catalog
}
