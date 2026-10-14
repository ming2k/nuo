use super::model_bar::context_usage_spans;
use super::*;
use crate::model::layout::LayoutMap;
use crate::render::Theme;
use nuotc::{Color, Rect};

fn activity_row_text(width: u16, status: &str, phase: usize) -> String {
    activity_row_text_with_clause(width, status, None, false, phase)
}

fn activity_row_text_with_clause(
    width: u16,
    status: &str,
    backoff_clause: Option<&str>,
    awaiting: bool,
    phase: usize,
) -> String {
    let mut terminal = nuotc::TestTerminal::new(width, 1);
    terminal.draw(|frame| {
        draw_activity_bar(
            frame,
            Rect::new(0, 0, width, 1),
            None,
            crate::chrome::ActivityBarProps {
                status,
                backoff_clause,
                awaiting_permission: awaiting,
            },
            phase,
            &Theme::default(),
        );
    });
    terminal
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
}

/// Render the activity bar and collect the foreground color of each cell,
/// so a test can assert e.g. the permission state paints in the warning
/// hue rather than the shimmer palette.
fn activity_row_colors(width: u16, status: &str, awaiting: bool, phase: usize) -> Vec<Color> {
    activity_row_colors_with_clause(width, status, None, awaiting, phase)
}

fn activity_row_colors_with_clause(
    width: u16,
    status: &str,
    backoff_clause: Option<&str>,
    awaiting: bool,
    phase: usize,
) -> Vec<Color> {
    let mut terminal = nuotc::TestTerminal::new(width, 1);
    terminal.draw(|frame| {
        draw_activity_bar(
            frame,
            Rect::new(0, 0, width, 1),
            None,
            crate::chrome::ActivityBarProps {
                status,
                backoff_clause,
                awaiting_permission: awaiting,
            },
            phase,
            &Theme::default(),
        );
    });
    terminal
        .buffer()
        .content
        .iter()
        .map(|cell| cell.fg)
        .collect()
}

#[test]
fn activity_bar_preserves_interrupt_hint_at_minimum_width() {
    let row = activity_row_text(
        36,
        "retrying a provider request after a very detailed transient failure",
        8,
    );
    assert!(row.contains("Esc Esc interrupt"), "row was {row:?}");
    assert!(row.contains('…'), "long status was not truncated: {row:?}");
}

#[test]
fn tilde_home_shortens_a_home_rooted_path() {
    let home = dirs::home_dir()
        .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
        .expect("test requires a discoverable home directory");
    let under = home.join("projects").join("xx");
    let rendered = tilde_home(&under);
    assert_eq!(
        rendered,
        std::path::PathBuf::from("~")
            .join("projects")
            .join("xx")
            .display()
            .to_string()
    );

    // The home directory itself collapses to a bare `~`.
    assert_eq!(tilde_home(&home), "~");
}

#[test]
fn backoff_clause_renders_beside_status_and_degrades_narrow() {
    // Root status label keeps the workflow story; the transport countdown is a
    // separate, muted clause — never replacing the label.
    let wide = activity_row_text_with_clause(
        100,
        "waiting for model",
        Some("retry 2/8 (next in 4s)"),
        false,
        0,
    );
    assert!(wide.contains("waiting for model"), "{wide:?}");
    assert!(wide.contains("retry 2/8 (next in 4s)"), "{wide:?}");

    // Under width pressure the compact attempt counter survives and the
    // status label is still intact.
    let narrow = activity_row_text_with_clause(
        48,
        "waiting for model",
        Some("retry 2/8 (next in 4s)"),
        false,
        0,
    );
    assert!(narrow.contains("waiting for model"), "{narrow:?}");
    assert!(
        narrow.contains("(2/8)"),
        "compact form should drop the countdown tail and keep (2/8): {narrow:?}"
    );

    // No clause configured → no stray separators.
    let plain = activity_row_text_with_clause(80, "answering", None, false, 0);
    assert!(plain.contains("answering"), "{plain:?}");
}

