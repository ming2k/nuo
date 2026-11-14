//! The composer ⇄ transcript crossing must be **visible in both directions**
//! (ADR-0145 `[INV-FOCUS-04]`).
//!
//! The regression these tests lock: the composer answered "the keyboard is
//! here" with a bright panel and answered "it left" by recessing that panel, but
//! the transcript answered nothing at all until an entry happened to be
//! highlighted — and even then only with a foreground tint that vanished on an
//! expanded step. `Shift-Tab` therefore had one visible half, and the pane the
//! keyboard arrived in looked exactly like the pane it was not in.
//!
//! The first fix overshot: a tinted rail down both gutter columns plus a
//! full-width band behind the highlighted entry's header. The rail put the cue at
//! the two extreme edges of the pane and nowhere the reader was actually looking;
//! the band had to invert the disclosure and lifecycle tones the renderers had
//! resolved on purpose, so the pointed-at row read as a *highlighted component*
//! rather than as part of a live surface. The design under test states the same
//! two facts at two scales instead — a whole-pane wash, and a one-cell entry
//! marker — and these tests pin both, plus the fact that nothing else moved.
//!
//! Every assertion is on the painted grid rather than on the flags that feed it.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use nuo_view::disclosure::Interaction;
use nuo_view::model::document::{MessageKind, TranscriptMessage};

