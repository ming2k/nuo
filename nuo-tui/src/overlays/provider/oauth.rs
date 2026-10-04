//! The OAuth in-progress sheet (browser authorization prompt and verification code).

use nuotc::{
    Frame, {Modifier, Style},
};

use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{
    ContentModalSpec, FooterHint, content_modal_area, content_modal_probe, hierarchical_breadcrumb,
    modal_chrome_rows, modal_frame, modal_header_parts, render_modal_footer,
};
use crate::render::Theme;

/// Draw the OAuth-in-progress sheet: instruction, URL, optional user code, status.
#[allow(clippy::too_many_arguments)]
pub fn draw_oauth_pending(
    title: &str,
    message: &str,
    url: &str,
    user_code: &str,
    error: Option<&str>,
    _selected_item: usize,
    frame: &mut Frame,
    theme: &Theme,
    scroll: &mut usize,
    hit_map: Option<&mut crate::ui::ComponentTree>,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> nuotc::Rect {
    let geometry = ContentModalSpec::OAUTH_PENDING;
    let probe = content_modal_probe(frame, geometry);
    let probe_w = (probe.width as usize).saturating_sub(6).max(20);

    let mut raw_lines: Vec<(String, Style)> = Vec::new();
    let mut estimated_rows: u16 = 0;

    if let Some(err) = error {
        raw_lines.push((
            format!("✗ {err}"),
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD),
        ));
        raw_lines.push((String::new(), Style::default()));
        raw_lines.push((
            "Press Esc to go back and try again.".to_string(),
            Style::default().fg(theme.muted()),
        ));
        estimated_rows += 3;
    } else {
        if !message.is_empty() {
            raw_lines.push((message.to_string(), Style::default().fg(theme.fg())));
            raw_lines.push((String::new(), Style::default()));
            let msg_wrapped = (message.len().saturating_sub(1) / probe_w) as u16;
            estimated_rows += 2 + msg_wrapped;
        }

        if !url.is_empty() {
            raw_lines.push((
                "Open this link in your browser if it did not open automatically:".to_string(),
                Style::default().fg(theme.muted()),
            ));
            raw_lines.push((
                url.to_string(),
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::UNDERLINED),
            ));
            raw_lines.push((String::new(), Style::default()));
            let url_wrapped = (url.len().saturating_sub(1) / probe_w) as u16;
            estimated_rows += 3 + url_wrapped;
        } else {
            raw_lines.push((
                "Starting authorization…".to_string(),
                Style::default().fg(theme.muted()),
            ));
            raw_lines.push((String::new(), Style::default()));
            estimated_rows += 2;
        }

        if !user_code.is_empty() {
            raw_lines.push((
                format!("Verification Code: {user_code}"),
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            ));
            raw_lines.push((String::new(), Style::default()));
            estimated_rows += 2;
        }

        raw_lines.push((
            "Waiting for authorization to complete in browser…".to_string(),
            Style::default().fg(theme.muted()),
        ));
        estimated_rows += 1;
    }

    let desired =
        estimated_rows.max(raw_lines.len() as u16) + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let f = modal_frame(frame, area, theme, true, true);

    if let Some(map) = hit_map {
        map.mount_oauth_modal(area);
        map.mount_oauth_url(f.body);
    }

    if let Some(h) = f.header {
        let parts = hierarchical_breadcrumb(
            &["Connections", "Add preset connection", title],
            h.width as usize,
        );
        modal_header_parts(frame, Some(h), &parts, theme);
    }

    let rows: Vec<SelectableRow> = raw_lines
        .into_iter()
        .map(|(text, style)| SelectableRow::styled(text, style))
        .collect();
    render_selectable_body(
        frame, f.body, &rows, scroll, None, theme, selection, layout_map,
    );

    if let Some(fo) = f.footer {
        let mut hints: Vec<FooterHint> = Vec::new();
        if error.is_none() {
            if !url.is_empty() {
                hints.push(FooterHint::secondary("u", "copy url"));
            }
            if !user_code.is_empty() {
                hints.push(FooterHint::secondary("c", "copy code"));
            }
        }
        hints.push(FooterHint::key_always(crate::keymap::Key::ESC, "cancel"));
        render_modal_footer(frame, fo, &hints, theme);
    }
    area
}