#[test]
fn activity_bar_carries_no_todos_badge() {
    // Decoupled: the activity bar is a pure liveness surface now and never
    // embeds a `todos d/t` summary.
    let mut terminal = nuotc::TestTerminal::new(80, 1);
    terminal.draw(|frame| {
        draw_activity_bar(
            frame,
            Rect::new(0, 0, 80, 1),
            None,
            ActivityBarProps {
                status: "Working",
                backoff_clause: None,
                awaiting_permission: false,
            },
            0,
            &Theme::default(),
        );
    });
    let text = terminal
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(!text.contains("todos"), "badge leaked onto bar: {text:?}");
    assert!(!text.contains("Ctrl-t"), "hint leaked onto bar: {text:?}");
}

#[test]
fn narrow_runtime_row_keeps_interrupt_keys_without_todos_badge() {
    let mut terminal = nuotc::TestTerminal::new(36, 1);
    terminal.draw(|frame| {
        draw_activity_bar(
            frame,
            Rect::new(0, 0, 36, 1),
            None,
            ActivityBarProps {
                status: "retrying a provider request after a detailed transient failure",
                backoff_clause: None,
                awaiting_permission: false,
            },
            8,
            &Theme::default(),
        );
    });
    let text = terminal
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Esc Esc"), "row was {text:?}");
    // The activity row does not carry a todos summary.
    assert!(!text.contains("todos"), "badge leaked: {text:?}");
    // Session-state flags live on the hint bar; the activity row never
    // carries them, even when they would fit.
    assert!(!text.contains("autopilot"), "row was {text:?}");
}

/// A pending permission request paints the status label in a steady warning
/// hue rather than the ordinary shimmer palette, so the bar reads as a
/// distinct attention state ("the round is paused on your decision") above
/// the permission sheet. The warning hue must actually appear on the label
/// cells, distinguishing it from the brand-colored shimmer.
#[test]
fn activity_bar_paints_awaiting_permission_in_warning_hue() {
    let theme = Theme::default();
    let awaiting = activity_row_colors(80, "awaiting permission", true, 4);
    let normal = activity_row_colors(80, "working", false, 4);

    // The warning color must be present somewhere in the awaiting row.
    assert!(
        awaiting.contains(&theme.warning),
        "awaiting-permission row must use the warning hue"
    );
    // A permission state must not shimmer (the shimmer sweeps the brand hue
    // across phases). The normal row, by contrast, carries brand-derived
    // colors at this phase.
    assert!(
        !awaiting.contains(&theme.warning) || awaiting != normal,
        "awaiting row must differ from the ordinary shimmer row"
    );
    // Sanity: the normal row does carry some non-warning color from the
    // shimmer (so the comparison above is meaningful).
    assert!(
        normal
            .iter()
            .any(|&c| c != theme.muted() && c != Color::Reset),
        "normal row should carry shimmer colors"
    );
}

#[test]
fn format_token_count_uses_si_suffixes() {
    assert_eq!(format_token_count(0), "0");
    assert_eq!(format_token_count(999), "999");
    assert_eq!(format_token_count(1000), "1.0k");
    assert_eq!(format_token_count(20_200), "20.2k");
    assert_eq!(format_token_count(1_000_000), "1.0M");
    assert_eq!(format_token_count(3_200_000_000), "3.2B");
}

#[test]
fn context_usage_spans_render_used_and_percentage() {
    let theme = Theme::default();
    let spans = context_usage_spans(20_200, 256_000, &theme, theme.panel());
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "20.2k (8%)");
    // Color psychology: calm/muted at low ratio, warning at >=70%, error at >=90%.
    assert_eq!(spans[1].style.fg, theme.muted());

    let warn_spans = context_usage_spans(195_000, 256_000, &theme, theme.panel());
    assert_eq!(warn_spans[1].style.fg, theme.warn());

    let crit_spans = context_usage_spans(240_000, 256_000, &theme, theme.panel());
    assert_eq!(crit_spans[1].style.fg, theme.err());
}

