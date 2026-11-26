use super::*;
use crate::shell::render::Theme;
use graticule::Rect;
use view::model::layout::LayoutMap;

/// Paint the completion menu into a test buffer and return the rect the
/// popup actually occupied (found by scanning for the popup background),
/// so assertions can check alignment and full-width highlighting without
/// duplicating the layout math.
fn paint_completion_menu(
    input_anchor_x: u16,
    selected: Option<usize>,
) -> (graticule::TestTerminal, Rect) {
    let theme = Theme::default();
    let completions = vec![
        crate::shell::completion::Completion {
            label: "/repeat".to_string(),
            description: "Schedule a prompt on a cron".to_string(),
            insert_text: "/repeat".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::shell::completion::CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        },
        crate::shell::completion::Completion {
            label: "/permissions".to_string(),
            description: "Manage permissions".to_string(),
            insert_text: "/permissions".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::shell::completion::CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        },
    ];
    let mut terminal = graticule::TestTerminal::new(80, 12);
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        draw_completion_menu(
            f,
            &mut layout_map,
            None,
            &completions,
            selected,
            Rect::new(input_anchor_x, 10, 1, 1),
            &theme,
        );
    });
    // The two rows directly above the input box are the popup.
    (terminal, Rect::new(input_anchor_x, 8, 80, 2))
}

#[test]
fn completion_menu_left_edge_aligns_with_anchor_column() {
    let (terminal, popup) = paint_completion_menu(2, None);
    let buf = terminal.buffer();
    let body = Theme::default().body();
    // Row start of the popup: cells left of the anchor column keep the
    // app background; the popup body starts exactly at the anchor column.
    let y = popup.y;
    let at_anchor = buf.get(2, y).expect("cell at anchor column");
    assert_eq!(
        at_anchor.style.bg, body,
        "popup body must start at the anchor"
    );
    assert_eq!(at_anchor.symbol.as_str(), "/");
    let left_of_anchor = buf.get(1, y).expect("cell left of anchor");
    assert_ne!(
        left_of_anchor.style.bg, body,
        "popup must not start before the anchor"
    );
}

#[test]
fn completion_menu_selected_row_is_one_solid_band_full_width() {
    let theme = Theme::default();
    let (terminal, popup) = paint_completion_menu(2, Some(0));
    let buf = terminal.buffer();
    let brand = theme.brand();
    let body = theme.body();
    let y = popup.y; // first popup row = selected row
    // Find the popup's horizontal extent on this row (cells whose bg is
    // the popup body/brand rather than the app background).
    let row_cells: Vec<u16> = (0..buf.area().width)
        .filter(|&x| {
            let bg = buf.get(x, y).map(|c| c.style.bg);
            bg == Some(brand) || bg == Some(body)
        })
        .collect();
    assert!(!row_cells.is_empty(), "popup row not found");
    let (first, last) = (*row_cells.first().unwrap(), *row_cells.last().unwrap());
    // Every cell of the selected row inside the popup extent carries the
    // selection background — label, the padding between label and
    // description, and the fill out to the popup's right edge — so the
    // highlight reads as one continuous band.
    for x in first..=last {
        assert_eq!(
            buf.get(x, y).map(|c| c.style.bg),
            Some(brand),
            "cell ({x}, {y}) broke the selection band"
        );
    }
    // The band spans across the menu width for the candidate.
    assert!(
        last - first >= 12,
        "popup band too narrow: {first}..={last}"
    );
    // The unselected row keeps the popup body background across its full
    // width (no brand cell leaks onto it).
    let second_row = popup.y + 1;
    for x in first..=last {
        assert_eq!(
            buf.get(x, second_row).map(|c| c.style.bg),
            Some(body),
            "cell ({x}, {second_row}) of the unselected row lost the body bg"
        );
    }
}

