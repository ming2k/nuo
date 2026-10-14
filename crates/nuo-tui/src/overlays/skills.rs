//! Skills modal — a centered, dismissable overlay listing every loaded skill.
//!
//! Opened via the `/skills` slash command (intercepted locally in `input.rs`,
//! never sent to the backend). Each row shows the skill name, a short hint, and
//! its status (enabled / disabled / quarantined). `Enter` toggles a detail
//! expansion (full description, version, source, tags, and quarantine action if
//! unverified).

use nuotc::{
    Frame, Style, {Line, Span},
};

use super::common::{placeholder, selectable_row};
use crate::primitives::{
    ContentModalSpec, FooterHint, SCROLL_EDGE_MARGIN, content_modal_area, content_modal_probe,
    keyvocab, modal_chrome_rows, modal_frame, modal_header, render_body, render_modal_footer,
};
use crate::render::Theme;

/// Draw the skills modal.
///
/// `session_context` provides `skills: Vec<SkillInfo>`. `modal_index` is the row
/// cursor; `expanded` is the index of the row whose detail block is shown (or
/// `None`). `scroll` is read AND written back so the caller's offset stays
/// consistent with the clamped body height.
pub fn draw_skills_modal(
    frame: &mut Frame,
    session_context: Option<&nuo_wire::SessionContextSnapshot>,
    modal_index: usize,
    expanded: Option<usize>,
    scroll: &mut usize,
    theme: &Theme,
) -> nuotc::Rect {
    let geometry = ContentModalSpec::SKILLS;
    let probe = content_modal_probe(frame, geometry);
    let body_w = (probe.width as usize)
        .saturating_sub(2 * crate::design::MODAL_INNER_H_PADDING as usize)
        .max(20);

    // Body: the skill list with optional detail expansion
    let skills = session_context.map(|s| s.skills.as_slice()).unwrap_or(&[]);
    let mut body: Vec<Line> = Vec::new();

    if skills.is_empty() {
        body.push(placeholder(
            "No skills loaded.",
            session_context.is_some(),
            theme.muted(),
        ));
    } else {
        for (i, skill) in skills.iter().enumerate() {
            let (state_badge, mark_enabled) = if skill.quarantined {
                ("quarantined", false)
            } else if skill.enabled {
                ("enabled", true)
            } else {
                ("disabled", false)
            };

            // The selectable row: name + description hint + status badge.
            body.push(selectable_row(
                i,
                modal_index,
                &skill.name,
                &skill.description,
                mark_enabled,
                state_badge,
                state_badge,
                body_w,
                theme,
            ));

            // Detail expansion for the selected row.
            if expanded == Some(i) {
                let detail_indent = "    ";
                let muted = Style::default().fg(theme.muted());
                let fg = Style::default().fg(theme.fg());

                if skill.quarantined {
                    let warn = Style::default().fg(theme.warn());
                    body.push(Line::from(Span::styled(
                        format!(
                            "{}[Quarantined] Project skill requires authorization.",
                            detail_indent
                        ),
                        warn,
                    )));
                    body.push(Line::from(Span::styled(
                        format!(
                            "{}Action: Run `/trust skills` or `/trust` to enable.",
                            detail_indent
                        ),
                        warn,
                    )));
                    body.push(Line::from(""));
                }

                // Full description (may be long — that's the point of the
                // detail view; the row hint is truncated, this is not).
                for line in skill.description.lines() {
                    body.push(Line::from(Span::styled(
                        format!("{}{}", detail_indent, line),
                        fg,
                    )));
                }

                // Metadata line: version + source + tags — same-rank peers
                // (R2), so they join with plain whitespace, no dot.
                let mut meta_parts: Vec<String> = Vec::new();
                if let Some(v) = &skill.version {
                    meta_parts.push(format!("v{}", v));
                }
                meta_parts.push(skill.source.clone());
                if !skill.tags.is_empty() {
                    meta_parts.push(format!("#{}", skill.tags.join(" #")));
                }
                body.push(Line::from(Span::styled(
                    format!(
                        "{}{}",
                        detail_indent,
                        meta_parts.join(&" ".repeat(crate::design::JOIN_ENUMERATE_COLS))
                    ),
                    muted,
                )));

                body.push(Line::from(""));
            }
        }
    }

    let desired = (body.len() as u16) + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let f = modal_frame(frame, area, theme, true, true);

    // Header
    modal_header(frame, f.header, "Skills", theme);

    let follow = if skills.is_empty() {
        None
    } else {
        Some(modal_index)
    };
    render_body(
        frame,
        f.body,
        body,
        scroll,
        crate::primitives::BodyRenderOptions::new(follow, SCROLL_EDGE_MARGIN, false),
        theme,
    );

    // Footer
    if let Some(fo) = f.footer {
        let hints: &[FooterHint] = if skills.is_empty() {
            &[FooterHint::key_always(crate::keymap::Key::ESC, "close")]
        } else {
            &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "select"),
                FooterHint::key_primary(crate::keymap::Key::ENTER, "detail"),
                FooterHint::key_always(crate::keymap::Key::ESC, "close"),
            ]
        };
        render_modal_footer(frame, fo, hints, theme);
    }

    // Return the panel rect so the event loop can register it as the
    // click-outside-to-dismiss target (the modal is in
    // `Modal::dismissable_by_outside_click`).
    area
}