/// Split-row contract: the telemetry gauges (`context`, `rate`, and unified
/// anchor the left half, and the identity group (`model effort
/// @instance`) pins right — reading left → right as **context → speed →
/// identity**. Under width pressure the keycap hint drops first, then
/// the instance suffix (provenance is nice-to-have) while the model
/// name, effort tag, and context meter all still fit.
#[test]
fn model_bar_orders_context_then_model() {
    let row_text = |width: u16| -> String {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|f| {
            draw_model_bar(
                f,
                Rect::new(0, 0, width, 1),
                ModelBarProps {
                    current_model: "kimi-k2.7-code",
                    model_available: true,
                    provider_name: Some("kimi-code"),
                    reasoning_effort: Some("max"),
                    ..Default::default()
                },
                &Theme::default(),
                &crate::keymap::GlobalOverrides::default(),
            );
        });
        let buf = terminal.buffer();
        (0..buf.area().width as usize)
            .map(|x| buf.content[x].symbol().to_string())
            .collect::<String>()
    };

    // Wide enough for everything: `ctx` left,
    // `model effort @instance` right, in that left-to-right order.
    let wide = row_text(80);
    let ctx_pos = wide.find("(0%)").expect("context meter shown");
    let model_pos = wide.find("kimi-k2.7-code").expect("model shown");
    assert!(
        ctx_pos < model_pos,
        "row must read context → identity: {wide:?}"
    );
    let inst_pos = wide.find("@kimi-code").expect("instance suffix shown");
    assert!(model_pos < inst_pos, "instance follows the model: {wide:?}");
    // Unbound telemetry command renders no keycap hint (ADR-0238).
    assert!(
        !wide.contains("Ctrl-o"),
        "unbound telemetry renders no keycap: {wide:?}"
    );
    // Justified split: the identity cluster pins flush to the row's
    // right edge (mirrored `inner` indent).
    assert!(
        wide.trim_end()
            .ends_with("kimi-k2.7-code max @kimi-code"),
        "identity must end at the right edge: {wide:?}"
    );

    // Narrower row: instance suffix drops at 35,
    // while the context meter, model name, and effort tag survive in order.
    let narrow = row_text(42);
    assert!(
        narrow.contains("@kimi-code"),
        "provenance suffix survives at 48: {narrow:?}"
    );
    let tighter = row_text(35);
    assert!(
        !tighter.contains('@'),
        "instance should hide next: {tighter:?}"
    );
    let ctx_pos = tighter.find("(0%)").expect("context survives");
    let model_pos = tighter.find("kimi-k2.7-code").expect("model survives");
    assert!(
        ctx_pos < model_pos,
        "order must hold after dropping the hints: {tighter:?}"
    );
}

#[test]
fn model_bar_renders_model_and_context() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 3);
    terminal.draw(|f| {
        draw_model_bar(
            f,
            Rect::new(0, 2, 80, 1),
            ModelBarProps {
                current_model: "mock-model",
                model_available: true,
                provider_name: Some("mock-instance"),
                ..Default::default()
            },
            &theme,
            &crate::keymap::GlobalOverrides::default(),
        );
    });
    let buf = terminal.buffer();
    let text = (0..buf.area().width as usize)
        .map(|x| buf.content[2 * 80 + x].symbol().to_string())
        .collect::<String>();
    assert!(
        text.contains("mock-model @mock-instance"),
        "row was {text:?}"
    );
}

#[test]
fn model_bar_renders_context_gauge_for_projected_route_context_window() {
    // ADR-0182: A dynamically discovered relay model (e.g. glm-5.3 via opencode-go)
    // passes its projected route context_window. Even though the model is unknown
    // to the static baseline registry, the context gauge renders correctly.
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(100, 3);
    terminal.draw(|f| {
        draw_model_bar(
            f,
            Rect::new(0, 2, 100, 1),
            ModelBarProps {
                current_model: "glm-5.3",
                model_available: true,
                provider_name: Some("opencode-go"),
                context_tokens: Some(50_000),
                context_window: 1_000_000,
                ..Default::default()
            },
            &theme,
            &crate::keymap::GlobalOverrides::default(),
        );
    });
    let buf = terminal.buffer();
    let text = (0..buf.area().width as usize)
        .map(|x| buf.content[2 * 100 + x].symbol().to_string())
        .collect::<String>();
    assert!(
        text.contains("50.0k") && text.contains("(5%)"),
        "context gauge must be rendered for projected context_window: {text:?}"
    );
    assert!(
        text.contains("glm-5.3 @opencode-go"),
        "identity must be rendered: {text:?}"
    );
}