#[test]
fn completion_menu_caps_width_and_stays_anchored() {
    let theme = Theme::default();
    let completions = [
        ("/models", "Switch the active model"),
        ("/tools", "Manage session tools (enable/disable)"),
        (
            "/auto-approve",
            "Toggle auto-approve mode — skip permission prompts (on/off)",
        ),
    ]
    .iter()
    .map(|(l, d)| crate::shell::completion::Completion {
        label: l.to_string(),
        description: d.to_string(),
        insert_text: l.to_string(),
        replace_start: 0,
        replace_end: 1,
        kind: crate::shell::completion::CompletionItemKind::Slash,
        alias_of: None,
        doc: None,
    })
    .collect::<Vec<_>>();
    let mut terminal = graticule::TestTerminal::new(80, 12);
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        draw_completion_menu(
            f,
            &mut layout_map,
            None,
            &completions,
            None,
            Rect::new(2, 10, 1, 1),
            &theme,
        );
    });
    let buf = terminal.buffer();
    let body = theme.body();
    // The popup keeps its anchor: body-colored cells start at column 2,
    // never stretch to the right edge of the 80-column viewport.
    let y = 9u16; // last popup row (3 candidates above rows 10..12)
    let cells: Vec<u16> = (0..80u16)
        .filter(|&x| buf.get(x, y).map(|c| c.style.bg) == Some(body))
        .collect();
    let (first, last) = (*cells.first().unwrap(), *cells.last().unwrap());
    assert_eq!(first, 2, "popup must stay anchored at the typed token");
    assert!(
        (last - first + 1) as usize <= 80 * 3 / 5,
        "popup must not fill the viewport: {first}..={last}"
    );
    let row_text: String = (first..=last)
        .filter_map(|x| buf.get(x, y).map(|c| c.symbol.as_str().to_string()))
        .collect();
    assert!(
        row_text.starts_with("/auto-approve"),
        "row was {row_text:?}"
    );
}

#[test]
fn completion_menu_renders_compact_entry_list_without_inline_descriptions() {
    let (terminal, popup) = paint_completion_menu(2, None);
    let buf = terminal.buffer();
    let row_text = |y: u16| -> String {
        (0..buf.area().width)
            .filter_map(|x| buf.get(x, y).map(|c| c.symbol.as_str().to_string()))
            .collect()
    };
    let first = row_text(popup.y);
    // Pure command entry in the left menu: no inline description text or separator
    assert!(first.contains("/repeat"), "row was {first:?}");
    assert!(
        !first.contains("Schedule a prompt on a cron"),
        "inline description should not appear in candidate list: {first:?}"
    );
    assert!(!first.contains('|'), "row was {first:?}");
}

#[test]
fn completion_menu_marks_alias_rows_with_canonical_target() {
    // An alias candidate is marked with [*] in the menu list.
    let theme = Theme::default();
    let completions = vec![
        crate::shell::completion::Completion {
            label: "/auto-approve".to_string(),
            description: "Toggle auto-approve mode".to_string(),
            insert_text: "/auto-approve".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::shell::completion::CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        },
        crate::shell::completion::Completion {
            label: "/auto".to_string(),
            description: "Toggle auto-approve mode".to_string(),
            insert_text: "/auto-approve".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::shell::completion::CompletionItemKind::SlashAlias,
            alias_of: Some("/auto-approve".to_string()),
            doc: None,
        },
    ];
    let mut terminal = graticule::TestTerminal::new(80, 12);
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        draw_completion_menu(
            f,
            &mut layout_map,
            None,
            &completions,
            None,
            Rect::new(2, 10, 1, 1),
            &theme,
        );
    });
    let buf = terminal.buffer();
    let row_text = |y: u16| -> String {
        (0..buf.area().width)
            .filter_map(|x| buf.get(x, y).map(|c| c.symbol.as_str().to_string()))
            .collect()
    };
    let alias_row = row_text(9); // popup bottom row = second candidate
    assert!(
        alias_row.trim_start().starts_with("/auto [*]"),
        "alias shows [*] marker: {alias_row:?}"
    );
    let canonical_row = row_text(8);
    assert!(
        canonical_row.trim_start().starts_with("/auto-approve"),
        "canonical row is plain: {canonical_row:?}"
    );
    assert!(
        !canonical_row.contains("[*]"),
        "canonical rows carry no alias marker: {canonical_row:?}"
    );
}

