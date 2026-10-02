//! Integration test: panicking on assertion failure is the desired
//! behaviour here, so the workspace `unwrap_used`/`expect_used` lints
//! are relaxed for this file. (Lib/bin code stays linted.)
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! High-level API tests: Frame, Layout, Paragraph/Block/Clear widgets render
//! correctly into the grid and the frame loop converges.

use nuotc::{
    Alignment, Block, BorderType, Borders, Clear, Color, Constraint, Direction, Frame, Layout,
    Line, Margin, Paragraph, Rect, Span, Style, Terminal, Wrap,
    backend::{Backend, Bce},
    diff,
    grid::{Fit, Grid},
};

#[test]
fn layout_split_vertical_three_rows() {
    let area = Rect::new(0, 0, 10, 30);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(0),
            Constraint::Length(5),
        ])
        .split(area);
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].height, 5);
    assert_eq!(chunks[0].y, 0);
    assert_eq!(chunks[1].height, 20);
    assert_eq!(chunks[2].height, 5);
    assert_eq!(chunks[2].y, 25);
}

#[test]
fn paragraph_writes_styled_text_into_grid() {
    let mut grid = Grid::new(20, 2);
    let style = Style::default().fg(Color::Rgb(255, 0, 0));
    let para = Paragraph::new(Line::from(Span::styled("hello", style)));
    {
        let mut frame = Frame::new(&mut grid);
        frame.render_widget(para, Rect::new(0, 0, 20, 1));
    }
    assert_eq!(grid.get(0, 0).unwrap().symbol, "h");
    assert_eq!(grid.get(4, 0).unwrap().symbol, "o");
    assert_eq!(grid.get(0, 0).unwrap().style.fg, Color::Rgb(255, 0, 0));
}

#[test]
fn paragraph_text_without_bg_preserves_existing_surface() {
    let mut grid = Grid::new(20, 1);
    let bg = Color::Rgb(7, 8, 9);
    grid.fill_rect(0, 0, 20, 1, Style::default().bg(bg));
    let para = Paragraph::new(Line::from(Span::styled(
        "hello",
        Style::default().fg(Color::Rgb(255, 0, 0)),
    )));
    {
        let mut frame = Frame::new(&mut grid);
        frame.render_widget(para, Rect::new(0, 0, 20, 1));
    }
    assert_eq!(grid.get(0, 0).unwrap().symbol, "h");
    assert_eq!(grid.get(0, 0).unwrap().style.bg, bg);
    assert_eq!(grid.get(4, 0).unwrap().style.bg, bg);
}

#[test]
fn paragraph_wraps_long_text() {
    let mut grid = Grid::new(5, 4);
    let para = Paragraph::new("hello world").wrap(Wrap { trim: false });
    {
        let mut frame = Frame::new(&mut grid);
        frame.render_widget(para, Rect::new(0, 0, 5, 4));
    }
    // "hello" on row 0, " worl" on row 1, "d" on row 2.
    assert_eq!(grid.get(0, 0).unwrap().symbol, "h");
    assert_eq!(grid.get(0, 1).unwrap().symbol, " ");
    assert_eq!(grid.get(1, 1).unwrap().symbol, "w");
    assert_eq!(grid.get(0, 2).unwrap().symbol, "d");
}

#[test]
fn paragraph_with_block_fills_bg_and_draws_left_bar() {
    let mut grid = Grid::new(10, 3);
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Rgb(1, 2, 3)))
        .style(Style::default().bg(Color::Rgb(10, 10, 10)));
    let para = Paragraph::new(Line::raw("hi")).block(block);
    {
        let mut frame = Frame::new(&mut grid);
        frame.render_widget(para, Rect::new(0, 0, 10, 3));
    }
    // Left border bar at col 0.
    assert_eq!(grid.get(0, 0).unwrap().symbol, "┃");
    // Background fill behind the bar.
    assert_eq!(grid.get(0, 0).unwrap().style.bg, Color::Rgb(10, 10, 10));
    // Text starts at col 1.
    assert_eq!(grid.get(1, 0).unwrap().symbol, "h");
}