#[test]
fn model_bar_renders_unavailable_model_indicator() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 1);
    terminal.draw(|f| {
        draw_model_bar(
            f,
            Rect::new(0, 0, 80, 1),
            ModelBarProps {
                current_model: "old-delisted-model",
                model_available: false,
                provider_name: Some("glm-cn"),
                ..Default::default()
            },
            &theme,
            &crate::keymap::GlobalOverrides::default(),
        );
    });
    let buf = terminal.buffer();
    let text = (0..buf.area().width as usize)
        .map(|x| buf.content[x].symbol().to_string())
        .collect::<String>();
    assert!(
        text.contains("old-delisted-model [unavailable]"),
        "row was {text:?}"
    );
}

#[test]
fn model_bar_click_rects_follow_context_and_connection_layout() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 1);

    let mut captured = ModelBarRects::default();
    terminal.draw(|f| {
        captured = draw_model_bar(
            f,
            Rect::new(0, 0, 80, 1),
            ModelBarProps {
                current_model: "kimi-k2.7-code",
                provider_name: None,
                ..Default::default()
            },
            &theme,
            &crate::keymap::GlobalOverrides::default(),
        );
    });
    let ctx = captured.context.expect("context rect present");
    let conn = captured.connection.expect("connection rect present");
    assert!(
        ctx.x + ctx.width <= conn.x,
        "context meter must sit left of the connection segment"
    );
    // The gauges anchor the row's left edge: the context rect starts at
    // the inner indent, one cell in.
    assert_eq!(ctx.x, 1, "gauges must lead the row from the left indent");
    // Rects carry their gauge segment text; the context gauge
    // renders without unbound keycap.
    let buf = terminal.buffer();
    let slice = |r: Rect| -> String {
        (r.x..r.x + r.width)
            .map(|x| buf[(x, r.y)].symbol().to_string())
            .collect::<String>()
    };
    assert_eq!(slice(ctx), "0 (0%)", "context rect mismatch");
    assert_eq!(
        slice(conn),
        "kimi-k2.7-code",
        "connection rect mismatch"
    );
    // The identity cluster sits right of the gauges, pinned to the row's
    // right edge (one trailing indent cell).
    let row: String = (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect();
    let model_pos = row.find("kimi-k2.7-code").expect("model on the row");
    assert!(
        ctx.x + ctx.width <= model_pos as u16,
        "identity must sit right of the context gauge: {row:?}"
    );
    assert_eq!(
        &row[80 - 1 - "kimi-k2.7-code".len()..80 - 1],
        "kimi-k2.7-code",
        "model must end at the right indent: {row:?}"
    );
}

#[test]
fn model_bar_reasoning_tag_shows_effort_when_set() {
    // Render the full model row for three effort states and read back the
    // whole line: the bare `{effort}` tag must appear right after the
    // model name when reasoning is in use and be absent entirely
    // otherwise.
    fn row_text(effort: Option<&str>) -> String {
        let mut terminal = nuotc::TestTerminal::new(80, 1);
        terminal.draw(|f| {
            draw_model_bar(
                f,
                Rect::new(0, 0, 80, 1),
                ModelBarProps {
                    current_model: "mock",
                    reasoning_effort: effort,
                    ..Default::default()
                },
                &Theme::default(),
                &crate::keymap::GlobalOverrides::default(),
            );
        });
        let buf = terminal.buffer();
        (0..buf.area().width as usize)
            .map(|x| buf.content[x].symbol().to_string())
            .collect::<String>()
            .trim()
            .to_string()
    }

    // No reasoning → no effort word anywhere on the row.
    let off = row_text(None);
    assert!(!off.contains("high"), "effort leaked in: {off:?}");
    assert!(!off.contains('◆'), "no diamond glyph in: {off:?}");
    // Reasoning on → bare `high` appears after the model name.
    let on = row_text(Some("high"));
    assert!(on.contains("high"), "missing effort tag in: {on:?}");
    let model_pos = on.find("mock").expect("model name on the row");
    let effort_pos = on.find("high").expect("effort tag");
    assert!(model_pos < effort_pos, "effort must follow the model name");
    // A different effort level renders its own value, not a hardcoded one.
    assert!(row_text(Some("max")).contains("max"));
}