#[test]
fn completion_menu_hover_doc_flyout_only_appears_when_entry_is_selected() {
    let theme = Theme::default();
    let doc = crate::shell::completion::CommandDoc {
        name: "/schedule".to_string(),
        summary: "Schedule a prompt on a cron or countdown".to_string(),
        usage: vec!["/schedule <when> <prompt>".to_string()],
        category: Some("Automation".to_string()),
        subcommands: vec![
            ("list".to_string(), "List scheduled prompts".to_string()),
            (
                "cancel".to_string(),
                "Cancel one schedule by id".to_string(),
            ),
        ],
    };
    let completions = vec![crate::shell::completion::Completion {
        label: "/schedule".to_string(),
        description: "Schedule a prompt".to_string(),
        insert_text: "/schedule".to_string(),
        replace_start: 0,
        replace_end: 2,
        kind: crate::shell::completion::CompletionItemKind::Slash,
        alias_of: None,
        doc: Some(doc),
    }];

    // 1. Unselected (selected = None): No right-side doc window is rendered
    let mut term_unselected = graticule::TestTerminal::new(80, 12);
    term_unselected.draw(|f| {
        let mut layout_map = LayoutMap::new();
        draw_completion_menu(
            f,
            &mut layout_map,
            None,
            &completions,
            None,
            Rect::new(2, 10, 1, 1),
            &theme,
        );
    });
    let buf_unselected = term_unselected.buffer();
    let panel_bg = theme.panel();
    let has_panel_unselected =
        (0..80u16).any(|x| buf_unselected.get(x, 9).map(|c| c.style.bg) == Some(panel_bg));
    assert!(
        !has_panel_unselected,
        "unselected completion must not render hover doc flyout"
    );

    // 2. Selected (selected = Some(0)): Right-side doc window is rendered with panel_bg
    let mut term_selected = graticule::TestTerminal::new(80, 12);
    term_selected.draw(|f| {
        let mut layout_map = LayoutMap::new();
        draw_completion_menu(
            f,
            &mut layout_map,
            None,
            &completions,
            Some(0),
            Rect::new(2, 10, 1, 1),
            &theme,
        );
    });
    let buf_selected = term_selected.buffer();
    let has_panel_selected =
        (0..80u16).any(|x| buf_selected.get(x, 9).map(|c| c.style.bg) == Some(panel_bg));
    assert!(
        has_panel_selected,
        "selected completion must render hover doc flyout with panel bg"
    );
}

#[test]
fn completion_menu_hover_doc_flyout_shows_alias_to_target_header() {
    let theme = Theme::default();
    let doc = crate::shell::completion::CommandDoc {
        name: "/auto-approve".to_string(),
        summary: "Toggle auto-approve mode".to_string(),
        usage: vec!["/auto-approve".to_string()],
        category: Some("Agent".to_string()),
        subcommands: vec![],
    };
    let completions = vec![crate::shell::completion::Completion {
        label: "/auto".to_string(),
        description: "Toggle auto-approve mode".to_string(),
        insert_text: "/auto-approve".to_string(),
        replace_start: 0,
        replace_end: 2,
        kind: crate::shell::completion::CompletionItemKind::SlashAlias,
        alias_of: Some("/auto-approve".to_string()),
        doc: Some(doc),
    }];

    let mut term = graticule::TestTerminal::new(80, 12);
    term.draw(|f| {
        let mut layout_map = LayoutMap::new();
        draw_completion_menu(
            f,
            &mut layout_map,
            None,
            &completions,
            Some(0),
            Rect::new(2, 10, 1, 1),
            &theme,
        );
    });
    let buf = term.buffer();
    let row_text = |y: u16| -> String {
        (0..buf.area().width)
            .filter_map(|x| buf.get(x, y).map(|c| c.symbol.as_str().to_string()))
            .collect()
    };
    // Check that the flyout header contains "/auto -> /auto-approve"
    let full_text: Vec<String> = (0..12).map(row_text).collect();
    let found_header = full_text
        .iter()
        .any(|r| r.contains("/auto -> /auto-approve"));
    assert!(
        found_header,
        "flyout header should show `/auto -> /auto-approve`, got buffer:\n{}",
        full_text.join("\n")
    );
}

