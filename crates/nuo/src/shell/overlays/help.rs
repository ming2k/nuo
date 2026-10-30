//! The Help & Keybindings modal dialog (ADR-0131 [INV-HELP-01..03]).
use nuotc::{Frame, Line, Modifier, Rect, Span, Style};

use crate::shell::render::Theme;
use crate::shell::surfaces::SceneKind;
use nuo_ui_primitives::elevation::modal_frame;
use nuo_ui_primitives::primitives::{
    ContentModalSpec, FooterHint, content_modal_area, modal_header, render_body,
    render_modal_footer,
};

/// Format one keybinding row with prominent accent keycap and muted description.
fn binding_row(key: &str, desc: &str, theme: &Theme) -> Line<'static> {
    let key_style = Style::default()
        .fg(theme.keycap_accent())
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.fg());

    Line::from(vec![
        Span::styled(format!("  {:<12} ", key), key_style),
        Span::styled(desc.to_string(), desc_style),
    ])
}

/// Section title banner inside the help body.
fn section_header(title: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("── {} ", title),
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "─".repeat(50usize.saturating_sub(title.len() + 4)),
            Style::default().fg(theme.dim()),
        ),
    ])
}

/// Render the sovereign Help dialog.
pub fn draw_help_modal(
    frame: &mut Frame,
    _area: Rect,
    scene: SceneKind,
    viewed_session_id: Option<&str>,
    unread_faults: usize,
    scroll: &mut usize,
    theme: &Theme,
) -> Rect {
    let geometry = ContentModalSpec::TOOLS;
    let bounds = content_modal_area(frame, geometry, 22);
    let f = modal_frame(frame, bounds, theme, true, true);

    modal_header(frame, f.header, "Help & Keybindings", theme);

    let mut body = Vec::new();

    // 1. Current Scene Contextual Actions (ADR-0131 [INV-HELP-01])
    match scene {
        SceneKind::Thread => {
            let session_tag = viewed_session_id
                .map(|id| {
                    let tail = if id.len() <= 4 {
                        id
                    } else {
                        &id[id.len() - 4..]
                    };
                    format!("Thread #{}", tail)
                })
                .unwrap_or_else(|| "Thread (Active Conversation)".to_string());
            body.push(section_header(&session_tag, theme));
            let notice_desc = if unread_faults > 0 {
                format!("Notices & diagnostic faults ({} unread)", unread_faults)
            } else {
                "Notices & diagnostic fault log".to_string()
            };
            body.push(binding_row("Ctrl-O", &notice_desc, theme));
            body.push(binding_row(
                "Alt-T",
                "Context window & token telemetry",
                theme,
            ));
            body.push(binding_row(
                "Alt-C",
                "Model route & connection details",
                theme,
            ));
            body.push(binding_row(
                "Esc",
                "Interrupt running turn / dismiss overlay",
                theme,
            ));
        }
        SceneKind::Dashboard => {
            body.push(section_header("Dashboard (Fleet Orchestration)", theme));
            body.push(binding_row(
                "Enter",
                "Inspect session dossier & inspect details",
                theme,
            ));
            body.push(binding_row("/", "Filter sessions by id or status", theme));
            body.push(binding_row("Esc", "Dismiss active view / cancel", theme));
        }
        SceneKind::Settings => {
            body.push(section_header("Settings & Preferences", theme));
            body.push(binding_row(
                "Enter",
                "Select category or toggle setting",
                theme,
            ));
            body.push(binding_row("Esc", "Exit settings", theme));
        }
        SceneKind::Subagent => {
            body.push(section_header("Subagent Execution View", theme));
            body.push(binding_row("Esc", "Step back to parent thread", theme));
        }
    }
    body.push(Line::from(""));

    // 2. Client Domain: Tabs & Workspaces
    body.push(section_header("Tabs & Workspaces", theme));
    body.push(binding_row("t", "New thread in a new tab", theme));
    body.push(binding_row("w", "Close active tab", theme));
    body.push(binding_row(
        "Alt-[ / ]",
        "Switch to previous / next tab",
        theme,
    ));
    body.push(binding_row(
        "Alt-1..9",
        "Jump directly to tab 1 through 9",
        theme,
    ));
    body.push(Line::from(""));

    // 3. Client Domain: Views & Navigation
    body.push(section_header("Views & Navigation", theme));
    body.push(binding_row("d", "Navigate to Dashboard", theme));
    body.push(binding_row(
        "s",
        "Open saved threads overview picker",
        theme,
    ));
    body.push(binding_row(",", "Open Settings & appearance", theme));
    body.push(Line::from(""));

    // 4. Client Domain: System & Tools
    body.push(section_header("System & Tools", theme));
    body.push(binding_row(
        "p",
        "Open Command Palette (Quick Switcher)",
        theme,
    ));
    body.push(binding_row(
        "/",
        "Slash commands & tool execution in composer",
        theme,
    ));
    body.push(binding_row(
        "F1 / Alt-h",
        "Toggle this Help & Keybindings dialog",
        theme,
    ));

    render_body(
        frame,
        f.body,
        body,
        scroll,
        nuo_ui_primitives::primitives::BodyRenderOptions::new(None, 0, false),
        theme,
    );

    if let Some(fo) = f.footer {
        let hints = [
            FooterHint::primary("Key", "direct action"),
            FooterHint::always("Esc", "close"),
        ];
        render_modal_footer(frame, fo, &hints, theme);
    }

    bounds
}
