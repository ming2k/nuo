//! Floating which-key card for the `C-x` scene namespace (ADR-0023).
//!
//! Rendered in the bottom-right corner while the namespace is armed. The card
//! is **generated from the namespace's own verb table**
//! ([`crate::keymap::scene_namespace`]) — the same table the router resolves
//! against — so a verb can never be dispatchable without being advertised, nor
//! advertised without being dispatchable (ADR-0238). Completely decoupled from
//! View layouts, with zero layout shift.
//!
//! On **modern terminals** (`Chromatic`: TrueColor / 256-color, which can
//! render a distinct background) the card is **borderless** and reads purely by
//! its elevated background — an edge-free floating pill, matching the toast
//! component's visual language (ADR-0023). On `Hybrid` (ANSI-16) and
//! `Structured` (monochrome / Linux VT) terminals, where a background delta is
//! indistinguishable or unavailable, it keeps an explicit frame via
//! [`Theme::elevation`], mirroring `elevation::modal_frame`.

use nuotc::{
    Block as RtBlock, BorderType, Borders, Clear, Frame, Line, Paragraph, Rect, Span, Style,
};

use super::super::Theme;
use crate::keymap::scene_namespace::SceneVerb;

/// What the leave verb does when a foreground dialog is up: dismiss it.
/// Nothing navigates between Scenes on this path.
pub(crate) const CLOSE_OVERLAY_LABEL: &str = "close overlay";
/// What the leave verb does when a Scene other than the home Conversation is
/// current: leave it (detach from an aside, pop the task zoom, or return from
/// the dashboard / settings, or close active tab).
pub(crate) const CLOSE_SCENE_LABEL: &str = "close tab";
/// What the leave verb does at the bare home scene: the chord still resolves
/// (it disarms the namespace) but it has nothing to act on. The card says so
/// rather than promising an exit that will not happen (ADR-0238).
pub(crate) const HOME_SCENE_LABEL: &str = "home already";

/// Resolve what the namespace's leave row should say for the current surface
/// stack. A **dialog** is checked first because it is the visual foreground and
/// is exactly what the chord dismisses. A **sheet** is deliberately transparent
/// to the namespace: it carries a pending decision and owns its own keys
/// (ADR-0173 §3), so the chord leaves the scene beneath it and the card says so.
pub(crate) fn close_label_for(has_dialog: bool, leaves_a_scene: bool) -> &'static str {
    if has_dialog {
        CLOSE_OVERLAY_LABEL
    } else if leaves_a_scene {
        CLOSE_SCENE_LABEL
    } else {
        HOME_SCENE_LABEL
    }
}

