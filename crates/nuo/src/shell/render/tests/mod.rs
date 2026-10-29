//! The view rendering test suite, split by surface. Shared fixtures
//! (full-view renderer, grid row reader) live here.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::shell::composer::{ComposerProps, ComposerText};
use nuo_view::markdown_table::{build_table_render, shrink_column_widths};
use nuo_view::text_layout::wrap_text;
use unicode_width::UnicodeWidthStr;

fn render_full_view(
    width: u16,
    height: u16,
    messages: &[TranscriptMessage],
    page_hints: Option<ViewHints<'_>>,
) -> nuotc::TestTerminal {
    render_full_view_with_context(width, height, messages, page_hints, None)
}

fn render_full_view_with_context(
    width: u16,
    height: u16,
    messages: &[TranscriptMessage],
    page_hints: Option<ViewHints<'_>>,
    thread_context: Option<ThreadContext<'_>>,
) -> nuotc::TestTerminal {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(width, height);
    let hints = page_hints;
    let context = thread_context.or({
        Some(ThreadContext {
            role: Some("developer"),
            workspace: "~/projects/xx",
            unattended: false,
            confined: true,
        })
    });
    let tab_bar = Some(TabBarProps {
        tabs: None,
        active_tab: 0,
    });
    terminal.draw(|f| {
        let _ = draw_transcript(
            f,
            &mut LayoutMap::new(),
            TranscriptProps {
                messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::shell::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                subagent_bar: None,
                page_hints: hints,
                tab_bar,
                notice_dock: None,
                thread_id: Some("sess-01a2b3c4"),
                thread_context: context,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: nuo_view::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });
    terminal
}

fn grid_row(terminal: &nuotc::TestTerminal, y: u16) -> String {
    let buffer = terminal.buffer();
    let width = buffer.area().width;
    (0..width)
        .map(|x| buffer.get(x, y).unwrap().symbol.as_str())
        .collect()
}

mod chrome;
mod composer;
mod history_panel;
mod layout_map;
mod selection;
mod tables;
mod tool_steps;
mod turn_headers;

/// fresh `wrap_text` / code preparation produces, across widths and content
/// shapes (including CJK and empty input).
#[test]
fn wrap_cache_is_transparent() {
    let samples = [
        "",
        "short",
        "A much longer paragraph that will definitely wrap across several lines \
         when the width is small enough, with 中文混排 and `code spans` too.",
        "line one\nline two\n\nline four",
    ];
    let mut cache = crate::shell::render::BlockWrapCache::default();
    for text in samples {
        for width in [1usize, 8, 40, 200] {
            let cached = cache.wrap_text(text, width);
            let fresh = nuo_view::text_layout::wrap_text(text, width);
            assert_eq!(cached.len(), fresh.len());
            for (c, f) in cached.iter().zip(fresh.iter()) {
                assert_eq!(
                    (c.text.as_str(), c.start_byte, c.end_byte),
                    (f.text.as_str(), f.start_byte, f.end_byte)
                );
            }
        }
    }
    // Code preparation: same logical split, same per-line wrap.
    let code = "fn main() {\n    let s = \"中文字符串很长的确会换行\";\n}\n";
    for width in [20usize, 60] {
        let prepared = cache.prepare_code(code, width);
        let mut fresh: Vec<(usize, Vec<_>)> = Vec::new();
        let mut offset = 0usize;
        for line in code.split('\n') {
            let mut wrapped = nuo_view::text_layout::wrap_text(line, width);
            if wrapped.is_empty() {
                wrapped.push(nuo_view::text_layout::WrappedLine {
                    text: String::new(),
                    start_byte: 0,
                    end_byte: 0,
                });
            }
            fresh.push((offset, wrapped));
            offset += line.len() + 1;
        }
        assert_eq!(prepared.logical.len(), fresh.len());
        for ((po, pw), (fo, fw)) in prepared.logical.iter().zip(fresh.iter()) {
            assert_eq!(po, fo);
            assert_eq!(pw.len(), fw.len());
            for (c, f) in pw.iter().zip(fw.iter()) {
                assert_eq!(
                    (c.text.as_str(), c.start_byte, c.end_byte),
                    (f.text.as_str(), f.start_byte, f.end_byte)
                );
            }
        }
    }
}