#[test]
fn model_bar_shows_the_instance_suffix_after_the_model_name() {
    // The `@<instance>` suffix must trail the model name so identical
    // models served by different instances stay attributable — and must
    // vanish entirely when no instance is known.
    fn row_text(provider_name: Option<&str>) -> String {
        let mut terminal = nuotc::TestTerminal::new(80, 1);
        terminal.draw(|f| {
            draw_model_bar(
                f,
                Rect::new(0, 0, 80, 1),
                ModelBarProps {
                    current_model: "mock",
                    provider_name,
                    ..Default::default()
                },
                &Theme::default(),
                &crate::keymap::GlobalOverrides::default(),
            );
        });
        let buf = terminal.buffer();
        (0..buf.area().width as usize)
            .map(|x| buf.content[x].symbol().to_string())
            .collect::<String>()
            .trim()
            .to_string()
    }

    let named = row_text(Some("kimi-code"));
    assert!(
        named.contains("@kimi-code"),
        "missing @instance in: {named:?}"
    );
    // The suffix is the last segment of the identity group:
    // `model effort @instance`.
    let model_pos = named.find("mock").expect("model name on the row");
    let inst_pos = named.find("@kimi-code").expect("instance suffix");
    assert!(model_pos < inst_pos, "instance must follow the model name");
    // Unknown / empty instance → no `@` anywhere on the row.
    assert!(!row_text(None).contains('@'));
    assert!(!row_text(Some("")).contains('@'));
}

#[test]
fn model_bar_full_cluster_orders_model_effort_instance() {
    // The right cluster reads `Kimi K3 max @kimi-code` — effort tight
    // after the model name, the @instance provenance last. The identity
    // group (`model effort @instance`) joins with single spaces; it sits
    // across the wider gap from the left-anchored gauges.
    let mut terminal = nuotc::TestTerminal::new(120, 1);
    terminal.draw(|f| {
        draw_model_bar(
            f,
            Rect::new(0, 0, 120, 1),
            ModelBarProps {
                current_model: "mock",
                provider_name: Some("kimi-code"),
                reasoning_effort: Some("max"),
                ..Default::default()
            },
            &Theme::default(),
            &crate::keymap::GlobalOverrides::default(),
        );
    });
    let buf = terminal.buffer();
    let text = (0..buf.area().width as usize)
        .map(|x| buf.content[x].symbol().to_string())
        .collect::<String>();
    let model_pos = text.find("mock").expect("model name");
    let effort_pos = text.find("max").expect("effort");
    let inst_pos = text.find("@kimi-code").expect("instance suffix");
    assert!(
        model_pos < effort_pos && effort_pos < inst_pos,
        "expected `model effort @instance` order in: {text:?}"
    );
    assert!(
        text.contains("mock max @kimi-code"),
        "identity group should join with single spaces in: {text:?}"
    );
}

