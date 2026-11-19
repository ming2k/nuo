use super::*;

fn notice(status: LineStatus, topic: &str, text: &str, at_ms: u64) -> SurfaceLine {
    SurfaceLine::disclosure(status, text, at_ms).with_label(topic)
}

fn all_text(terminal: &graticule::TestTerminal) -> String {
    let buf = terminal.buffer();
    let area = buf.area();
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buf.get(x, y) {
                out.push_str(&cell.symbol);
            }
        }
        out.push('\n');
    }
    out
}

#[test]
fn notices_modal_renders_header_fault_count_and_items() {
    let theme = Theme::default();
    let lines = vec![
        notice(
            LineStatus::Failed,
            "backend",
            "link down",
            1_700_000_000_000,
        ),
        notice(
            LineStatus::Warn,
            "images",
            "image withheld",
            1_700_000_060_000,
        ),
    ];
    let mut dialog = NoticesDialog {
        index: 1,
        ..Default::default()
    };
    let mut layout_map = LayoutMap::default();
    let mut terminal = graticule::TestTerminal::new(100, 30);
    terminal.draw(|f| {
        draw_notices_modal(
            f,
            f.area(),
            &lines,
            &mut dialog,
            &theme,
            &SelectionState::None,
            &mut layout_map,
        );
    });
    let text = all_text(&terminal);
    assert!(text.contains("Notices"), "header names the surface: {text}");
    assert!(
        text.contains("2/2"),
        "header shows compact card index: {text}"
    );
    assert!(text.contains("backend"), "renders topic: {text}");
    assert!(text.contains("link down"), "renders title: {text}");
    assert!(text.contains("images"), "renders second topic: {text}");
    assert!(text.contains("fault"), "the header counts faults: {text}");
    assert!(
        !text.contains("───") && !text.contains("Notices & Disclosures"),
        "minimalist layout uses spatial gaps instead of divider lines and verbose title: {text}"
    );
}

#[test]
fn a_long_message_is_truncated_on_the_row_and_wrapped_in_the_detail() {
    let theme = Theme::default();
    let long = "Failed to open link https://example.test/a/very/long/path:                     permission denied by the operating system";
    let lines = vec![notice(LineStatus::Failed, "link", long, 1_700_000_000_000)];
    let mut dialog = NoticesDialog {
        detail: true,
        ..Default::default()
    };
    let mut layout_map = LayoutMap::default();
    let mut terminal = graticule::TestTerminal::new(100, 30);
    terminal.draw(|f| {
        draw_notices_modal(
            f,
            f.area(),
            &lines,
            &mut dialog,
            &theme,
            &SelectionState::None,
            &mut layout_map,
        );
    });
    let text = all_text(&terminal);
    assert!(
        text.contains("permission") && text.contains("denied"),
        "detail is wrapped: {text}"
    );
    assert!(text.contains("link"), "states topic: {text}");
    assert!(
        !text.contains("┌ Detail:") && !text.contains('│') && !text.contains('└'),
        "expanded detail uses spatial indentation without box-drawing clutter: {text}"
    );
}

#[test]
fn an_empty_log_explains_itself_instead_of_showing_a_blank_panel() {
    let theme = Theme::default();
    let mut dialog = NoticesDialog::default();
    let mut layout_map = LayoutMap::default();
    let mut terminal = graticule::TestTerminal::new(100, 30);
    terminal.draw(|f| {
        draw_notices_modal(
            f,
            f.area(),
            &[],
            &mut dialog,
            &theme,
            &SelectionState::None,
            &mut layout_map,
        );
    });
    let text = all_text(&terminal);
    assert!(text.contains("No operational notices"), "{text}");
}

#[test]
fn severity_band_conveys_severity_without_glyph_or_text_badge() {
    // ADR-0139: the card header carries severity by color alone. The topic
    // pill is painted on the derived band; no `▲`/`✗`/`✓`/`ℹ` glyph and no
    // bracketed `[ERROR]`-style text badge may appear on the header row.
    let theme = Theme::default();
    let lines = vec![
        notice(LineStatus::Failed, "a", "boom", 1),
        notice(LineStatus::Warn, "b", "degraded", 2),
        notice(LineStatus::Ok, "c", "done", 3),
    ];
    let mut dialog = NoticesDialog::default();
    let mut layout_map = LayoutMap::default();
    let mut terminal = graticule::TestTerminal::new(100, 30);
    terminal.draw(|f| {
        draw_notices_modal(
            f,
            f.area(),
            &lines,
            &mut dialog,
            &theme,
            &SelectionState::None,
            &mut layout_map,
        );
    });
    let text = all_text(&terminal);

    // The topics are still present (the pill carries the word).
    assert!(text.contains("a"), "renders first topic: {text}");
    assert!(text.contains("b"), "renders second topic: {text}");
    assert!(text.contains("c"), "renders third topic: {text}");

    // Severity comes from the band, never from legacy markers.
    assert!(
        !text.contains("[ERROR]")
            && !text.contains("[WARN]")
            && !text.contains("[OK]")
            && !text.contains("[INFO]"),
        "text badges are gone: {text}"
    );
    assert!(
        !text.contains("▲") && !text.contains("ℹ"),
        "legacy raw glyphs are gone: {text}"
    );
    // On a color-capable theme the band is a real background wash on the
    // topic pill — not a no-op. Assert the cell under the topic actually
    // carries the derived band background.
    let buf = terminal.buffer();
    let band_bg = theme.severity_band(theme.err(), "error").bg;
    let found_band = (0..30u16)
        .any(|y| (0..100u16).any(|x| buf.get(x, y).is_some_and(|c| c.style.bg == band_bg)));
    assert!(
        found_band,
        "the severity band background must actually be painted, not merely \
         absent-glyph: {band_bg:?}"
    );
}

