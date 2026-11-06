//! Interactive Offline Storybook Workbench (ADR-0073).
//!
//! A zero-dependency, hermetic full-screen terminal workbench for
//! rapid visual inspection, viewport emulation, live theme switching, and interaction testing.

use crossterm::event::{self, Event, KeyCode};

use nuotc::{
    Borders, Frame, Line, Modifier, Paragraph, Rect, Span, Style, Terminal, widgets::Block,
};
use std::io::{self, Stdout};
use std::path::Path;

use super::ask_user::AskUser;
use super::empty_hero_card::EmptyHeroCard;
use super::modal_card::ModalCard;
use super::notice_card::NoticeCard;
use super::notice_dialog_card::NoticeDialogCard;
use super::palette_card::PaletteCard;
use super::reasoning_card::ReasoningCard;
use super::story::{AnyStory, StorybookComponent, instantiate_story};
use super::testing::render_story_styled_snapshot;
use super::toast_card::ToastCard;
use super::tool_card::ToolCard;
use super::turn_band_card::TurnBandCard;
use super::which_key_card::WhichKeyCard;
use nuo_ui_primitives::theme::Theme;

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

        let x = available
            .x
            .saturating_add((available.width.saturating_sub(target_w)) / 2);
        let y = available
            .y
            .saturating_add((available.height.saturating_sub(target_h)) / 2);

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