/// A finished tool step, expanded or not.
fn step(name: &str, expanded: bool) -> TranscriptMessage {
    let mut m = TranscriptMessage::tool_step("call_1", name, r#"{"path":"a.rs"}"#);
    if let MessageKind::ToolStep {
        output,
        expanded: exp,
        status,
        ..
    } = &mut m.kind
    {
        *output = Some("ok".to_string());
        *exp = expanded;
        *status = nuo_view::model::document::ToolStepStatus::Ok;
    }
    m
}

struct Painted {
    terminal: nuotc::TestTerminal,
}

/// Paint the whole transcript surface (footer stack included, so the composer
/// panel is in the same frame) for one keyboard-ownership state.
fn paint(
    messages: &[TranscriptMessage],
    plane: FocusPlane,
    focused_entry: Option<nuo_view::model::layout::InteractiveTarget>,
) -> Painted {
    paint_with_theme(
        &Theme::default(),
        messages,
        plane,
        focused_entry,
        &SelectionState::None,
    )
}

/// Every theme the shipped scheme table exposes, so the painted-grid assertions cover
/// real palettes instead of only `zen`.
///
/// The pane cue is derived from each palette's own channel headroom, so a scheme whose
/// page sits close to its bands gets a *different* wash magnitude — and the question
/// "does the document survive the lift" has a different answer on each. Covering one
/// palette would leave the derivation untested exactly where it does the most work.
fn shipped_schemes() -> Vec<Theme> {
    let custom = nuo_ui_primitives::color_scheme::ColorSchemeConfig::default();
    nuo_ui_primitives::theme::COLOR_SCHEMES
        .iter()
        .map(|scheme| Theme::from_color_scheme(&scheme.id, &custom))
        .collect()
}

/// Paint one frame with a caller-supplied theme and selection.
fn paint_with_theme(
    theme: &Theme,
    messages: &[TranscriptMessage],
    plane: FocusPlane,
    focused_entry: Option<nuo_view::model::layout::InteractiveTarget>,
    selection: &SelectionState,
) -> Painted {
    let theme = *theme;
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    terminal.draw(|f| {
        let _ = draw_transcript(
            f,
            &mut LayoutMap::new(),
            TranscriptProps {
                messages,
                scroll: 0,
                selection,
                cell_selection: None,
                backoff: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "draft",
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
                focused_entry,
                focus_plane: plane,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: nuo_view::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });
    Painted { terminal }
}

/// The page tone a transcript frame paints with for this keyboard state.
///
/// The wash arrives through the **theme** now (ADR-0145, amended), not through a
/// post-pass restyle, so the expected value is the lifted theme's own surface — which
/// is also the faithful statement of the design: the pane's page *is* that tone while
/// the pane owns the keyboard.
fn page_tone(theme: &Theme, plane: FocusPlane) -> nuotc::Color {
    nuo_view::focus_plane::transcript_theme(theme, plane).surface()
}

/// Every background colour present in one row, left to right.
fn row_backgrounds(painted: &Painted, y: u16) -> Vec<nuotc::Color> {
    let width = painted.terminal.buffer().area().width;
    (0..width)
        .map(|x| painted.terminal.buffer().get(x, y).unwrap().style.bg)
        .collect()
}

/// Rows whose **whole** width carries one background tone — i.e. an unbroken
/// surface, as opposed to a row with content bands sitting on the page.
fn rows_painted_entirely(painted: &Painted, tone: nuotc::Color) -> Vec<u16> {
    let height = painted.terminal.buffer().area().height;
    (0..height)
        .filter(|&y| row_backgrounds(painted, y).iter().all(|bg| *bg == tone))
        .collect()
}

/// `Shift-Tab` must change something the user can *see* on the side the
/// keyboard arrives at — not only on the side it leaves.
///
/// The primary assertion is that the **pane surface itself** changes: the
/// transcript's page tone is what moves, so the cue is present everywhere the
/// reader looks rather than at the pane's edges.
#[test]
fn crossing_lifts_the_whole_transcript_pane() {
    let theme = Theme::default();
    let messages = vec![step("read", false)];

    let composer_plane = paint(&messages, FocusPlane::Composer, None);
    let transcript_plane = paint(&messages, FocusPlane::Transcript, None);

    // Sanity: the two tones are genuinely different surfaces.
    let focused_page = page_tone(&theme, FocusPlane::Transcript);
    assert_ne!(focused_page, theme.surface());

    let washed = rows_painted_entirely(&transcript_plane, focused_page);
    assert!(
        washed.len() >= 5,
        "the transcript pane must lift its whole surface while it owns the \
         keyboard (found {} fully-washed rows)",
        washed.len()
    );
    let unwound = rows_painted_entirely(&composer_plane, focused_page);
    assert_eq!(
        unwound,
        Vec::<u16>::new(),
        "the wash is a transcript-plane state, never permanent decoration"
    );
    assert!(
        !rows_painted_entirely(&composer_plane, theme.surface()).is_empty(),
        "on the composer plane the transcript stays on the page tone"
    );
}

/// The wash must reach the **interior** of the pane, not just its margins: the
/// old cue tinted the two gutter columns and left the middle of the viewport
/// exactly as it was, so the cue lived where nobody was reading.
#[test]
fn wash_reaches_the_column_the_reader_is_reading() {
    let theme = Theme::default();
    let messages = vec![step("read", false)];
    let transcript_plane = paint(&messages, FocusPlane::Transcript, None);

    let y = rows_painted_entirely(&transcript_plane, theme.pane_focus())[0];
    let width = transcript_plane.terminal.buffer().area().width;
    // The middle of the row is the text column, not a margin.
    for x in [2, width / 2, width - 3] {
        assert_eq!(
            transcript_plane
                .terminal
                .buffer()
                .get(x, y)
                .unwrap()
                .style
                .bg,
            theme.pane_focus(),
            "column {x} of row {y} must be part of the washed pane"
        );
    }
}

/// Content bands inside the pane keep their **elevation**, which is the property
/// the pane cue has to preserve.
///
/// An earlier version of this test asserted that a code band's background was
/// *exactly* `code_surface()` after the wash, and that assertion was the bug: it
/// locked the band's absolute tone, so an implementation that raised the page to
/// meet the band passed while eating more than half of the elevation — the
/// document flattened into a single slab. What matters is the step above the page,
/// and the lift is applied additively for exactly that reason.
#[test]
fn wash_preserves_the_relative_step_of_a_content_band() {
    let theme = Theme::default();
    let messages = vec![step("read", true)];
    let lum = |c: nuotc::Color| -> f32 {
        match c {
            nuotc::Color::Rgb(r, g, b) => 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32,
            _ => 0.0,
        }
    };

    // Locate the code band in the **blurred** frame, where its tone is the token
    // itself, then sample the same cells in the focused frame.
    let blurred = paint(&messages, FocusPlane::Composer, None);
    let focused = paint(&messages, FocusPlane::Transcript, None);
    let code = theme.code_surface();
    let mut sampled = 0usize;
    for y in 0..blurred.terminal.buffer().area().height {
        for x in 0..blurred.terminal.buffer().area().width {
            if blurred.terminal.buffer().get(x, y).unwrap().style.bg != code {
                continue;
            }
            sampled += 1;
            let before = lum(code) - lum(theme.surface());
            let after = lum(focused.terminal.buffer().get(x, y).unwrap().style.bg)
                - lum(page_tone(&theme, FocusPlane::Transcript));
            assert!(
                (after - before).abs() < 1.5,
                "({x},{y}): the code band must keep its {before:.1}-unit elevation \
                 (now {after:.1})"
            );
        }
    }
    assert!(sampled > 0, "an expanded read step must render a code band");
}

/// A **sent-message panel** inside the transcript must keep its elevation after
/// the page is lifted.
///
/// This is the regression the first implementation shipped: it repainted only the
/// cells still on the page tone, which preserved every band's *absolute* colour and
/// therefore looked correct in the unit test, while the page rose to meet the panels
/// and swallowed most of their elevation. Same sampling discipline as the code-band
/// test — find the panel where its tone is nameable, then re-read those cells.
#[test]
fn wash_keeps_the_sent_message_panel_raised_above_the_page() {
    let theme = Theme::default();
    let messages = vec![
        TranscriptMessage::new(nuo_protocol::Role::User, "please refactor this")
            .with_round(1)
            .with_sent_at_ms(1_700_000_000_000),
        step("read", false),
    ];
    let lum = |c: nuotc::Color| -> f32 {
        match c {
            nuotc::Color::Rgb(r, g, b) => 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32,
            _ => 0.0,
        }
    };

    let blurred = paint(&messages, FocusPlane::Composer, None);
    let focused = paint(&messages, FocusPlane::Transcript, None);
    let panel = theme.user_surface();
    let mut sampled = 0usize;
    for y in 0..blurred.terminal.buffer().area().height {
        for x in 0..blurred.terminal.buffer().area().width {
            if blurred.terminal.buffer().get(x, y).unwrap().style.bg != panel {
                continue;
            }
            sampled += 1;
            let before = lum(panel) - lum(theme.surface());
            let after = lum(focused.terminal.buffer().get(x, y).unwrap().style.bg)
                - lum(page_tone(&theme, FocusPlane::Transcript));
            assert!(
                (after - before).abs() < 1.5,
                "({x},{y}): the sent-message panel must keep its {before:.1}-unit \
                 elevation above the page (now {after:.1})"
            );
        }
    }
    assert!(sampled > 0, "a user message must paint its panel surface");

    // And the pane really did move — otherwise the assertion above is vacuous.
    assert!(
        lum(page_tone(&theme, FocusPlane::Transcript)) > lum(theme.surface()),
        "the wash must actually lift the page"
    );
}

/// The crossing is visible even with **no entry highlighted**, which is exactly
/// where `Shift-Tab` lands — and that visibility must not come from tinting a
/// row the keyboard is not on.
#[test]
fn browse_focus_paints_no_entry_marker() {
    let theme = Theme::default();
    let messages = vec![step("read", false)];
    let transcript_plane = paint(&messages, FocusPlane::Transcript, None);

    let marker = theme.pane_marker_style();
    let baked = (0..transcript_plane.terminal.buffer().area().height).any(|y| {
        (0..transcript_plane.terminal.buffer().area().width)
            .any(|x| transcript_plane.terminal.buffer().get(x, y).unwrap().style == marker)
    });
    assert!(
        !baked,
        "browse focus must not mark an arbitrary entry: nothing is highlighted"
    );
}

/// The highlighted entry gets exactly one **marker cell**, in its reserved
/// gutter — not a band under its text.
///
/// The band was the ugly half of the first design: it had to invert the running
/// hue and the muted rung the row had deliberately resolved, and it turned the
/// row into a highlighted component on a sheet that otherwise had no focus cue
/// at all. The marker says "this row" without touching a single cell of it.
#[test]
fn focused_entry_is_marked_with_one_gutter_cell() {
    let theme = Theme::default();
    let messages = vec![step("read", false)];
    let target = nuo_view::model::layout::InteractiveTarget::tool_step(0);

    let composer_plane = paint(&messages, FocusPlane::Composer, Some(target));
    let transcript_plane = paint(&messages, FocusPlane::Transcript, Some(target));

    let marker = theme.pane_marker_style();
    let marker_rows = |p: &Painted| -> Vec<(u16, u16)> {
        let height = p.terminal.buffer().area().height;
        let width = p.terminal.buffer().area().width;
        let mut cells = Vec::new();
        for y in 0..height {
            for x in 0..width {
                if p.terminal.buffer().get(x, y).unwrap().style == marker {
                    cells.push((x, y));
                }
            }
        }
        cells
    };

    assert_eq!(
        marker_rows(&composer_plane),
        Vec::new(),
        "the composer plane must not mark a transcript entry"
    );
    let cells = marker_rows(&transcript_plane);
    assert_eq!(
        cells.len(),
        1,
        "the marker is one cell, not a row: {cells:?}"
    );

    // It sits in the reserved gutter, immediately beside the content band.
    let (x, _) = cells[0];
    assert!(
        x < TRANSCRIPT_H_INSET,
        "the marker belongs in the pane gutter (x={x}), never over content"
    );
}

/// The row the marker names keeps its own tone: keyboard ownership must not
/// recolour a summary.
///
/// This is the invariant that the band violated. A running step is the sharpest
/// case — its accent is the in-flight hue, and the band used to blend it toward
/// the primary foreground so it could be read against the band.
#[test]
fn marking_an_entry_does_not_recolour_its_summary() {
    let theme = Theme::default();
    let messages = vec![step("read", true)];
    let target = nuo_view::model::layout::InteractiveTarget::tool_step(0);

    let composer_plane = paint(&messages, FocusPlane::Composer, Some(target));
    let transcript_plane = paint(&messages, FocusPlane::Transcript, Some(target));

    // The marker's row is the entry's header, and it is the row the cue exists
    // for. Its glyphs and their tones must be identical on both planes: focus may
    // add a cell, it may not recolor one.
    let marker = theme.pane_marker_style();
    let header_y = (0..transcript_plane.terminal.buffer().area().height)
        .find(|&y| {
            (0..transcript_plane.terminal.buffer().area().width)
                .any(|x| transcript_plane.terminal.buffer().get(x, y).unwrap().style == marker)
        })
        .expect("the focused entry must be marked");

    // Content columns must be identical on both planes: focus adds a gutter marker,
    // it may not recolor the summary.
    let summary_cells = |p: &Painted, y: u16| -> Vec<(String, nuotc::Color, nuotc::Modifier)> {
        (TRANSCRIPT_H_INSET..p.terminal.buffer().area().width)
            .map(|x| p.terminal.buffer().get(x, y).unwrap())
            // Background is *expected* to differ (that is the pane cue). Only the
            // glyph and its foreground are under test.
            .map(|c| (c.symbol.to_string(), c.style.fg, c.style.add))
            .collect()
    };
    assert_eq!(
        summary_cells(&composer_plane, header_y),
        summary_cells(&transcript_plane, header_y),
        "the pane crossing must not recolor the focused entry's header row"
    );
    // The gutter column carries the marker in the transcript plane only.
    assert_eq!(
        transcript_plane
            .terminal
            .buffer()
            .get(TRANSCRIPT_H_INSET - 1, header_y)
            .unwrap()
            .symbol,
        theme.pane_marker_glyph()
    );
    assert_eq!(
        composer_plane
            .terminal
            .buffer()
            .get(TRANSCRIPT_H_INSET - 1, header_y)
            .unwrap()
            .symbol,
        " "
    );
}

/// An **expanded** entry keeps its marker. The old affordance-hue cue was
/// suppressed exactly here — on the state where inspection actually happens — so
/// the plane the user was reading from had no focus signal at all.
#[test]
fn expanded_entry_keeps_its_marker() {
    let theme = Theme::default();
    let messages = vec![step("read", true)];
    let target = nuo_view::model::layout::InteractiveTarget::tool_step(0);

    let transcript_plane = paint(&messages, FocusPlane::Transcript, Some(target));
    let marker = theme.pane_marker_style();
    let found = (0..transcript_plane.terminal.buffer().area().height).any(|y| {
        (0..transcript_plane.terminal.buffer().area().width)
            .any(|x| transcript_plane.terminal.buffer().get(x, y).unwrap().style == marker)
    });
    assert!(
        found,
        "an expanded focused entry must still be marked: keyboard ownership is \
         not a disclosure fact"
    );
}

/// The two planes must speak in the **same** channel: the composer's panel and
/// the transcript's pane are both background surfaces, and both are visibly off
/// their blurred state. Asserted together because the whole defect was that only
/// one of them was.
#[test]
fn both_planes_announce_ownership_in_the_background_channel() {
    let theme = Theme::default();
    let messages = vec![step("read", false)];

    // The composer half: its panel is the active input surface while it owns the
    // keyboard, and the recessed one the instant a crossing takes it away.
    let panel_at = |focused: bool| -> nuotc::Color {
        let mut terminal = nuotc::TestTerminal::new(30, 6);
        terminal.draw(|f| {
            crate::shell::composer::draw_composer(
                crate::shell::composer::ComposerProps {
                    frame: f,
                    input_rect: Rect::new(0, 0, 30, 4),
                    theme: &theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: &mut 0,
                    selection: &SelectionState::None,
                },
                crate::shell::composer::ComposerText {
                    input: "draft",
                    byte_cursor: 5,
                },
                focused,
                focused,
                false,
                0,
                0,
                crate::shell::composer_hints::ComposerHints::default(),
            );
        });
        terminal.buffer().get(0, 0).unwrap().style.bg
    };
    let lit = panel_at(true);
    let receded = panel_at(false);
    assert_eq!(
        lit,
        theme.input_surface(),
        "the composer panel is the active input surface while it owns the keyboard"
    );
    assert_ne!(lit, theme.surface(), "and it is visibly off the page");
    assert_eq!(receded, theme.input_surface_inactive());
    assert_ne!(receded, lit, "the composer's two states must differ");

    // The transcript half: the same channel, the same kind of state change.
    let transcript = paint(&messages, FocusPlane::Transcript, None);
    let focused_page = page_tone(&theme, FocusPlane::Transcript);
    let washed = rows_painted_entirely(&transcript, focused_page);
    assert!(
        !washed.is_empty(),
        "the transcript's pane must be visibly off the page, like the composer's panel"
    );

    // Neither plane's lit state may be mistakable for the other's ambient one.
    assert_ne!(focused_page, receded);
    assert_ne!(focused_page, theme.surface());
}

/// A row that is **both focused and hovered** still answers the pointer.
///
/// The regression: `Interaction` used to resolve focus *over* hover into a single
/// variant, so hovering the entry the keyboard was already on produced no cue at
/// all. Clicking a summary focuses it and leaves the pointer sitting on it — that
/// is the ordinary way a row is reached, and the row went visually dead the instant
/// it did.
#[test]
fn a_focused_row_under_the_pointer_still_shows_the_hover_cue() {
    let theme = Theme::default();
    let messages = vec![step("read", false)];
    let target = nuo_view::model::layout::InteractiveTarget::tool_step(0);

    let summary_fg = |painted: &Painted| -> nuotc::Color {
        let b = painted.terminal.buffer();
        for y in 0..b.area().height {
            for x in 0..b.area().width {
                if b.get(x, y).unwrap().symbol == "+" {
                    return b.get(x, y).unwrap().style.fg;
                }
            }
        }
        panic!("no collapsed-step marker painted");
    };

    // `hovered_step` is the frame's pointer answer, `focused_entry` the keyboard's.
    let paint_with = |hovered: Option<usize>| -> Painted {
        let mut terminal = nuotc::TestTerminal::new(80, 24);
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
                    input: "draft",
                    chrome_hidden: false,
                    queue_bar: QueueBarProps {
                        items: &[],
                        blocked: false,
                        expand_key: None,
                    },
                    tasks_bar: Default::default(),
                    interrupt_key: None,
                    subagent_bar: None,
                    page_hints: None,
                    tab_bar: None,
                    thread_id: None,
                    thread_context: None,
                    round_started_at: None,
                    hovered_step: hovered,
                    focused_entry: Some(target),
                    focus_plane: FocusPlane::Transcript,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: nuo_view::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        Painted { terminal }
    };

    let focused_only = summary_fg(&paint_with(None));
    let focused_and_hovered = summary_fg(&paint_with(Some(0)));
    assert_ne!(
        focused_only, focused_and_hovered,
        "a focused row under the pointer must still show the hover cue, not just the marker"
    );
}

/// The whole painted-grid contract holds on **every shipped scheme**, not just `zen`.
///
/// Each palette derives its own wash magnitude from its own headroom, so this is where
/// the derivation is actually exercised: a scheme with little room gets a smaller wash,
/// and a `Reset`-surface scheme gets no wash at all and must still announce the plane.
/// Asserting this on one palette would leave the interesting half untested.
#[test]
fn every_shipped_scheme_announces_the_transcript_plane() {
    let messages = vec![
        TranscriptMessage::new(nuo_protocol::Role::User, "selectable prose for the pane")
            .with_round(1),
        step("read", true),
    ];
    let target = nuo_view::model::layout::InteractiveTarget::tool_step(1);
    let selection = SelectionState::Range {
        anchor: nuo_view::model::layout::SemanticCursor::new(0, 0, 0),
        head: nuo_view::model::layout::SemanticCursor::new(0, 0, 26),
    };
    let lum = |c: nuotc::Color| -> f32 {
        match c {
            nuotc::Color::Rgb(r, g, b) => 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32,
            _ => 0.0,
        }
    };

    for theme in shipped_schemes() {
        let label = format!("{}", theme.elevation as u8);
        let blurred = paint_with_theme(
            &theme,
            &messages,
            FocusPlane::Composer,
            Some(target),
            &selection,
        );
        let focused = paint_with_theme(
            &theme,
            &messages,
            FocusPlane::Transcript,
            Some(target),
            &selection,
        );

        // 1. The pane moved, and the marker is present.
        let marker = theme.pane_marker_style();
        let marked = (0..focused.terminal.buffer().area().height).any(|y| {
            (0..focused.terminal.buffer().area().width)
                .any(|x| focused.terminal.buffer().get(x, y).unwrap().style == marker)
        });
        assert!(marked, "scheme {label}: the focused entry must be marked");

        if !theme.has_pane_wash() {
            // No wash: the gutter fallback must be there instead, so the plane is
            // still announced in a background channel.
            let edge = theme.pane_focus_edge_style();
            let gutter = (0..focused.terminal.buffer().area().height).any(|y| {
                (0..2).any(|x| focused.terminal.buffer().get(x, y).unwrap().style == edge)
            });
            assert!(
                gutter,
                "scheme {label}: a wash-less palette still needs a cue"
            );
            continue;
        }

        // 2. The page moved, and every band kept its separation from it.
        let page = page_tone(&theme, FocusPlane::Transcript);
        assert_ne!(page, theme.surface(), "scheme {label}: the page must lift");

        // Only cells **inside the transcript pane** are shifted: the head band and the
        // footer stack are painted from the unlifted theme, so they legitimately keep
        // the page tone even while the pane is lifted.
        let pane = super::transcript_band_rect(nuotc::Rect::new(
            0,
            0,
            blurred.terminal.buffer().area().width,
            blurred.terminal.buffer().area().height,
        ));
        let mut sampled = 0usize;
        for y in pane.y..pane.bottom() {
            for x in pane.x..pane.right() {
                let before = blurred.terminal.buffer().get(x, y).unwrap().style.bg;
                // Only cells the lift must carry: the page and its bands.
                if !theme.pane_bands().contains(&before) {
                    continue;
                }
                sampled += 1;
                let after = focused.terminal.buffer().get(x, y).unwrap().style.bg;
                if before == theme.surface() {
                    // The page itself, not a band: it must move, and that is asserted
                    // above.
                    continue;
                }
                let before_step = lum(before) - lum(theme.surface());
                let after_step = lum(after) - lum(page);
                assert!(
                    (after_step - before_step).abs() < 1.5,
                    "scheme {label} ({x},{y}): a band's elevation changed \
                     {before_step:.1} -> {after_step:.1}"
                );
            }
        }
        assert!(
            sampled > 0,
            "scheme {label}: the fixture must paint at least one pane surface"
        );
    }
}

/// Focus is a **background state + one marker**, not a hue tint.
///
/// The last remaining way to reintroduce the original asymmetry is to smuggle a
/// foreground cue back into the interaction channel. That channel is hover's.
#[test]
fn focus_contributes_no_tint() {
    let theme = Theme::default();
    assert_eq!(Interaction::Focused.color(&theme), None);
    assert_eq!(Interaction::Hovered.color(&theme), Some(theme.affordance()));
}

/// End-to-end closure check: a **selection** inside a focused transcript keeps its
/// separation from the page.
///
/// Selection was the last surface I had only argued about rather than measured. It is
/// shifted with everything else (the lift is a translation, so its delta is
/// invariant), and this samples a real selected cell in both planes to prove the
/// argument holds through the actual render path.
#[test]
fn a_selection_keeps_its_separation_under_the_pane_wash() {
    let theme = Theme::default();
    // A user message with real selectable prose: the point is the *selection band*,
    // and a step's chrome carries no selectable text to select.
    let messages = vec![
        TranscriptMessage::new(
            nuo_protocol::Role::User,
            "please refactor the selection rendering path",
        )
        .with_round(1),
    ];
    let target = nuo_view::model::layout::InteractiveTarget::tool_step(0);
    let lum = |c: nuotc::Color| -> f32 {
        match c {
            nuotc::Color::Rgb(r, g, b) => 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32,
            _ => 0.0,
        }
    };

    let selection = SelectionState::Range {
        anchor: nuo_view::model::layout::SemanticCursor::new(0, 0, 0),
        head: nuo_view::model::layout::SemanticCursor::new(0, 0, 44),
    };
    let render = |plane: FocusPlane| -> nuotc::TestTerminal {
        let mut terminal = nuotc::TestTerminal::new(80, 28);
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptProps {
                    messages: &messages,
                    scroll: 0,
                    selection: &selection,
                    cell_selection: None,
                    backoff: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "draft",
                    chrome_hidden: false,
                    queue_bar: QueueBarProps {
                        items: &[],
                        blocked: false,
                        expand_key: None,
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
                    focused_entry: Some(target),
                    focus_plane: plane,
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
    };

    let blurred = render(FocusPlane::Composer);
    let focused = render(FocusPlane::Transcript);
    let selected = theme.selected();

    // Locate the selection band where its tone is nameable (the blurred frame), then
    // read the same cells in the focused frame.
    let mut sampled = 0usize;
    for y in 0..blurred.buffer().area().height {
        for x in 0..blurred.buffer().area().width {
            if blurred.buffer().get(x, y).unwrap().style.bg != selected {
                continue;
            }
            sampled += 1;
            let before = lum(selected) - lum(theme.surface());
            let after = lum(focused.buffer().get(x, y).unwrap().style.bg)
                - lum(page_tone(&theme, FocusPlane::Transcript));
            assert!(
                (after - before).abs() < 1.5,
                "({x},{y}): the selection band must keep its {before:.1}-unit separation \
                 above the page (now {after:.1})"
            );
        }
    }
    assert!(sampled > 0, "a block selection must paint its band");
}
