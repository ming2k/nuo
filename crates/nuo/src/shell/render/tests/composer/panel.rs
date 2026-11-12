//! Panel padding, backgrounds, interjections, and markdown chrome.

use super::*;

#[test]
fn user_message_and_composer_keep_symmetric_panel_padding() {
    let theme = Theme::default();
    let user_bg = theme.user_surface();
    let app_bg = theme.surface();
    let input_bg = theme.input_surface();
    let width = 60u16;
    let mut terminal = nuotc::TestTerminal::new(width, 24);

    // A long user message fills the first wrapped line edge to edge, so the
    // right-side padding is only present if the wrap width reserves it.
    let messages = vec![TranscriptMessage::new(
        nuo_protocol::Role::User,
        "x".repeat(200),
    )];
    let long_input = "y".repeat(200);

    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        // draw_transcript only computes the input box geometry; the composer
        // itself is drawn separately (as the live app does), using the
        // returned input_rect.
        let render = draw_transcript(
            f,
            &mut layout_map,
            TranscriptProps {
                messages: &messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: &long_input,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    blocked: false,
                    expand_key: Some(crate::shell::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                interrupt_key: None,
                subagent_bar: None,
                page_hints: None,
                tab_bar: None,
                thread_id: None,
                thread_context: None,
                round_started_at: None,
                hovered_step: None,
                focused_entry: None,
                focus_plane: crate::shell::render::FocusPlane::Composer,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: nuo_view::layout::Strategy::default(),
                height_cache: None,
            },
        );
        let mut input_scroll = 0;
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: render.input_rect,
                theme: &theme,
                layout_map: &mut layout_map,
                input_scroll: &mut input_scroll,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: &long_input,
                byte_cursor: 0,
            },
            true,
            true,
            false,
            0,
            0,
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });

    let buffer = terminal.buffer();

    // Find the first user-message text row. Layout (60-col terminal):
    //   cols 0,1  = global app_bg (viewport margin)
    //   cols 2,3  = user_panel_bg inner pad (USER_MESSAGE_TEXT_GAP_COLS)
    //   col  4+   = text
    let user_row = (0..buffer.area().height)
        .find(|&y| {
            let c4 = &buffer.get(4, y).unwrap();
            c4.symbol.as_str() == "x" && c4.style.bg == user_bg
        })
        .expect("user message row exists");

    // Left: 2-col app_bg outer gutter (viewport margin + entry inset),
    // then 2-col user_panel_bg inner pad.
    assert_eq!(
        buffer.get(0, user_row).unwrap().style.bg,
        app_bg,
        "left outer gutter"
    );
    assert_eq!(
        buffer.get(1, user_row).unwrap().style.bg,
        app_bg,
        "left outer gutter"
    );
    assert_eq!(
        buffer.get(2, user_row).unwrap().style.bg,
        user_bg,
        "left inner padding must be user_panel_bg"
    );
    assert_eq!(
        buffer.get(3, user_row).unwrap().style.bg,
        user_bg,
        "left inner padding is 2 cols, not 1"
    );
    assert_eq!(
        buffer.get(4, user_row).unwrap().symbol.as_str(),
        "x",
        "text starts at col 4"
    );

    // Right: 2-col user_panel_bg inner pad, then 2-col app_bg outer gutter.
    // user_text_width = (band_w) - (TEXT_GAP + RIGHT_PAD) = (60-4) - 4 = 52
    // -> text fills cols 4..56.
    assert_eq!(
        buffer.get(56, user_row).unwrap().symbol.as_str(),
        " ",
        "right inner padding must stay clear of wrapped text"
    );
    assert_eq!(
        buffer.get(56, user_row).unwrap().style.bg,
        user_bg,
        "right inner padding"
    );
    assert_eq!(
        buffer.get(57, user_row).unwrap().style.bg,
        user_bg,
        "right inner padding"
    );
    assert_eq!(
        buffer.get(58, user_row).unwrap().style.bg,
        app_bg,
        "right outer gutter"
    );
    assert_eq!(
        buffer.get(59, user_row).unwrap().style.bg,
        app_bg,
        "right outer gutter"
    );

    // Composer: the panel starts at x = FOOTER_H_INSET (2) and carries its
    // own tinted bg. `›` at x=2, text from x=4 — the same text column the
    // user panel uses (cols 2,3 are the prompt prefix; text lands at 4) —
    // so sent messages and the live input read as one left margin.
    let composer_row = (0..buffer.area().height)
        .find(|&y| {
            let c4 = &buffer.get(4, y).unwrap();
            c4.symbol.as_str() == "y" && c4.style.bg == input_bg
        })
        .expect("composer row exists");
    assert_eq!(
        buffer.get(2, composer_row).unwrap().symbol.as_str(),
        "›",
        "composer prompt opens the panel at the footer inset"
    );
    assert_eq!(
        buffer.get(2, composer_row).unwrap().style.bg,
        input_bg,
        "composer panel begins at the footer inset"
    );
    assert_eq!(
        buffer.get(4, composer_row).unwrap().symbol.as_str(),
        "y",
        "composer text starts at the same column as user text"
    );
    // full_w (composer panel) = 60 - 2*FOOTER_H_INSET = 56, spanning
    // x=2..58; the last panel column stays panel-tinted air.
    assert_eq!(
        buffer.get(57, composer_row).unwrap().style.bg,
        input_bg,
        "composer panel air extends to its last column"
    );
    assert_eq!(
        buffer.get(58, composer_row).unwrap().style.bg,
        app_bg,
        "composer right outer gutter"
    );
    assert_eq!(
        buffer.get(59, composer_row).unwrap().style.bg,
        app_bg,
        "composer right outer gutter"
    );
}