#[test]
fn paragraph_center_alignment() {
    let mut grid = Grid::new(10, 1);
    let para = Paragraph::new(Line::raw("hi")).alignment(Alignment::Center);
    {
        let mut frame = Frame::new(&mut grid);
        frame.render_widget(para, Rect::new(0, 0, 10, 1));
    }
    // "hi" is 2 wide, centered in 10 → starts at col 4.
    assert_eq!(grid.get(4, 0).unwrap().symbol, "h");
    assert_eq!(grid.get(5, 0).unwrap().symbol, "i");
    assert_eq!(grid.get(3, 0).unwrap().symbol, " ");
}

#[test]
fn clear_resets_area_to_blank() {
    let mut grid = Grid::new(5, 2);
    grid.put(0, 0, Fit::Clip, Style::default(), "abcde");
    {
        let mut frame = Frame::new(&mut grid);
        frame.render_widget(Clear, Rect::new(0, 0, 3, 1));
    }
    assert_eq!(grid.get(0, 0).unwrap().symbol, " ");
    assert_eq!(grid.get(3, 0).unwrap().symbol, "d");
}

#[test]
fn rect_inner_margin() {
    let r = Rect::new(0, 0, 10, 10);
    let inner = r.inner(Margin::new(2, 1));
    assert_eq!(inner, Rect::new(2, 1, 6, 8));
}

#[test]
fn paragraph_scroll_skips_top_rows() {
    let mut grid = Grid::new(5, 2);
    let para = Paragraph::new(vec![
        Line::raw("row0"),
        Line::raw("row1"),
        Line::raw("row2"),
    ])
    .scroll(1, 0);
    {
        let mut frame = Frame::new(&mut grid);
        frame.render_widget(para, Rect::new(0, 0, 5, 2));
    }
    // Row 0 scrolled away; row1 is visible.
    assert_eq!(grid.get(0, 0).unwrap().symbol, "r");
    assert_eq!(grid.get(0, 1).unwrap().symbol, "r");
    assert_eq!(grid.get(0, 0).unwrap().symbol, "r");
}

#[test]
fn frame_loop_converges() {
    // Paint, then assert content reached stdout (backend owns the borrow for
    // the terminal's lifetime, so read after it drops).
    let mut buf = Vec::new();
    {
        let backend = Backend::with_bce(&mut buf, Bce::Yes);
        let mut term = Terminal::new(backend);
        term.resize_to(10, 2);
        term.draw(|f| {
            f.render_widget(Paragraph::new(Line::raw("hi")), Rect::new(0, 0, 10, 1));
        })
        .unwrap();
    }
    assert!(String::from_utf8_lossy(&buf).contains("hi"));

    // Convergence is a property of the diff, so test it at that level: after a
    // promote, a second diff against the stable grid is empty.
    let mut back = Grid::new(10, 2);
    let mut front = Grid::new(10, 2);
    back.put(0, 0, Fit::Clip, Style::default(), "hi");
    let first = diff::diff(&back, &front);
    assert!(!first.draws.is_empty());
    diff::promote(&mut back, &mut front);
    let second = diff::diff(&back, &front);
    assert!(second.draws.is_empty(), "idle frame emits nothing");
}

#[test]
fn resize_to_clears_screen_and_homes_cursor_on_commit() {
    let mut buf = Vec::new();
    {
        let backend = Backend::with_bce(&mut buf, Bce::Yes);
        let mut term = Terminal::new(backend);
        term.resize_to(20, 5);
        term.draw(|f| {
            f.render_widget(Paragraph::new(Line::raw("hello")), Rect::new(0, 0, 10, 1));
        })
        .unwrap();

        // Now resize again
        term.resize_to(30, 8);
        term.draw(|f| {
            f.render_widget(Paragraph::new(Line::raw("resized")), Rect::new(0, 0, 10, 1));
        })
        .unwrap();
    }
    let output = String::from_utf8_lossy(&buf);
    assert!(
        output.contains("\x1b[2J"),
        "resize commit must clear screen: {output:?}"
    );
    assert!(
        output.contains("\x1b[1;1H") || output.contains("\x1b[H"),
        "resize commit must home cursor before clear: {output:?}"
    );
}