/// Persisted continuity state for zero-friction recompilation and developer continuity (ADR-0146 `[INV-TUI-STORY-05]`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StorybookContinuityState {
    pub selected_component: String,
    pub selected_story: String,
    pub viewport_mode: String,
    pub theme_index: usize,
    pub search_query: String,
}

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
    is_playing: bool,
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
            is_playing: true,
        };
        wb.update_theme();
        wb.restore_continuity();
        wb
    }

    fn continuity_path() -> std::path::PathBuf {
        std::env::temp_dir().join("nuo_storybook_continuity.json")
    }

    /// Persist current story navigation and environment context to disk (ADR-0146 `[INV-TUI-STORY-05]`).
    pub fn persist_continuity(&self) {
        let (comp, story) = if let Some(idx) = self.current_story_index() {
            if let Some(s) = self.stories.get(idx) {
                (s.component_name().to_string(), s.story_name().to_string())
            } else {
                (String::new(), String::new())
            }
        } else {
            (String::new(), String::new())
        };

        let state = StorybookContinuityState {
            selected_component: comp,
            selected_story: story,
            viewport_mode: self.viewport_mode.label().to_string(),
            theme_index: self.theme_index,
            search_query: self.search_query.clone(),
        };

        if let Ok(json) = serde_json::to_string(&state) {
            let _ = std::fs::write(Self::continuity_path(), json);
        }
    }

    /// Apply a continuity capsule onto this workbench state (ADR-0146 `[INV-TUI-STORY-05]`).
    pub fn apply_continuity_state(&mut self, state: StorybookContinuityState) {
        self.theme_index = state.theme_index % THEME_PRESETS.len();
        self.update_theme();

        self.viewport_mode = match state.viewport_mode.as_str() {
            "80x24" => ViewportMode::Size80x24,
            "100x30" => ViewportMode::Size100x30,
            "120x40" => ViewportMode::Size120x40,
            _ => ViewportMode::Full,
        };

        if !state.search_query.is_empty() {
            self.search_query = state.search_query;
            self.recompute_filter();
        }

        if !state.selected_component.is_empty() && !state.selected_story.is_empty() {
            for (pos, &story_idx) in self.filtered_indices.iter().enumerate() {
                if let Some(story) = self.stories.get(story_idx) {
                    if story.component_name() == state.selected_component
                        && story.story_name() == state.selected_story
                    {
                        self.selected_pointer = pos;
                        break;
                    }
                }
            }
        }
    }

    /// Restore story navigation and environment context from disk (ADR-0146 `[INV-TUI-STORY-05]`).
    pub fn restore_continuity(&mut self) {
        let path = Self::continuity_path();
        let Ok(data) = std::fs::read_to_string(path) else {
            return;
        };
        if let Ok(state) = serde_json::from_str::<StorybookContinuityState>(&data) {
            self.apply_continuity_state(state);
        }
    }

    pub fn stories(&self) -> &[Box<dyn AnyStory>] {
        &self.stories
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn theme_index(&self) -> usize {
        self.theme_index
    }

    pub fn viewport_mode(&self) -> ViewportMode {
        self.viewport_mode
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

    pub fn current_story_index(&self) -> Option<usize> {
        self.filtered_indices.get(self.selected_pointer).copied()
    }

    /// Run the interactive storybook loop directly on the physical terminal.
    pub fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let profile = crate::shell::terminal::detect_terminal_profile();
        crate::shell::terminal::enter_terminal(&profile)?;
        crate::shell::ensure_panic_hook();
        // Off the first-frame path: the probe can block for up to 2 s on a
        // terminal that does not answer the kitty-protocol query.
        std::thread::Builder::new()
            .name("nuo-kbd-enhance".into())
            .spawn(move || crate::shell::terminal::negotiate_keyboard_enhancement(&profile))?;

        let stdout = io::stdout();
        let backend = nuotc::Backend::new(stdout).with_profile(profile);
        let mut terminal = Terminal::new(backend);

        let result = self.run_loop(&mut terminal);

        crate::shell::terminal::restore_terminal();
        result
    }

    fn run_loop(
        &mut self,
        terminal: &mut Terminal<Stdout>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tick_interval = std::time::Duration::from_millis(60);
        let mut last_tick = std::time::Instant::now();

        loop {
            terminal.draw(|frame| {
                self.draw_ui(frame);
            })?;

            let timeout = tick_interval.saturating_sub(last_tick.elapsed());
            if event::poll(timeout)? {
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
                                    story.handle_key(
                                        &crate::shell::input::keystroke::key_from_event(key),
                                    );
                                }
                            }
                        }
                        WorkbenchMode::Searching => match key.code {
                            KeyCode::Esc | KeyCode::Enter => {
                                self.mode = WorkbenchMode::Browsing;
                            }
                            KeyCode::Backspace => {
                                self.search_query.pop();
                                self.recompute_filter();
                                self.persist_continuity();
                            }
                            KeyCode::Char(c) => {
                                self.search_query.push(c);
                                self.recompute_filter();
                                self.persist_continuity();
                            }
                            _ => {}
                        },
                        WorkbenchMode::Browsing => match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Char('/') => {
                                self.mode = WorkbenchMode::Searching;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if !self.filtered_indices.is_empty() {
                                    self.selected_pointer =
                                        (self.selected_pointer + 1) % self.filtered_indices.len();
                                    self.persist_continuity();
                                }
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if !self.filtered_indices.is_empty() {
                                    self.selected_pointer = self
                                        .selected_pointer
                                        .checked_sub(1)
                                        .unwrap_or(self.filtered_indices.len() - 1);
                                    self.persist_continuity();
                                }
                            }
                            KeyCode::Char(' ') | KeyCode::Enter => {
                                if !self.filtered_indices.is_empty() {
                                    self.mode = WorkbenchMode::Interacting;
                                }
                            }
                            KeyCode::Char('v') | KeyCode::Char('V') => {
                                self.viewport_mode = self.viewport_mode.next();
                                self.persist_continuity();
                            }
                            KeyCode::Char('t') | KeyCode::Char('T') => {
                                self.cycle_theme();
                                self.persist_continuity();
                            }
                            KeyCode::Char('p') | KeyCode::Char('P') => {
                                self.is_playing = !self.is_playing;
                            }
                            KeyCode::Char('r') | KeyCode::Char('R') => {
                                if let Some(idx) = self.current_story_index() {
                                    if let Some(story) = self.stories.get_mut(idx) {
                                        story.reset();
                                    }
                                }
                            }
                            _ => {}
                        },
                    }
                }
            }

            if self.is_playing && last_tick.elapsed() >= tick_interval {
                if let Some(story_idx) = self.current_story_index() {
                    if let Some(story) = self.stories.get_mut(story_idx) {
                        story.tick();
                    }
                }
                last_tick = std::time::Instant::now();
            }
        }

        self.persist_continuity();
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
            WorkbenchMode::Browsing => {
                Span::styled(" [BROWSING] ", Style::default().fg(self.theme.system_fg))
            }
            WorkbenchMode::Searching => Span::styled(
                " [SEARCH] ",
                Style::default()
                    .fg(self.theme.app_bg)
                    .bg(self.theme.system_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            WorkbenchMode::Interacting => Span::styled(
                " [INTERACTING] ",
                Style::default()
                    .fg(self.theme.app_bg)
                    .bg(self.theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
        };

        let (_, theme_label) = THEME_PRESETS[self.theme_index % THEME_PRESETS.len()];
        let liveness_badge = if self.is_playing {
            Span::styled(
                " ● LIVE ",
                Style::default()
                    .fg(self.theme.ok())
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(" ⏸ PAUSED ", Style::default().fg(self.theme.warning))
        };

        let header_spans = vec![
            Span::styled(
                " Nuo Storybook ",
                Style::default()
                    .fg(self.theme.text)
                    .add_modifier(Modifier::BOLD),
            ),
            mode_badge,
            liveness_badge,
            Span::raw(" | Viewport: "),
            Span::styled(
                self.viewport_mode.label(),
                Style::default().fg(self.theme.warning),
            ),
            Span::raw(" | Theme: "),
            Span::styled(theme_label, Style::default().fg(self.theme.system_fg)),
            Span::raw(" | Stories: "),
            Span::styled(
                format!(
                    "{}/{}",
                    self.selected_pointer + 1,
                    self.filtered_indices.len()
                ),
                Style::default().fg(self.theme.text),
            ),
        ];
        frame.render_widget(
            &mut Paragraph::new(Line::from(header_spans))
                .style(Style::default().bg(self.theme.panel_bg)),
            header_rect,
        );

        // 2. Footer Bar
        let footer_text = match self.mode {
            WorkbenchMode::Browsing => {
                " [↑/↓] Select | [/] Search | [Space] Interact | [V] Box | [T] Theme | [P] Play/Pause | [R] Reset | [Q] Quit"
            }
            WorkbenchMode::Searching => " Type query to filter | [Enter/Esc] Confirm search",
            WorkbenchMode::Interacting => {
                " [ESC] Exit Component Interaction back to Storybook Navigation"
            }
        };
        frame.render_widget(
            &mut Paragraph::new(Line::from(Span::styled(
                footer_text,
                Style::default().fg(self.theme.text_muted),
            )))
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
        let mut catalog_block = Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(self.theme.dim_fg))
            .style(Style::default().bg(self.theme.panel_bg));
        frame.render_widget(&mut catalog_block, catalog_rect);

        let inner_catalog = Rect::new(
            catalog_rect.x.saturating_add(1),
            catalog_rect.y,
            catalog_rect.width.saturating_sub(2),
            catalog_rect.height,
        );

        let mut catalog_lines = Vec::new();

        if !self.search_query.is_empty() || self.mode == WorkbenchMode::Searching {
            catalog_lines.push(Line::from(vec![
                Span::styled(
                    "Filter: ",
                    Style::default()
                        .fg(self.theme.warning)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    &self.search_query,
                    Style::default()
                        .fg(self.theme.text)
                        .add_modifier(Modifier::UNDERLINE),
                ),
            ]));
            catalog_lines.push(Line::raw(""));
        }

        let mut last_category = "";
        let mut selected_line_idx = 0;
        for (pos, &story_idx) in self.filtered_indices.iter().enumerate() {
            let story = &self.stories[story_idx];
            if story.category() != last_category {
                last_category = story.category();
                catalog_lines.push(Line::from(Span::styled(
                    format!("▼ {}", last_category),
                    Style::default()
                        .fg(self.theme.warning)
                        .add_modifier(Modifier::BOLD),
                )));
            }

            let is_selected = pos == self.selected_pointer;
            if is_selected {
                selected_line_idx = catalog_lines.len();
            }
            let prefix = if is_selected { " ▶ " } else { "   " };
            let title = format!("{}: {}", story.component_name(), story.story_name());
            let line_style = if is_selected {
                Style::default()
                    .fg(self.theme.text)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.theme.text_muted)
            };

            catalog_lines.push(Line::from(vec![
                Span::styled(
                    prefix,
                    if is_selected {
                        Style::default().fg(self.theme.system_fg)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(title, line_style),
            ]));
        }

        let visible_h = inner_catalog.height as usize;
        let scroll_y = if selected_line_idx >= visible_h {
            (selected_line_idx.saturating_sub(visible_h.saturating_sub(3))) as u16
        } else {
            0
        };

        frame.render_widget(
            &mut Paragraph::new(catalog_lines).scroll(scroll_y, 0),
            inner_catalog,
        );

        // Draw Canvas Stage
        let target_canvas_rect = self.viewport_mode.clamp_rect(canvas_outer_rect);

        if self.viewport_mode != ViewportMode::Full {
            let mut stage_box = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(self.theme.system_fg));
            let inner_target = stage_box.inner(target_canvas_rect);
            frame.render_widget(&mut stage_box, target_canvas_rect);

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
            let safe_story_name = story
                .story_name()
                .to_lowercase()
                .replace([' ', '/', '\\', ':', '*'], "_");
            let filename = format!(
                "{}_{}.snapshot.txt",
                story.component_name().to_lowercase(),
                safe_story_name
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

    // 1. NoticeCard (Overlays) - 4 stories
    for story in NoticeCard::stories() {
        catalog.push(instantiate_story::<NoticeCard>(story));
    }

    // 2. AskUser (Overlays) - 4 stories
    for story in AskUser::stories() {
        catalog.push(instantiate_story::<AskUser>(story));
    }

    // 3. ToolCard (Disclosure) - 5 stories
    for story in ToolCard::stories() {
        catalog.push(instantiate_story::<ToolCard>(story));
    }

    // 4. Toast (Overlays) - 5 stories
    for story in ToastCard::stories() {
        catalog.push(instantiate_story::<ToastCard>(story));
    }

    // 5. WhichKey (Navigation) - 3 stories
    for story in WhichKeyCard::stories() {
        catalog.push(instantiate_story::<WhichKeyCard>(story));
    }

    // 6. ModalDialog (Overlays) - 3 stories
    for story in ModalCard::stories() {
        catalog.push(instantiate_story::<ModalCard>(story));
    }

    // 7. TurnBand (Transcript) - 3 stories
    for story in TurnBandCard::stories() {
        catalog.push(instantiate_story::<TurnBandCard>(story));
    }

    // 8. EmptyStateHero (Transcript) - 3 stories
    for story in EmptyHeroCard::stories() {
        catalog.push(instantiate_story::<EmptyHeroCard>(story));
    }

    // 9. ColorPalette (System) - 4 stories
    for story in PaletteCard::stories() {
        catalog.push(instantiate_story::<PaletteCard>(story));
    }

    // 10. ReasoningTrace (Disclosure) - 3 stories
    for story in ReasoningCard::stories() {
        catalog.push(instantiate_story::<ReasoningCard>(story));
    }

    // 11. NoticeCenterDialog (Overlays) - 5 stories
    for story in NoticeDialogCard::stories() {
        catalog.push(instantiate_story::<NoticeDialogCard>(story));
    }

    catalog
}