#[test]
fn completion_menu_structured_monochrome_renders_framed_box_and_fits_all_items() {
    let mut theme = Theme::monochrome();
    theme.glyphs = graticule::ASCII_GLYPHS;
    let completions = vec![
        crate::shell::completion::Completion {
            label: "/help".to_string(),
            description: "Show help".to_string(),
            insert_text: "/help".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::shell::completion::CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        },
        crate::shell::completion::Completion {
            label: "/exit".to_string(),
            description: "Exit shell".to_string(),
            insert_text: "/exit".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::shell::completion::CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        },
    ];
    let mut terminal = graticule::TestTerminal::new(80, 12);
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        draw_completion_menu(
            f,
            &mut layout_map,
            None,
            &completions,
            Some(0),
            Rect::new(2, 10, 1, 1),
            &theme,
        );
    });
    let screen = (0..12)
        .map(|y| {
            (0..80)
                .map(|x| {
                    terminal
                        .buffer()
                        .get(x, y)
                        .map(|c| c.symbol.as_str())
                        .unwrap_or(" ")
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("/help"), "must render /help item: {screen}");
    assert!(
        screen.contains("/exit"),
        "must render /exit item without truncation: {screen}"
    );
    assert!(
        screen.contains('+'),
        "must render ASCII border corners: {screen}"
    );
    assert!(
        screen.contains('-'),
        "must render ASCII horizontal border: {screen}"
    );
}

/// A queued row with no reported stamp must render **no** time, not the epoch.
///
/// The projection used to default a missing `sent_at_ms` to `0`, and
/// `sent_time_label(0)` is the Unix epoch in local time — `08:00` in UTC+8. The
/// modal therefore painted a fabricated wall-clock beside the user's own queued
/// message, which is a claim about when they wrote it.
#[test]
fn queue_modal_does_not_invent_a_time_for_a_row_without_one() {
    use crate::shell::overlays::{QueueModalProps, draw_queue_modal};
    use crate::shell::render::QueueItemProps;

    let theme = Theme::default();
    let render = |queued_at_ms: Option<u64>| -> String {
        let mut terminal = graticule::TestTerminal::new(100, 16);
        terminal.draw(|f| {
            let items = vec![QueueItemProps {
                queued_at_ms,
                text: "queued prompt".to_string(),
                is_steer: false,
            }];
            let mut scroll = 0usize;
            draw_queue_modal(
                f,
                QueueModalProps {
                    items: &items,
                    blocked: false,
                },
                0,
                &mut scroll,
                true,
                &theme,
            );
        });
        terminal
            .buffer()
            .cells()
            .iter()
            .map(|c| c.symbol.as_str())
            .collect()
    };

    let with_stamp = render(Some(1_700_000_000_123));
    assert!(
        with_stamp.contains(':'),
        "a real stamp still renders a clock: {with_stamp:?}"
    );

    let without = render(None);
    assert!(
        without.contains("queued prompt"),
        "the row still renders: {without:?}"
    );
    assert!(
        !without.contains(':'),
        "no stamp means no invented time (the epoch would be 08:00 here): {without:?}"
    );
}
