//! Testing Utilities for Declarative Components and Stories (ADR-0073).

use nuotc::{Color, Modifier, TestTerminal};
use super::story::AnyStory;
use crate::theme::Theme;

/// Render an `AnyStory` in a fresh headless terminal and extract the full
/// cell grid including foreground, background, and modifier state.
pub fn render_story_styled_snapshot(story: &dyn AnyStory, width: u16, height: u16) -> String {
    let mut terminal = TestTerminal::new(width, height);
    let theme = Theme::default();

    terminal.draw(|f| {
        story.render(f, f.area(), &theme);
    });

    let buf = terminal.buffer();
    let bw = buf.area().width as usize;
    let bh = buf.area().height as usize;

    // 1. Text matrix
    let mut rows: Vec<String> = Vec::with_capacity(bh);
    for y in 0..bh {
        let mut row = String::new();
        for x in 0..bw {
            let cell = &buf.content[y * bw + x];
            row.push_str(cell.symbol());
        }
        rows.push(row.trim_end().to_string());
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    let text_grid = rows.join("\n");

    // 2. Style summary layer (colors and modifiers)
    let style_layer = style_matrix(buf);

    if style_layer.is_empty() {
        text_grid
    } else {
        format!("{text_grid}\n\n--- styles ---\n{style_layer}")
    }
}

fn style_matrix(buf: &nuotc::Grid) -> String {
    let bw = buf.area().width as usize;
    let bh = buf.area().height as usize;

    let mut distinct_styles = Vec::new();
    for y in 0..bh {
        for x in 0..bw {
            let style = buf.content[y * bw + x].style();
            if (style.fg != Color::Reset || style.bg != Color::Reset || style.add != Modifier::empty())
                && !distinct_styles.contains(&style)
            {
                distinct_styles.push(style);
            }
        }
    }

    if distinct_styles.is_empty() {
        return String::new();
    }

    let symbol_of = |s: nuotc::Style| -> char {
        if s.fg == Color::Reset && s.bg == Color::Reset && s.add == Modifier::empty() {
            '.'
        } else {
            let idx = distinct_styles.iter().position(|&x| x == s).unwrap_or(0);
            (b'A' + (idx % 26) as u8) as char
        }
    };

    let legend = distinct_styles
        .iter()
        .map(|&s| {
            format!(
                "{}: fg={:?} bg={:?} mod={:?}",
                symbol_of(s),
                s.fg,
                s.bg,
                s.add
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let mut map_lines = Vec::new();
    for y in 0..bh {
        let mut line = String::new();
        for x in 0..bw {
            let s = buf.content[y * bw + x].style();
            line.push(symbol_of(s));
        }
        let trimmed = line.trim_end_matches('.');
        if !trimmed.is_empty() {
            map_lines.push(trimmed.to_string());
        }
    }

    format!("{legend}\n\n{}", map_lines.join("\n"))
}