/// The input box's tinted panel owns two surface tokens — active (the box
/// owns the keyboard) and inactive (a transcript step owns it). The two
/// backgrounds must be visibly different colors, so "where does typing
/// land" is legible from the panel alone, and neither may melt into the
/// app background. Regression guard for the activated/deactivated input
/// being indistinguishable.
#[test]
fn composer_focused_and_unfocused_panels_render_distinct_backgrounds() {
    let theme = Theme::default();
    let active_bg = theme.input_surface();
    let inactive_bg = theme.input_surface_inactive();
    let app_bg = theme.surface();
    assert_ne!(active_bg, inactive_bg, "pair must be distinct colors");

    let panel_at = |focused: bool| -> nuotc::Color {
        let mut terminal = nuotc::TestTerminal::new(30, 6);
        terminal.draw(|f| {
            draw_composer(
                ComposerProps {
                    frame: f,
                    input_rect: Rect::new(0, 0, 30, 4),
                    theme: &theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: &mut 0,
                    selection: &SelectionState::None,
                },
                ComposerText {
                    input: "hello",
                    byte_cursor: 5,
                },
                focused,
                false,
                false,
                0,
                0,
                crate::shell::composer_hints::ComposerHints::default(),
            );
        });
        let buffer = terminal.buffer();
        // The top breathing row: painted unconditionally in panel bg, so its
        // background carries the surface color either way.
        let cell = &buffer.get(0, 0).unwrap();
        assert_eq!(cell.symbol.as_str(), " ", "top breathing row is blank");
        cell.style.bg
    };

    let rendered_active = panel_at(true);
    let rendered_inactive = panel_at(false);
    assert_eq!(
        rendered_active, active_bg,
        "focused box must paint the input-surface color"
    );
    assert_eq!(
        rendered_inactive, inactive_bg,
        "unfocused box must paint the input-surface-inactive color"
    );
    assert_ne!(
        rendered_active, app_bg,
        "focused panel must not melt into the app background"
    );
    assert_ne!(
        rendered_inactive, app_bg,
        "unfocused panel must not melt into the app background"
    );
    // The whole box carries the tint, not just an edge: every row of the
    // four-row panel is the same surface.
    let mut terminal = nuotc::TestTerminal::new(30, 6);
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 30, 4),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: "hello",
                byte_cursor: 5,
            },
            true,
            false,
            false,
            0,
            0,
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });
    for y in 0..4u16 {
        assert_eq!(
            terminal.buffer().get(15, y).unwrap().style.bg,
            active_bg,
            "every composer row carries the input surface"
        );
    }
}

