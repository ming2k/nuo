//! The view rendering test suite, split by surface. Shared fixtures
//! (full-view renderer, grid row reader) live here.

use super::*;
use crate::composer::{ComposerProps, ComposerText};
use crate::markdown_table::{build_table_render, shrink_column_widths};
use crate::text_layout::wrap_text;
use unicode_width::UnicodeWidthStr;

fn render_full_view(
    width: u16,
    height: u16,
    messages: &[TranscriptMessage],
    page_hints: Option<ViewHints<'_>>,
) -> nuotc::TestTerminal {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(width, height);
    let hints = page_hints;
    terminal.draw(|f| {
        let _ = draw_transcript(
            f,
            &mut LayoutMap::new(),
            TranscriptProps {
                messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: None,
                side_banner: None,
                page_hints: hints,
                session_head: Some(SessionHead {
                    session_id: "sess-01a2b3c4",
                    workspace: "~/projects/xx",
                    role: Some("developer"),
                    switching_target: None,
                    tabs: None,
                    active_tab: 0,
                }),
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });
    terminal
}

fn grid_row(terminal: &nuotc::TestTerminal, y: u16) -> String {
    let buffer = terminal.buffer();
    let width = buffer.area().width;
    (0..width).map(|x| buffer[(x, y)].symbol()).collect()
}

mod chrome;
mod composer;
mod history_panel;
mod layout_map;
mod selection;
mod tables;
mod tool_steps;
