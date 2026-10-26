//! Floating which-key card for the `C-x` scene namespace (ADR-0023).
//!
//! Rendered in the bottom-right corner while the namespace is armed. The card
//! is generated from pre-formatted key items (such as the namespace's own verb table
//! in `keymap::scene_namespace`). Completely decoupled from View layouts, with
//! zero layout shift.
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

/// An item on the floating which-key guide card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhichKeyItem {
    pub chord: String,
    pub label: std::borrow::Cow<'static, str>,
    pub active: bool,
}

impl WhichKeyItem {
    pub fn new(
        chord: impl Into<String>,
        label: impl Into<std::borrow::Cow<'static, str>>,
        active: bool,
    ) -> Self {
        Self {
            chord: chord.into(),
            label: label.into(),
            active,
        }
    }
}

/// What the leave verb does when a foreground dialog is up: dismiss it.
/// Nothing navigates between Scenes on this path.
pub const CLOSE_OVERLAY_LABEL: &str = "close overlay";
/// What the leave verb does when closing active tab.
pub const CLOSE_TAB_LABEL: &str = "close tab";

/// Resolve what the namespace's leave row should say for the current surface
/// stack. A **dialog** is checked first because it is the visual foreground and
/// is exactly what the chord dismisses.
pub fn close_label_for(has_dialog: bool) -> &'static str {
    if has_dialog {
        CLOSE_OVERLAY_LABEL
    } else {
        CLOSE_TAB_LABEL
    }
}