/// ADR-0137: an admitted turn-seam interjection (InRound steer) renders as a
/// one-body-row block: no separate header row, no header/body gap row, no
/// solid transition padding rows. A one-line steer therefore costs exactly
/// one body row (plus the shared inter-message gap), and its identity reads
/// as `› steer` on that row.
#[test]
fn in_round_interjection_renders_one_body_row() {
    use nuo_view::model::document::SteerOriginTag;
    let theme = Theme::default();
    let width = 72u16;
    let mut terminal = nuotc::TestTerminal::new(width, 24);

    let steer = TranscriptMessage::new(nuo_protocol::Role::User, "use rg, not find")
        .with_origin(nuo_view::model::document::UserMessageOrigin::Steer)
        .with_sent_at_ms(1_700_000_000_000)
        .with_origin_tags(vec![SteerOriginTag::InRound]);
    let messages = vec![steer];

    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        let _ = draw_transcript(
            f,
            &mut layout_map,
            TranscriptProps {
                messages: &messages,
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
                    blocked: false,
                    expand_key: Some(crate::shell::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                interrupt_key: None,
                subagent_bar: None,
                page_hints: None,
                tab_bar: None,
                thread_id: None,
                thread_context: None,
                round_started_at: None,
                hovered_step: None,
                focused_entry: None,
                focus_plane: crate::shell::render::FocusPlane::Composer,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: nuo_view::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });

    let buffer = terminal.buffer();
    let row_text = |y: u16| -> String {
        (0..buffer.area().width)
            .map(|x| buffer.get(x, y).unwrap().symbol.as_str().to_string())
            .collect()
    };
    let grid = (0..buffer.area().height).map(row_text).collect::<Vec<_>>();
    // Sanity: the tag must be honored (the message claims InRound).
    assert!(
        messages[0].has_origin_tag(SteerOriginTag::InRound),
        "test message must carry the InRound tag"
    );
    let rendered = grid.clone().join("\n");
    assert!(
        rendered.contains("›") && rendered.contains("steer"),
        "interjection identity must render the steer lead:\n{rendered}"
    );
    // Row 0..1 is the stream-top gap; the interjection body is row 2.
    let first = grid[2].clone();
    let rest = grid[3..].to_vec();
    assert!(
        first.contains("steer") && first.contains("use rg, not find"),
        "interjection identity row must carry the steer lead and text: {first:?}  grid:\n{rendered}"
    );
    assert!(
        rendered.contains("›  steer"),
        "the steer lead must be the embossed `›  steer` prefix:\n{rendered}"
    );
    assert!(
        !rendered.contains("queued") && !rendered.contains("› round 1"),
        "an admitted interjection is not a pending queued panel and carries no round header row:\n{rendered}"
    );
    assert!(
        rest.iter().all(|row| row.trim().is_empty()),
        "a one-line interjection must not add rows below it: {rest:?}"
    );
}

/// The dropdown shares the composer's surface language, not the permission
/// sheet's: it opens and closes with full panel-bg padding rows and never
/// paints a full-height brand-colored left column (which would read as
/// selection/severity). The top and bottom rows must be solid panel
/// background (no half-block `▄`/`▀` glyphs), and the left column must NOT
/// be brand-colored.
#[test]
fn history_panel_uses_composer_padding_not_brand_column() {
    let theme = Theme::default();
    let history: Vec<nuo_protocol::HistoryEntry> = ["one", "two", "three"]
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            nuo_protocol::HistoryEntry::new(text.to_string(), Some(format!("s{i}")), None, i as u64)
        })
        .collect();
    let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();
    let ranked = nuo_ui_primitives::fuzzy::rank(&texts, "");
    let input_rect = nuotc::Rect::new(0, 40, 30, 2);
    let mut terminal = nuotc::TestTerminal::new(30, 42);
    let mut panel: Option<nuotc::Rect> = None;
    terminal.draw(|f| {
        panel = draw_history_panel(
            f,
            crate::shell::overlays::history::HistoryPanelProps {
                history: &history,
                ranked: &ranked,
                modal_index: 0,
                scroll: &mut 0,
                follow_selection: true,
                input_rect,
                activity_height: 0,
            },
            &theme,
        )
    });
    let panel = panel.expect("panel should render");
    let buf = terminal.buffer();

    // Top row is a full panel-bg padding row (no half-block glyph).
    let top_left = buf.get(panel.x, panel.y).expect("top-left cell");
    assert_eq!(
        top_left.style.bg,
        theme.panel(),
        "top edge must be a solid panel-bg row, matching the composer's padding"
    );
    assert_eq!(
        top_left.symbol.as_str(),
        " ",
        "top edge must be blank (no ▄ transition glyph)"
    );
    // Bottom row is likewise a solid panel-bg padding row.
    let bottom_left = buf
        .get(panel.x, panel.y + panel.height - 1)
        .expect("bottom-left cell");
    assert_eq!(
        bottom_left.style.bg,
        theme.panel(),
        "bottom edge must be a solid panel-bg row, matching the composer's padding"
    );
    assert_eq!(
        bottom_left.symbol.as_str(),
        " ",
        "bottom edge must be blank (no ▀ transition glyph)"
    );

    // No full-height brand column: the background of the left column on the
    // header row (which is never selection-tinted) must NOT be the brand
    // color. A brand column would paint every left-edge cell, including the
    // header's, with brand as its background. The header sits one row below
    // the top transition edge.
    let header_left = buf.get(panel.x, panel.y + 1).expect("header left cell");
    assert_ne!(
        header_left.style.bg,
        theme.brand(),
        "no full-height brand left column — the composer edge language has none"
    );
}

