//! Provider-delete confirm overlay.
//!
//! A small centered confirm panel rendered *on top of* the stage-1 provider
//! picker (`Modal::Connections`) when the user presses `Shift+D` on a custom
//! provider. Unlike the drill-in modals (model editor / custom-provider
//! editor) it does not replace the list — the picker stays visible behind it,
//! dimmed by an extra [`recess_backdrop`] pass so the user can see exactly
//! which provider is about to be destroyed.
//!
//! Keyboard model: two buttons ([`ProviderDeleteChoice::Cancel`] default,
//! [`ProviderDeleteChoice::Delete`]) driven by ←/→/Tab/↑/↓; Enter activates
//! the focused button; Esc / Ctrl+C cancel. All of that is handled in the
//! scene-routed component handler — this module only paints the result.

use nuotc::{
    Alignment, Block as RtBlock, Clear, Color, Constraint, Direction, Frame, Layout, Line,
    Modifier, Paragraph, Rect, Span, Style,
};

use crate::primitives::Recess;
use crate::primitives::{centered_rect_h, modal_frame, recess_backdrop};
use crate::render::Theme;

/// Which button in the confirm overlay holds keyboard focus. Mirrors
/// `crate::app::ProviderDeleteChoice`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderDeleteChoice {
    Cancel,
    Delete,
}

/// Draw the provider-delete confirm overlay.
///
/// `provider_name` is the human label of the provider staged for deletion
/// (rendered in the body so the user sees exactly what Enter-on-Delete will
/// remove); `focus` selects which of the two footer buttons is highlighted.
///
/// Returns the rect of the confirm panel (for hit-testing).
pub fn draw_provider_delete_confirm(
    frame: &mut Frame,
    provider_name: &str,
    focus: ProviderDeleteChoice,
    theme: &Theme,
) -> Rect {
    // Dim the surface behind the overlay — including the provider picker that
    // is already on screen — so the confirm panel reads as the focal layer.
    // `recess_backdrop` darkens in place (it does not clear), so the picker
    // stays visible for context. Done once here, on top of any prior recess.
    recess_backdrop(frame, Recess::Dim, theme);

    // Compact centered panel, sized to its content rather than a fixed slab of
    // the frame. Body = 1 prompt line; the frame adds header + footer rows
    // plus its own inner vertical padding. A modest fixed height keeps the
    // panel compact and visually distinct from the full provider picker.
    let panel_rows: u16 = 9;
    let area = centered_rect_h(48, panel_rows, frame.area());

    let f = modal_frame(frame, area, theme, true, true);

    if let Some(h) = f.header {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Delete connection?",
                Style::default()
                    .fg(theme.warn())
                    .add_modifier(Modifier::BOLD),
            ))),
            h,
        );
    }

    // Body: the provider name in the danger/warn tone so it is unambiguous.
    if f.body.height > 0 {
        let line = Line::from(vec![
            Span::styled("Delete ", Style::default().fg(theme.muted())),
            Span::styled(
                provider_name.to_string(),
                Style::default()
                    .fg(theme.warn())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "? This cannot be undone.",
                Style::default().fg(theme.muted()),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), f.body);
    }

    // Footer: two buttons side by side — Cancel (default-safe) on the left,
    // Delete on the right. The focused button gets an inverted (filled) style.
    if let Some(footer) = f.footer {
        draw_buttons(frame, footer, focus, theme);
    }

    area
}

/// Render the two-button footer. Each button is a half-width cell; the focused
/// one is filled (inverted) so the highlight is visible on any theme.
fn draw_buttons(frame: &mut Frame, area: Rect, focus: ProviderDeleteChoice, theme: &Theme) {
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    let left = halves[0];
    let right = halves[1];

    button(
        frame,
        left,
        " Cancel ",
        focus == ProviderDeleteChoice::Cancel,
        false,
        theme,
    );
    button(
        frame,
        right,
        " Delete ",
        focus == ProviderDeleteChoice::Delete,
        true,
        theme,
    );
}

/// Paint one button cell. `danger` tints the Delete label with the warn tone
/// so the destructive action is visually distinct even when not focused.
fn button(frame: &mut Frame, area: Rect, label: &str, focused: bool, danger: bool, theme: &Theme) {
    let fg = if focused {
        theme.panel()
    } else if danger {
        theme.warn()
    } else {
        theme.fg()
    };
    let bg = if focused {
        if danger { theme.warn() } else { theme.brand() }
    } else {
        Color::Reset
    };
    let style = if focused {
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(fg).bg(bg)
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        RtBlock::default().style(if focused {
            Style::default().bg(bg)
        } else {
            Style::default()
        }),
        area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(label, style))).alignment(Alignment::Center),
        area,
    );
}