/// Render the floating namespace guide while the `Ctrl+X` namespace is armed.
pub fn draw_which_key_overlay(
    frame: &mut Frame,
    theme: &Theme,
    armed: bool,
    items: &[WhichKeyItem],
    viewport: Rect,
) {
    if !armed || viewport.width < 38 || viewport.height < 10 || items.is_empty() {
        return;
    }

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
    frame.render_widget(&mut Clear::default(), area);

    // 2. Surface + framing per archetype (ADR-0023). Modern
    //    terminals (`Chromatic`) distinguish the card by its elevated
    //    background alone — a borderless floating pill (the toast's visual
    //    language). `Hybrid` / `Structured` cannot rely on a background delta,
    //    so they keep an explicit thick frame.
    let bg = theme.which_key_bg();
    let fill = Style::default().bg(bg);
    let is_borderless = matches!(
        theme.elevation,
        crate::elevation::ElevationArchetype::Chromatic
    );

    let (content_area, left_pad) = if is_borderless {
        // Fill the entire card rect with the elevated surface first, so the
        // two trailing padding rows are part of the band (Paragraph only
        // paints the rows it has lines for).
        frame.render_widget(&mut RtBlock::default().style(fill), area);
        (area, 1usize)
    } else {
        let mut block = RtBlock::default()
            .borders(Borders::LEFT | Borders::RIGHT | Borders::TOP | Borders::BOTTOM)
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(theme.brand()))
            .style(fill);
        let inner = block.inner(area);
        frame.render_widget(&mut block, area);
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
    for item in items {
        let key_fg = if item.active {
            theme.keycap_accent()
        } else {
            theme.keycap_fg()
        };
        let key_style = Style::default()
            .fg(key_fg)
            .bg(bg)
            .add_modifier(nuotc::Modifier::BOLD);
        let key_span = Span::styled(item.chord.clone(), key_style);
        let gap = match item.chord.len() {
            1 => "   ",
            2 => "  ",
            3 => " ",
            _ => " ",
        };
        let desc_style = theme.keycap_label_style().bg(bg);
        lines.push(Line::from(vec![
            Span::styled(pad.clone(), fill),
            key_span,
            Span::styled(gap.to_string(), fill),
            Span::styled(item.label.as_ref(), desc_style),
        ]));
    }

    let mut paragraph = Paragraph::new(lines).style(fill);
    frame.render_widget(&mut paragraph, content_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_items(close_label: &'static str) -> Vec<WhichKeyItem> {
        vec![
            WhichKeyItem::new("w", close_label, true),
            WhichKeyItem::new("t", "threads", true),
            WhichKeyItem::new("d", "dashboard", true),
            WhichKeyItem::new("s", "settings", true),
            WhichKeyItem::new("p", "command palette", true),
            WhichKeyItem::new("Esc", "cancel", false),
        ]
    }

    fn term_content(terminal: &nuotc::TestTerminal) -> String {
        let buffer = terminal.buffer();
        let (w, h) = buffer.size();
        let mut text = String::new();
        for y in 0..h {
            for x in 0..w {
                text.push_str(buffer.get(x, y).unwrap().symbol.as_str());
            }
        }
        text
    }

    #[test]
    fn which_key_overlay_renders_for_ctrl_x() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        terminal.draw(|f| {
            draw_which_key_overlay(f, &theme, true, &test_items(CLOSE_TAB_LABEL), f.area());
        });
        let content = term_content(&terminal);
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
        assert_eq!(
            theme.elevation,
            crate::elevation::ElevationArchetype::Chromatic
        );
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        terminal.draw(|f| {
            draw_which_key_overlay(f, &theme, true, &test_items(CLOSE_TAB_LABEL), f.area());
        });
        let content = term_content(&terminal);
        assert!(
            !content.contains('┏') && !content.contains('┃') && !content.contains('━'),
            "the modern card must draw no heavy border glyphs: {content}"
        );
        // The card's own cells carry the dedicated elevated which-key background, not the
        // default — that background *is* the separation.
        let buffer = terminal.buffer();
        let mut card_bg = None;
        for y in 0..24 {
            for x in 0..80 {
                let cell = buffer.get(x, y).unwrap();
                if cell.symbol.as_str() == "C" {
                    card_bg = Some(cell.style.bg);
                    break;
                }
            }
            if card_bg.is_some() {
                break;
            }
        }
        assert_eq!(
            card_bg,
            Some(theme.which_key_bg()),
            "card cells paint the dedicated which-key overlay surface"
        );

        // Structured fallback keeps the frame.
        let mono = Theme::monochrome();
        let mut mono_term = nuotc::TestTerminal::new(80, 24);
        mono_term.draw(|f| {
            draw_which_key_overlay(f, &mono, true, &test_items(CLOSE_TAB_LABEL), f.area());
        });
        let mono_content = term_content(&mono_term);
        assert!(
            mono_content.contains("C-x menu"),
            "the framed fallback still renders the namespace title: {mono_content}"
        );
    }

    /// The card spells the resolved action: close overlay when dialog is up, close tab otherwise.
    #[test]
    fn which_key_overlay_resolves_overlay_vs_tab_label() {
        assert_eq!(close_label_for(true), CLOSE_OVERLAY_LABEL);
        assert_eq!(close_label_for(false), CLOSE_TAB_LABEL);

        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        terminal.draw(|f| {
            draw_which_key_overlay(f, &theme, true, &test_items(CLOSE_OVERLAY_LABEL), f.area());
        });
        let content = term_content(&terminal);
        assert!(content.contains("close overlay"), "{content}");
    }

    #[test]
    fn which_key_overlay_silent_when_none() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        terminal.draw(|f| {
            draw_which_key_overlay(f, &theme, false, &test_items(CLOSE_TAB_LABEL), f.area());
        });
        let content = term_content(&terminal);
        assert!(!content.contains("C-x"));
    }

    /// The card renders all provided items cleanly.
    #[test]
    fn which_key_card_renders_all_provided_items() {
        let theme = Theme::default();
        let items = test_items(CLOSE_TAB_LABEL);
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        terminal.draw(|f| {
            draw_which_key_overlay(f, &theme, true, &items, f.area());
        });
        let content = term_content(&terminal);

        for item in &items {
            assert!(
                content.contains(&item.chord),
                "item advertises `{}` but the card omits it: {content}",
                item.chord
            );
            assert!(
                content.contains(item.label.as_ref()),
                "item label `{}` missing from the card: {content}",
                item.label
            );
        }

        assert!(content.contains("cancel"), "the cancel row is the floor");
    }
}