/// Paint the completion menu into a test buffer and return the rect the
/// popup actually occupied (found by scanning for the popup background),
/// so assertions can check alignment and full-width highlighting without
/// duplicating the layout math.
fn paint_completion_menu(
    input_anchor_x: u16,
    selected: Option<usize>,
) -> (nuotc::TestTerminal, Rect) {
    let theme = Theme::default();
    let completions = vec![
        crate::completion::Completion {
            label: "/repeat".to_string(),
            description: "Schedule a prompt on a cron".to_string(),
            insert_text: "/repeat".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::completion::CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        },
        crate::completion::Completion {
            label: "/permissions".to_string(),
            description: "Manage permissions".to_string(),
            insert_text: "/permissions".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::completion::CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        },
    ];
    let mut terminal = nuotc::TestTerminal::new(80, 12);
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
    assert_eq!(at_anchor.bg, body, "popup body must start at the anchor");
    assert_eq!(at_anchor.symbol(), "/");
    let left_of_anchor = buf.get(1, y).expect("cell left of anchor");
    assert_ne!(
        left_of_anchor.bg, body,
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
            let bg = buf.get(x, y).map(|c| c.bg);
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
            buf.get(x, y).map(|c| c.bg),
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
            buf.get(x, second_row).map(|c| c.bg),
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
            "/unattended",
            "Toggle unattended mode — agent runs without human intervention (on/off)",
        ),
    ]
    .iter()
    .map(|(l, d)| crate::completion::Completion {
        label: l.to_string(),
        description: d.to_string(),
        insert_text: l.to_string(),
        replace_start: 0,
        replace_end: 1,
        kind: crate::completion::CompletionItemKind::Slash,
        alias_of: None,
        doc: None,
    })
    .collect::<Vec<_>>();
    let mut terminal = nuotc::TestTerminal::new(80, 12);
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
        .filter(|&x| buf.get(x, y).map(|c| c.bg) == Some(body))
        .collect();
    let (first, last) = (*cells.first().unwrap(), *cells.last().unwrap());
    assert_eq!(first, 2, "popup must stay anchored at the typed token");
    assert!(
        (last - first + 1) as usize <= 80 * 3 / 5,
        "popup must not fill the viewport: {first}..={last}"
    );
    let row_text: String = (first..=last)
        .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
        .collect();
    assert!(row_text.starts_with("/unattended"), "row was {row_text:?}");
}

#[test]
fn completion_menu_renders_compact_entry_list_without_inline_descriptions() {
    let (terminal, popup) = paint_completion_menu(2, None);
    let buf = terminal.buffer();
    let row_text = |y: u16| -> String {
        (0..buf.area().width)
            .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
            .collect()
    };
    let first = row_text(popup.y);
    // Pure command entry in the left menu: no inline description text or separator
    assert!(first.contains("/repeat"), "row was {first:?}");
    assert!(
        !first.contains("Schedule a prompt on a cron"),
        "inline description should not appear in candidate list: {first:?}"
    );
    assert!(!first.contains('·'), "row was {first:?}");
}

#[test]
fn completion_menu_marks_alias_rows_with_canonical_target() {
    // An alias candidate is marked with [*] in the menu list.
    let theme = Theme::default();
    let completions = vec![
        crate::completion::Completion {
            label: "/unattended".to_string(),
            description: "Toggle unattended mode".to_string(),
            insert_text: "/unattended".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::completion::CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        },
        crate::completion::Completion {
            label: "/auto".to_string(),
            description: "Toggle unattended mode".to_string(),
            insert_text: "/unattended".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::completion::CompletionItemKind::SlashAlias,
            alias_of: Some("/unattended".to_string()),
            doc: None,
        },
    ];
    let mut terminal = nuotc::TestTerminal::new(80, 12);
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
            .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
            .collect()
    };
    let alias_row = row_text(9); // popup bottom row = second candidate
    assert!(
        alias_row.trim_start().starts_with("/auto [*]"),
        "alias shows [*] marker: {alias_row:?}"
    );
    let canonical_row = row_text(8);
    assert!(
        canonical_row.trim_start().starts_with("/unattended"),
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
    let doc = crate::completion::CommandDoc {
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
    let completions = vec![crate::completion::Completion {
        label: "/schedule".to_string(),
        description: "Schedule a prompt".to_string(),
        insert_text: "/schedule".to_string(),
        replace_start: 0,
        replace_end: 2,
        kind: crate::completion::CompletionItemKind::Slash,
        alias_of: None,
        doc: Some(doc),
    }];

    // 1. Unselected (selected = None): No right-side doc window is rendered
    let mut term_unselected = nuotc::TestTerminal::new(80, 12);
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
        (0..80u16).any(|x| buf_unselected.get(x, 9).map(|c| c.bg) == Some(panel_bg));
    assert!(
        !has_panel_unselected,
        "unselected completion must not render hover doc flyout"
    );

    // 2. Selected (selected = Some(0)): Right-side doc window is rendered with panel_bg
    let mut term_selected = nuotc::TestTerminal::new(80, 12);
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
        (0..80u16).any(|x| buf_selected.get(x, 9).map(|c| c.bg) == Some(panel_bg));
    assert!(
        has_panel_selected,
        "selected completion must render hover doc flyout with panel bg"
    );
}

#[test]
fn completion_menu_hover_doc_flyout_shows_alias_to_target_header() {
    let theme = Theme::default();
    let doc = crate::completion::CommandDoc {
        name: "/unattended".to_string(),
        summary: "Toggle unattended mode".to_string(),
        usage: vec!["/unattended".to_string()],
        category: Some("Agent".to_string()),
        subcommands: vec![],
    };
    let completions = vec![crate::completion::Completion {
        label: "/auto".to_string(),
        description: "Toggle unattended mode".to_string(),
        insert_text: "/unattended".to_string(),
        replace_start: 0,
        replace_end: 2,
        kind: crate::completion::CompletionItemKind::SlashAlias,
        alias_of: Some("/unattended".to_string()),
        doc: Some(doc),
    }];

    let mut term = nuotc::TestTerminal::new(80, 12);
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
            .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
            .collect()
    };
    // Check that the flyout header contains "/auto -> /unattended"
    let full_text: Vec<String> = (0..12).map(row_text).collect();
    let found_header = full_text.iter().any(|r| r.contains("/auto -> /unattended"));
    assert!(
        found_header,
        "flyout header should show `/auto -> /unattended`, got buffer:\n{}",
        full_text.join("\n")
    );
}

