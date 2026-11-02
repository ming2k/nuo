//! The composer ⇄ transcript crossing must be **visible in both directions**
//! (ADR-0145).
//!
//! The regression these tests lock: the composer answered "the keyboard is
//! here" with a bright panel and answered "it left" by recessing that panel, but
//! the transcript answered nothing at all until an entry happened to be
//! highlighted — and even then only with a foreground tint that vanished on an
//! expanded step. `Shift-Tab` therefore had one visible half, and the pane the
//! keyboard arrived in looked exactly like the pane it was not in.
//!
//! Both halves are painted state, so both are asserted on the painted grid
//! rather than on the flags that feed it.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::shell::render::FocusPlane;
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
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);
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

/// Every background colour present in one row, left to right.
fn row_backgrounds(painted: &Painted, y: u16) -> Vec<nuotc::Color> {
    let width = painted.terminal.buffer().area().width;
    (0..width)
        .map(|x| painted.terminal.buffer().get(x, y).unwrap().style.bg)
        .collect()
}

/// The first row painted as a **focus band** — the band tone across most of the
/// row. Both the rail (a few gutter columns) and the band use the same tone, so
/// the two are told apart by extent rather than by colour: the rail frames the
/// pane, the band owns a row.
fn band_row(painted: &Painted, theme: &Theme) -> Option<u16> {
    let width = painted.terminal.buffer().area().width as usize;
    let height = painted.terminal.buffer().area().height;
    (0..height).find(|&y| {
        row_backgrounds(painted, y)
            .iter()
            .filter(|bg| **bg == theme.entry_focus_band())
            .count()
            * 2
            > width
    })
}

/// `Shift-Tab` must change something the user can *see* on the side the
/// keyboard arrives at — not only on the side it leaves.
///
/// This is the primary regression: an entry must paint a full-width band while
/// the transcript plane owns the keyboard, and the same entry must paint the
/// ambient page while the composer does.
#[test]
fn crossing_to_the_transcript_paints_a_band_on_the_focused_entry() {
    let theme = Theme::default();
    let messages = vec![step("read_text", false)];
    let target = nuo_view::model::layout::InteractiveTarget::tool_step(0);

    let composer_plane = paint(&messages, FocusPlane::Composer, Some(target));
    let transcript_plane = paint(&messages, FocusPlane::Transcript, Some(target));

    assert!(
        band_row(&composer_plane, &theme).is_none(),
        "the composer plane must not paint a transcript focus band"
    );
    let y = band_row(&transcript_plane, &theme)
        .expect("the transcript plane must paint the focused entry's band");

    // The band spans the row: a band that stops mid-row reads as a ragged
    // highlight rather than as "the keyboard is on this entry".
    let band_cols = row_backgrounds(&transcript_plane, y)
        .into_iter()
        .filter(|bg| *bg == theme.entry_focus_band())
        .count();
    let width = transcript_plane.terminal.buffer().area().width as usize;
    assert!(
        band_cols * 2 > width,
        "the focus band must dominate the header row ({band_cols}/{width} columns)"
    );
}

/// The crossing is visible even with **no entry highlighted**, which is exactly
/// where `Shift-Tab` lands: the pane paints a focus rail in its left gutter.
/// Without it the primary gesture had nothing to show for itself.
#[test]
fn crossing_to_a_browsing_transcript_paints_the_pane_rail() {
    let theme = Theme::default();
    let messages = vec![step("read_text", false)];

    let composer_plane = paint(&messages, FocusPlane::Composer, None);
    let transcript_plane = paint(&messages, FocusPlane::Transcript, None);

    assert!(
        band_row(&composer_plane, &theme).is_none(),
        "no entry is highlighted, so no band"
    );
    // The rail is one column; find it by scanning for the focus tone in a
    // gutter column that is blank on the composer plane.
    let rail_tone = theme.entry_focus_rail_style().bg;
    let height = transcript_plane.terminal.buffer().area().height;
    let rail_rows = (0..height)
        .filter(|&y| row_backgrounds(&transcript_plane, y).contains(&rail_tone))
        .count();
    assert!(
        rail_rows >= 5,
        "a browsing transcript must paint the pane rail down the viewport \
         (found {rail_rows} rows)"
    );
    let composer_rail_rows = (0..height)
        .filter(|&y| row_backgrounds(&composer_plane, y).contains(&rail_tone))
        .count();
    assert_eq!(
        composer_rail_rows, 0,
        "the rail is a transcript-plane state, never permanent decoration"
    );
}

/// An **expanded** entry's band must be as visible as a collapsed one's. The old
/// cue was suppressed on expanded steps, which is where inspection actually
/// happens — so the plane the user was reading from was the one with no focus
/// signal at all.
#[test]
fn expanded_entry_keeps_the_focus_band() {
    let theme = Theme::default();
    let messages = vec![step("read_text", true)];
    let target = nuo_view::model::layout::InteractiveTarget::tool_step(0);

    let transcript_plane = paint(&messages, FocusPlane::Transcript, Some(target));
    let y = band_row(&transcript_plane, &theme)
        .expect("an expanded focused entry must still paint its band");
    let band_cols = row_backgrounds(&transcript_plane, y)
        .into_iter()
        .filter(|bg| *bg == theme.entry_focus_band())
        .count();
    assert!(
        band_cols > 1,
        "an expanded entry's band must be a real band, not a token column"
    );
}

/// The two planes must speak in the **same** channel at the **same** weight: the
/// composer's panel and the entry's band are both background fills, and both are
/// visibly off the page. Asserted together because the whole defect was that
/// only one of them was.
#[test]
fn both_planes_announce_ownership_in_the_background_channel() {
    let theme = Theme::default();
    let messages = vec![step("read_text", false)];
    let target = nuo_view::model::layout::InteractiveTarget::tool_step(0);

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

    // The transcript half: the same channel, the same weight.
    let transcript = paint(&messages, FocusPlane::Transcript, Some(target));
    let y = band_row(&transcript, &theme).unwrap();
    let band_bg = row_backgrounds(&transcript, y)
        .into_iter()
        .find(|bg| *bg == theme.entry_focus_band())
        .unwrap();
    assert_ne!(
        band_bg,
        theme.surface(),
        "the transcript's band must be visibly off the page, like the composer's panel"
    );

    // Neither plane's lit state may be mistakable for the other's ambient one.
    assert_ne!(band_bg, receded);
}
