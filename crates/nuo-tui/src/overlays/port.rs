//! Port & Network Sharing modal (`C-x p`, ADR-0045).
//!
//! Provides an ephemeral, non-destructive palette to inspect, hot-plug, or
//! disable server TCP listeners without interrupting ongoing prompts or
//! polluting conversation transcripts.

use nuotc::{Frame, Line, Rect, Span, Style};
use crate::primitives::{
    ContentModalSpec, FooterHint, content_modal_area, keyvocab,
    modal_chrome_rows, modal_frame, modal_header, render_body, render_modal_footer,
};
use crate::render::Theme;

#[allow(clippy::too_many_arguments)]
pub fn draw_port_modal(
    frame: &mut Frame,
    port: u16,
    active: bool,
    token: Option<&str>,
    local_path: &str,
    notice: Option<&str>,
    loading: bool,
    theme: &Theme,
) -> Rect {
    let geometry = ContentModalSpec::TOOLS;
    let desired = 8 + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let f = modal_frame(frame, area, theme, true, true);

    modal_header(frame, f.header, "Port & Remote Sharing", theme);

    let mut body: Vec<Line> = Vec::new();
    body.push(Line::from(""));

    if loading {
        body.push(Line::from(vec![
            Span::styled("  Status:       ", Style::default().fg(theme.muted())),
            Span::styled("Synchronizing endpoints with server...", Style::default().fg(theme.brand())),
        ]));
    } else if active {
        body.push(Line::from(vec![
            Span::styled("  TCP Listener: ", Style::default().fg(theme.muted())),
            Span::styled("● Active ", Style::default().fg(theme.success)),
            Span::styled(format!("(port {})", port), Style::default().fg(theme.fg())),
        ]));
        body.push(Line::from(vec![
            Span::styled("  Address:      ", Style::default().fg(theme.muted())),
            Span::styled(format!("ws://127.0.0.1:{}/", port), Style::default().fg(theme.brand())),
        ]));
        if let Some(t) = token {
            let masked = if t.len() > 16 {
                format!("{}...{}", &t[..8], &t[t.len() - 6..])
            } else {
                t.to_string()
            };
            body.push(Line::from(vec![
                Span::styled("  Bearer Token: ", Style::default().fg(theme.muted())),
                Span::styled(masked, Style::default().fg(theme.fg())),
            ]));
            body.push(Line::from(vec![
                Span::styled("  Connect CMD:  ", Style::default().fg(theme.muted())),
                Span::styled(format!("nuo --remote 127.0.0.1:{} --token {}", port, t), Style::default().fg(theme.dim())),
            ]));
        }
    } else {
        body.push(Line::from(vec![
            Span::styled("  TCP Listener: ", Style::default().fg(theme.muted())),
            Span::styled("○ Disabled ", Style::default().fg(theme.dim())),
            Span::styled("(zero-attack-surface local IPC posture)", Style::default().fg(theme.muted())),
        ]));
        let lp_display = if local_path.is_empty() {
            "server.sock (active)".to_string()
        } else {
            local_path.to_string()
        };
        body.push(Line::from(vec![
            Span::styled("  Local IPC:    ", Style::default().fg(theme.muted())),
            Span::styled(lp_display, Style::default().fg(theme.fg())),
        ]));
        body.push(Line::from(vec![
            Span::styled("  Hint:         ", Style::default().fg(theme.muted())),
            Span::styled("Press [Space] to enable remote access on port 9800", Style::default().fg(theme.brand())),
        ]));
    }

    if let Some(msg) = notice {
        body.push(Line::from(""));
        body.push(Line::from(vec![
            Span::styled("  Notice:       ", Style::default().fg(theme.success)),
            Span::styled(msg.to_string(), Style::default().fg(theme.success)),
        ]));
    }

    let mut scroll = 0;
    render_body(
        frame,
        f.body,
        body,
        &mut scroll,
        crate::primitives::BodyRenderOptions::new(None, 0, false),
        theme,
    );

    if let Some(fo) = f.footer {
        let toggle_action = if active { "disable TCP" } else { "enable TCP" };
        let mut hints = vec![
            FooterHint::primary(keyvocab::SPACE, toggle_action),
        ];
        if active {
            hints.push(FooterHint::secondary("y", "copy connect cmd"));
        }
        hints.push(FooterHint::key_always(crate::keymap::Key::ESC, "close"));
        render_modal_footer(frame, fo, &hints, theme);
    }

    area
}