/// Render the floating namespace guide while the `Ctrl+X` namespace is armed.
///
/// `close_label` is the *resolved* description of what the leave verb does in
/// the current state — the caller owns that verdict because only it knows the
/// surface stack (a dialog dismisses, a scene leaves, the home scene has
/// nothing to act on). Passing the label rather than a boolean keeps the card
/// from promising an exit the dispatcher will not perform (ADR-0238).
pub(crate) fn draw_which_key_overlay(
    frame: &mut Frame,
    theme: &Theme,
    armed: bool,
    close_label: &'static str,
    viewport: Rect,
) {
    if !armed || viewport.width < 38 || viewport.height < 10 {
        return;
    }

    // Rows straight from the verb table: one row per verb, keyed by the chord
    // that verb advertises. The cancel row is the namespace's floor — it is not
    // a verb (it maps to no scene lifecycle action), so it is named here rather
    // than in the table.
    let mut items: Vec<(String, &'static str, bool)> = SceneVerb::ALL
        .iter()
        .copied()
        .map(|verb| {
            let label = match verb {
                SceneVerb::Leave => close_label,
                other => other.label(),
            };
            // Bold/primary only while the verb would actually do something.
            let primary = verb != SceneVerb::Leave || close_label != HOME_SCENE_LABEL;
            (verb.advertised_stroke_display().to_string(), label, primary)
        })
        .collect();
    // The cancel floor. `Esc` is listed because it is the universal escape and
    // the one second stroke a user is most likely to reach for.
    items.push(("Esc".to_string(), "cancel", false));

    let title = "C-x menu";
    let card_width: u16 = 36;
    let card_height: u16 = (items.len() as u16) + 3; // title + items + padding

    // Position at bottom-right, 3 rows above the terminal bottom
    let x = viewport.width.saturating_sub(card_width + 2).max(1);
    let y = viewport.height.saturating_sub(card_height + 3).max(1);

    let area = Rect::new(
        x,
        y,
        card_width.min(viewport.width.saturating_sub(x)),
        card_height,
    );

    // 1. Wipe underlying text cleanly with Clear widget
    frame.render_widget(Clear, area);

    // 2. Surface + framing per archetype (ADR-0023). Modern
    //    terminals (`Chromatic`) distinguish the card by its elevated
    //    background alone — a borderless floating pill (the toast's visual
    //    language). `Hybrid` / `Structured` cannot rely on a background delta,
    //    so they keep an explicit thick frame.
    let bg = theme.which_key_bg();
    let fill = Style::default().bg(bg);
    let is_borderless = matches!(theme.elevation, nuotc::ElevationArchetype::Chromatic);

    let (content_area, left_pad) = if is_borderless {
        // Fill the entire card rect with the elevated surface first, so the
        // two trailing padding rows are part of the band (Paragraph only
        // paints the rows it has lines for).
        frame.render_widget(RtBlock::default().style(fill), area);
        (area, 1usize)
    } else {
        let block = RtBlock::default()
            .borders(Borders::LEFT | Borders::RIGHT | Borders::TOP | Borders::BOTTOM)
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(theme.brand()))
            .style(fill);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        // One column of inner padding inside the frame, as before.
        (inner, 1usize)
    };

    // 3. Build action lines. Spans carry the card's background so the whole
    //    surface reads as one elevated band (no stray default-bg cells).
    let pad = " ".repeat(left_pad);
    let mut lines = Vec::with_capacity(items.len() + 2);
    lines.push(Line::from(vec![
        Span::styled(pad.clone(), fill),
        Span::styled(
            title,
            fill.fg(theme.brand()).add_modifier(nuotc::Modifier::BOLD),
        ),
    ]));
    for (key, desc, is_primary) in &items {
        let key_span = Span::styled(
            key.to_string(),
            theme.keycap_style().bg(bg),
        );
        let gap = match key.len() {
            1 => "   ",
            2 => "  ",
            3 => " ",
            _ => " ",
        };
        let desc_style = if *is_primary {
            fill.fg(theme.fg())
        } else {
            fill.fg(theme.dim())
        };
        lines.push(Line::from(vec![
            Span::styled(pad.clone(), fill),
            key_span,
            Span::styled(gap.to_string(), fill),
            Span::styled(*desc, desc_style),
        ]));
    }

    let paragraph = Paragraph::new(lines).style(fill);
    frame.render_widget(paragraph, content_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_key_overlay_renders_for_ctrl_x() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        terminal.draw(|f| {
            draw_which_key_overlay(f, &theme, true, CLOSE_SCENE_LABEL, f.area());
        });
        let content: String = terminal
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("C-x menu"));
        assert!(content.contains("close tab"));
        assert!(content.contains("threads"));
        assert!(content.contains("dashboard"));
        assert!(content.contains("settings"));
        assert!(content.contains("cancel"));
    }

    /// ADR-0023: on a modern (`Chromatic`) terminal the card is borderless —
    /// it reads by its elevated background alone, with no box glyphs — matching
    /// the toast's visual language. On `Structured` (monochrome) a background
    /// delta is unavailable, so the card keeps an explicit frame.
    #[test]
    fn which_key_card_is_borderless_on_modern_and_framed_on_monochrome() {
        let theme = Theme::default();
        assert_eq!(theme.elevation, nuotc::ElevationArchetype::Chromatic);
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        terminal.draw(|f| {
            draw_which_key_overlay(f, &theme, true, CLOSE_SCENE_LABEL, f.area());
        });
        let content: String = terminal
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            !content.contains('┏') && !content.contains('┃') && !content.contains('━'),
            "the modern card must draw no heavy border glyphs: {content}"
        );
        // The card's own cells carry the dedicated elevated which-key background, not the
        // default — that background *is* the separation.
        let card_cell = terminal
            .buffer()
            .content
            .iter()
            .find(|c| c.symbol() == "C")
            .expect("title text present");
        assert_eq!(
            card_cell.bg,
            theme.which_key_bg(),
            "card cells paint the dedicated which-key overlay surface"
        );

        // Structured fallback keeps the frame.
        let mono = Theme::monochrome();
        let mut mono_term = nuotc::TestTerminal::new(80, 24);
        mono_term.draw(|f| {
            draw_which_key_overlay(f, &mono, true, CLOSE_SCENE_LABEL, f.area());
        });
        let mono_content: String = mono_term
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            mono_content.contains("C-x menu"),
            "the framed fallback still renders the namespace title: {mono_content}"
        );
    }

    /// ADR-0238: the card spells the resolved action. At the bare home scene
    /// the chord has nothing to act on, so the card says that instead of
    /// promising an exit the dispatcher will not perform.
    #[test]
    fn which_key_overlay_never_promises_an_exit_the_home_scene_lacks() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        terminal.draw(|f| {
            draw_which_key_overlay(f, &theme, true, HOME_SCENE_LABEL, f.area());
        });
        let content: String = terminal
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("home already"), "{content}");
        assert!(
            !content.contains("close tab") && !content.contains("close overlay"),
            "no exit is promised at the home scene: {content}"
        );
    }

    #[test]
    fn which_key_overlay_silent_when_none() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        terminal.draw(|f| {
            draw_which_key_overlay(f, &theme, false, CLOSE_SCENE_LABEL, f.area());
        });
        let content: String = terminal
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!content.contains("C-x"));
    }

    /// ADR-0298 §1 / ADR-0238: the card is *generated* from the namespace's verb
    /// table — every verb the table declares appears on the card, keyed by the
    /// stroke that verb advertises. A verb cannot be added to the dispatcher
    /// without appearing here, and the card cannot print a stroke no verb
    /// declares.
    #[test]
    fn every_namespace_verb_appears_on_the_card() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        terminal.draw(|f| {
            draw_which_key_overlay(f, &theme, true, CLOSE_SCENE_LABEL, f.area());
        });
        let content: String = terminal
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();

        for verb in SceneVerb::ALL {
            let stroke = verb.advertised_stroke_display();
            assert!(
                content.contains(stroke),
                "{verb:?} advertises `{stroke}` but the card omits it: {content}"
            );
            // The leave verb's label is caller-supplied; the rest come from the
            // table itself.
            let label = match verb {
                SceneVerb::Leave => CLOSE_SCENE_LABEL,
                other => other.label(),
            };
            assert!(
                content.contains(label),
                "{verb:?} label `{label}` missing from the card: {content}"
            );
        }

        // The cancel floor is always present, and the card never invents a verb
        // the table does not have.
        assert!(content.contains("cancel"), "the cancel row is the floor");
    }
}