/// Same clamp check with a multi-codepoint emoji grapheme (ZWJ family) in
/// the heading: `wrap_text` measures per-char (overcounting the sequence)
/// while the grid renders per-grapheme, so this guards the underline width
/// against the char-vs-grapheme measurement split.
#[test]
fn h1_underline_clamps_with_emoji_grapheme() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(60, 12);
    let messages = vec![TranscriptMessage::new(
        nuo_protocol::Role::Assistant,
        "# 👨‍👩‍👧 OKX\n\nbody\n",
    )];
    terminal.draw(|f| {
        let _ = draw_transcript(
            f,
            &mut LayoutMap::new(),
            TranscriptProps {
                messages: &messages,
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
                    blocked: false,
                    expand_key: Some(crate::shell::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                interrupt_key: None,
                subagent_bar: None,
                page_hints: None,
                tab_bar: None,
                thread_id: None,
                thread_context: None,
                round_started_at: None,
                hovered_step: None,
                focused_entry: None,
                focus_plane: crate::shell::render::FocusPlane::Composer,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: nuo_view::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });
    let buffer = terminal.buffer();
    let width = buffer.area().width;
    let underline = nuotc::Modifier::UNDERLINE;

    let mut x_pos = None;
    'outer: for y in 0..buffer.area().height {
        for x in 0..width {
            if buffer.get(x, y).unwrap().symbol.as_str() == "X" {
                x_pos = Some((x, y));
                break 'outer;
            }
        }
    }
    let (xx, xy) = x_pos.expect("heading 'X' cell exists");

    assert!(
        buffer.get(xx, xy).unwrap().style.add.contains(underline),
        "heading 'X' text cell must be UNDERLINED"
    );
    let trailing = xx + 1;
    assert!(trailing < width, "trailing cell within grid");
    assert!(
        !buffer
            .get(trailing, xy)
            .unwrap()
            .style
            .add
            .contains(underline),
        "underline must not bleed past emoji heading at x={trailing}"
    );
}