#[test]
fn monochrome_still_names_severity_after_the_glyph_removal() {
    // The regression this guards: the deleted `▲`/`!`/`ℹ` glyph was the
    // ONLY severity signal on a monochrome/vt100 terminal. Color carries
    // nothing there, so the band must fall back to a textual marker or
    // severity becomes invisible on exactly the profile the glyph served.
    let theme = Theme {
        glyphs: graticule::ASCII_GLYPHS,
        ..Theme::from_color_scheme(
            "monochrome",
            &ui_primitives::color_scheme::ColorSchemeConfig::default(),
        )
    };
    assert!(!theme.severity_is_color_distinguishable());
    let lines = vec![
        notice(LineStatus::Failed, "a", "boom", 1),
        notice(LineStatus::Warn, "b", "degraded", 2),
    ];
    let mut dialog = NoticesDialog::default();
    let mut layout_map = LayoutMap::default();
    let mut terminal = graticule::TestTerminal::new(100, 30);
    terminal.draw(|f| {
        draw_notices_modal(
            f,
            f.area(),
            &lines,
            &mut dialog,
            &theme,
            &SelectionState::None,
            &mut layout_map,
        );
    });
    let text = all_text(&terminal);
    assert!(
        text.contains("error"),
        "monochrome must name the error severity in text: {text}"
    );
    assert!(
        text.contains("warn"),
        "monochrome must name the warning severity in text: {text}"
    );
}

#[test]
fn notice_content_registers_selectable_regions_and_paints_selection() {
    use view::model::layout::{MODAL_DOC_MSG_IDX, SemanticCursor};

    let theme = Theme::default();
    let lines = vec![notice(
        LineStatus::Failed,
        "backend",
        "connection refused by upstream",
        1_700_000_000_000,
    )];
    let mut dialog = NoticesDialog::default();
    let mut layout_map = LayoutMap::default();
    let mut terminal = graticule::TestTerminal::new(100, 30);
    terminal.draw(|f| {
        draw_notices_modal(
            f,
            f.area(),
            &lines,
            &mut dialog,
            &theme,
            &SelectionState::None,
            &mut layout_map,
        );
    });

    let region = layout_map
        .first_region_for_message(MODAL_DOC_MSG_IDX)
        .expect("notice modal body must register MODAL_DOC_MSG_IDX regions");
    assert!(region.rect.width > 0);

    // Select the headline row (block_idx = 1) and verify selection highlight paints.
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(MODAL_DOC_MSG_IDX, 1, 0),
        head: SemanticCursor::new(MODAL_DOC_MSG_IDX, 1, 10),
    };
    layout_map = LayoutMap::default();
    terminal.draw(|f| {
        draw_notices_modal(
            f,
            f.area(),
            &lines,
            &mut dialog,
            &theme,
            &sel,
            &mut layout_map,
        );
    });
    let buf = terminal.buffer();
    let sel_bg = theme.selected();
    let has_selection_bg = (0..30u16)
        .any(|y| (0..100u16).any(|x| buf.get(x, y).is_some_and(|c| c.style.bg == sel_bg)));
    assert!(
        has_selection_bg,
        "selected notice text must paint with theme.selected() background"
    );
}

#[test]
fn wrapping_respects_the_column_budget() {
    let rows = wrap_row("aaa bbb ccc ddd", 7);
    assert_eq!(rows, vec!["aaa bbb", "ccc ddd"]);
    assert!(rows.iter().all(|r| r.width() <= 7));
}

#[test]
fn truncation_is_width_aware_not_byte_aware() {
    let out = truncate_ellipsis("后端连接失败，请检查网络配置", 8);
    assert!(out.width() <= 8, "{out:?}");
    assert!(out.ends_with('…'));
}