/// Read back the one-row bar as joined text for assertion.
fn queue_row_text(props: QueueBarProps<'_>, width: u16, theme: &Theme) -> String {
    let mut terminal = nuotc::TestTerminal::new(width, 1);
    terminal.draw(|f| {
        draw_queue_bar(f, Rect::new(0, 0, width, 1), props, theme);
    });
    let buf = terminal.buffer();
    let mut out = String::new();
    for x in 0..width as usize {
        out.push_str(buf.content[x].symbol());
    }
    out.push('\n');
    out
}

#[test]
fn queue_bar_leads_with_brand_tag_on_a_plain_surface() {
    // Matching the todo bar: the `FOLLOW-UPS` tag leads at the gutter in the
    // brand accent on the plain frame surface — no tray glyph, no raised
    // tint — so the two bars read as one quiet family.
    let theme = Theme::default();
    let item = QueueItemProps {
        queued_at_ms: 1_700_000_000_000,
        text: "fix the flaky test".to_string(),
    };
    let mut terminal = nuotc::TestTerminal::new(70, 1);
    terminal.draw(|f| {
        draw_queue_bar(
            f,
            Rect::new(0, 0, 70, 1),
            QueueBarProps {
                items: &[item],
                paused: false,
                blocked: false,
                expand_key: Some(crate::keymap::Key::CTRL_Q),
            },
            &theme,
        );
    });
    let cells = terminal.buffer().content.clone();

    // (1) The tag leads at the gutter, brand-colored.
    assert_eq!(cells[0].symbol(), "F", "expected 'FOLLOW-UPS' tag at col 0");
    assert_eq!(
        cells[0].fg(),
        theme.brand(),
        "FOLLOW-UPS tag not brand-colored"
    );

    // (2) The bar sits on the plain surface: no raised tint anywhere
    // (sample the row's trailing cell too).
    assert_eq!(cells[0].bg(), Color::Reset, "tag must not sit on a tint");
    assert_eq!(cells[69].bg(), Color::Reset, "the row must stay plain");
}