#[test]
fn declarative_column_and_row_composition_in_terminal() {
    use nuotc::{Column, Divider, Padding, Row, WidgetExt};

    let mut buf = Vec::new();
    {
        let backend = Backend::with_bce(&mut buf, Bce::Yes);
        let mut term = Terminal::new(backend);
        term.resize_to(30, 10);

        // Build declarative UI tree
        let ui = Column::new()
            .child_with_constraint(Constraint::Length(1), Paragraph::new("Header Title"))
            .child_with_constraint(Constraint::Length(1), Divider::horizontal())
            .child_with_constraint(
                Constraint::Min(0),
                Row::new()
                    .child_with_constraint(Constraint::Length(10), Paragraph::new("Left Pane"))
                    .child_with_constraint(Constraint::Length(1), Divider::vertical())
                    .child_with_constraint(
                        Constraint::Min(0),
                        Paragraph::new("Content")
                            .with_padding(Padding::all(1))
                            .with_background(Style::default().bg(Color::Blue)),
                    ),
            );

        term.draw(|f| {
            f.render_widget(ui, f.area());
        })
        .unwrap();
    }

    let output = String::from_utf8_lossy(&buf);
    assert!(output.contains("Header"));
    assert!(output.contains("Title"));
    assert!(output.contains("Left"));
    assert!(output.contains("Pane"));
    assert!(output.contains("Content"));
    assert!(output.contains("─"));
    assert!(output.contains("│"));
}

#[test]
fn recording_canvas_and_render_node_caching_diff() {
    use nuotc::RenderNode;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().unwrap().flush()
        }
    }
    impl SharedBuf {
        fn content(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    let shared = SharedBuf::default();
    let backend = Backend::with_bce(shared.clone(), Bce::Yes);
    let mut term = Terminal::new(backend);
    term.resize_to(20, 5);

    let mut cached_node = RenderNode::new("status_bar");
    let mut build_count = 0;

    // Frame 1: Record into RenderNode and render
    term.draw(|f| {
        cached_node.record_if_dirty(1, Rect::new(0, 0, 20, 1), |c| {
            build_count += 1;
            c.put(0, 0, Style::default(), "Status: Ready");
        });
        f.render_node(&cached_node, f.area());
    })
    .unwrap();

    assert_eq!(build_count, 1);
    let out1 = shared.content();
    assert!(out1.contains("Status:") && out1.contains("Ready"));

    // Frame 2: Same revision and area -> should skip re-recording!
    shared.0.lock().unwrap().clear();
    term.draw(|f| {
        cached_node.record_if_dirty(1, Rect::new(0, 0, 20, 1), |_c| {
            build_count += 1;
        });
        f.render_node(&cached_node, f.area());
    })
    .unwrap();

    // Cache hit: build_count remains 1
    assert_eq!(build_count, 1);
    // Since frame 2 is identical to frame 1, terminal diff emits 0 draws (idle frame)
    assert!(
        shared.0.lock().unwrap().is_empty(),
        "Identical cached frame produces zero output bytes"
    );

    // Frame 3: Revision updated -> re-record triggered
    term.draw(|f| {
        cached_node.record_if_dirty(2, Rect::new(0, 0, 20, 1), |c| {
            build_count += 1;
            c.put(0, 0, Style::default(), "Status: Busy ");
        });
        f.render_node(&cached_node, f.area());
    })
    .unwrap();

    assert_eq!(build_count, 2);
    let out3 = shared.content();
    assert!(out3.contains("Busy"));
}