#[test]
fn queue_bar_legend_renders_the_resolved_chord_and_nothing_when_unbound() {
    // The legend is a promise: it renders the chord the registry resolves for
    // the expand command (ADR-0238), so a remap shows through…
    let item = QueueItemProps {
        queued_at_ms: 1_700_000_000_000,
        text: "fix the flaky test".to_string(),
    };
    let remapped = queue_row_text(
        QueueBarProps {
            items: std::slice::from_ref(&item),
            paused: false,
            blocked: false,
            expand_key: Some(crate::keymap::Key::ctrl('e')),
        },
        70,
        &Theme::default(),
    );
    assert!(remapped.contains("Ctrl-e"), "remap must show: {remapped:?}");
    assert!(
        !remapped.contains("Ctrl-q"),
        "the literal chord must not be hardcoded: {remapped:?}"
    );

    // …and a command with no binding renders no keycap at all, rather than
    // advertising a chord that resolves to nothing (the defect this guards).
    let unbound = queue_row_text(
        QueueBarProps {
            items: std::slice::from_ref(&item),
            paused: false,
            blocked: false,
            expand_key: None,
        },
        70,
        &Theme::default(),
    );
    assert!(
        unbound.contains("FOLLOW-UPS 1"),
        "identity survives: {unbound:?}"
    );
    assert!(
        !unbound.contains("Ctrl-") && !unbound.contains("expand"),
        "an unbound affordance must not be advertised: {unbound:?}"
    );

    // The default registry binding is the documented Ctrl-q expand.
    let default_binding = queue_row_text(
        QueueBarProps {
            items: &[item],
            paused: false,
            blocked: false,
            expand_key: Some(crate::keymap::Key::CTRL_Q),
        },
        70,
        &Theme::default(),
    );
    assert!(
        default_binding.contains("Ctrl-q expand"),
        "row was {default_binding:?}"
    );
}

#[test]
fn queue_bar_empty_state_hints_how_to_stage() {
    let text = queue_row_text(
        QueueBarProps {
            items: &[],
            paused: false,
            blocked: false,
            expand_key: Some(crate::keymap::Key::CTRL_Q),
        },
        70,
        &Theme::default(),
    );
    // Identity + zero count on the single row; no time label anymore.
    assert!(text.contains("FOLLOW-UPS 0"), "row was {text:?}");
    assert!(!text.contains("--:--"), "time label leaked: {text:?}");
    // The layout hides an empty queue, so the bar renders no hint for it.
    assert!(!text.contains("queue empty"), "empty hint leaked: {text:?}");
}

#[test]
fn queue_bar_previews_next_item_with_count_and_text() {
    let item = QueueItemProps {
        queued_at_ms: 1_700_000_000_000,
        text: "fix the flaky test in parser".to_string(),
    };
    let text = queue_row_text(
        QueueBarProps {
            items: &[item],
            paused: true,
            blocked: false,
            expand_key: Some(crate::keymap::Key::CTRL_Q),
        },
        92,
        &Theme::default(),
    );
    // Identity + count reflects the one item; no time label anymore.
    assert!(text.contains("FOLLOW-UPS 1"), "row was {text:?}");
    assert!(!text.contains(":"), "time label leaked: {text:?}");
    // Legend: the keycap unit is same-rank peers (R2) — joined by plain
    // whitespace, never a `·`. `Ctrl-p` no longer rides the top-level bar
    // (it now opens the Command Palette); the only bar affordance is
    // `Ctrl-q expand`, since the block toggle lives inside the queue panel.
    assert!(
        text.contains("Ctrl-q expand"),
        "expand affordance missing: {text:?}"
    );
    assert!(
        !text.contains("Ctrl-p"),
        "top-level block toggle moved into the queue panel: {text:?}"
    );
    assert!(!text.contains('·'), "no R1 dot between peers: {text:?}");
    // A live insert is transcript-owned (ADR-0126) and never rides the
    // bar, so every bar item previews plainly — no `steer›` badge.
    assert!(!text.contains("steer›"), "steer badge leaked: {text:?}");
    // The preview rides inline on the same row.
    assert!(
        text.contains("fix the flaky test"),
        "preview text missing: {text:?}"
    );
}

#[test]
fn queue_bar_never_renders_the_tab_affordance() {
    // The Tab toggle for the insert/next-round send target was removed —
    // a busy Enter always queues for the next round — so the queue bar's
    // legend must never mention Tab.
    let item = QueueItemProps {
        queued_at_ms: 1_700_000_000_000,
        text: "add a comment".to_string(),
    };
    let text = queue_row_text(
        QueueBarProps {
            items: &[item],
            paused: false,
            blocked: false,
            expand_key: Some(crate::keymap::Key::CTRL_Q),
        },
        70,
        &Theme::default(),
    );
    assert!(!text.contains("Tab"), "tab legend leaked: {text:?}");
    // A non-steering item never wears the mid-round `steer›` badge.
    assert!(!text.contains("steer›"), "steer badge leaked: {text:?}");
}
