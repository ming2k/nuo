//! The sovereign notice center dialog (ADR-0134).
//!
//! # Architecture & Scope Contract
//!
//! Replaces the legacy footer dock (`NoticeDock`, ADR-0121) with a sovereign,
//! client-global modal review dialog (`DialogKind::Notices`, ADR-0134).
//!
//! Operates on the Two-Track Notice Architecture:
//! - Track 1: Ephemeral interaction signals (copy receipts, armed confirm gates)
//!   render as transient top-right bubbles.
//! - Track 2: Retained operational notices (warnings, faults, retries) land in
//!   `SurfaceLog` and are reviewed in this sovereign card-stack dialog.
//!
//! The dialog presents notifications as distinct, formatted visual cards with:
//! - Severity conveyed by a **color band** ([ADR-0139](docs/adr/0139-severity-band-language-color-only-severity-on-notice-cards.md)):
//!   a derived background tint + tone foreground on the topic pill — no `▲`/`✗`/
//!   `✓`/`ℹ` glyphs and no `[ERROR]`/`[WARN]`/`[OK]`/`[INFO]` text badges.
//! - Topic pill indicators and right-aligned timestamp metadata.
//! - Prominent headline titles with active-selection accent markers (`▌`).
//! - Expandable detailed technical context (HTTP response bodies, stack traces).
//! - Spatial separation between cards and selectable text body (`render_selectable_body`).
//! - Zero footer displacement, ensuring 100% composer stability.

use graticule::{Frame, Modifier, Rect, Style};
use unicode_width::UnicodeWidthStr;

use super::common::one_line;
use crate::shell::keymap::Key;
use crate::shell::render::Theme;
use crate::shell::surfaces::NoticesDialog;
use nuo_ui_primitives::elevation::modal_frame;
use nuo_ui_primitives::primitives::{
    ContentModalSpec, FooterHint, HeaderPart, content_modal_area_in, keyvocab, modal_header_parts,
    render_modal_footer,
};
use nuo_view::components::selectable_body::{RowSegment, SelectableRow, render_selectable_body};
use nuo_view::model::layout::LayoutMap;
use nuo_view::model::log::{LineStatus, SurfaceLine};
use nuo_view::model::selection::SelectionState;

/// Draw the sovereign notice center modal dialog with an authentic card-stack layout.
pub fn draw_notices_modal(
    frame: &mut Frame,
    area: Rect,
    lines: &[SurfaceLine],
    dialog: &mut NoticesDialog,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> Rect {
    let geometry = ContentModalSpec::TOOLS;
    let bounds = content_modal_area_in(area, geometry, 24);
    let f = modal_frame(frame, bounds, theme, true, true);

    let faults = lines.iter().filter(|l| l.status.is_fault()).count();
    let meta = if lines.is_empty() {
        String::new()
    } else {
        let card_idx = (dialog.index + 1).min(lines.len());
        if faults > 0 {
            format!(
                "  {card_idx}/{}  {faults} fault{}",
                lines.len(),
                if faults == 1 { "" } else { "s" }
            )
        } else {
            format!("  {card_idx}/{}", lines.len())
        }
    };
    if meta.is_empty() {
        modal_header_parts(frame, f.header, &[HeaderPart::title("Notices")], theme);
    } else {
        modal_header_parts(
            frame,
            f.header,
            &[
                HeaderPart::title("Notices"),
                HeaderPart::Text {
                    text: &meta,
                    accent: false,
                },
            ],
            theme,
        );
    }

    let body_rect = f.body;
    let body_width = body_rect.width as usize;
    dialog.body_height = body_rect.height;

    // Clamp cursor index into bounds
    if lines.is_empty() {
        dialog.index = 0;
        dialog.detail = false;
    } else if dialog.index >= lines.len() {
        dialog.index = lines.len().saturating_sub(1);
    }

    let mut rows: Vec<SelectableRow> = Vec::new();
    let mut selected_row_idx: Option<usize> = None;

    if lines.is_empty() {
        rows.push(SelectableRow::from_segments(vec![]));
        rows.push(SelectableRow {
            prefix: Some(RowSegment::styled("   ", Style::default())),
            hang_prefix: None,
            segments: vec![RowSegment::styled(
                "No operational notices or diagnostic faults recorded.",
                Style::default()
                    .fg(theme.muted())
                    .add_modifier(Modifier::BOLD),
            )],
        });
        rows.push(SelectableRow {
            prefix: Some(RowSegment::styled("   ", Style::default())),
            hang_prefix: None,
            segments: vec![RowSegment::styled(
                "Operational warnings, retries, and errors land here automatically.",
                Style::default().fg(theme.dim()),
            )],
        });
    } else {
        for (i, notice) in lines.iter().enumerate() {
            let is_sel = dialog.index == i;
            if is_sel {
                selected_row_idx = Some(rows.len());
            }

            // Severity drives the band (ADR-0139): a derived tint pair
            // (background wash + tone foreground) replaces the legacy
            // `[ERROR]`/`[WARN]`/`[OK]`/`[INFO]` text badges and the `▲`/`✗`/
            // `✓`/`ℹ` glyphs. Color carries severity alone on the card — except
            // on a monochrome/vt100 profile, where the band carries a textual
            // marker instead, since color there carries nothing.
            let (accent, marker) = match notice.status {
                LineStatus::Failed => (theme.err(), "error"),
                LineStatus::Warn => (theme.warn(), "warn"),
                LineStatus::Ok => (theme.ok(), "ok"),
                LineStatus::Info => (theme.info(), "info"),
            };
            let band = theme.severity_band(accent, marker);

            let time = nuo_ui_primitives::time::sent_time_label(notice.at_ms);
            let topic = notice.label.as_deref().unwrap_or("system");

            let select_indicator = if is_sel { "▌ " } else { "  " };
            let select_style = if is_sel {
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            // 1. Card Header: [TOPIC PILL (severity band)] ... HH:MM
            let pill = if band.marker.is_empty() {
                format!(" {topic} ")
            } else {
                format!(" {topic} {} ", band.marker)
            };
            let header_w = 2 + pill.width();
            let right_pad = body_width
                .saturating_sub(header_w + time.width() + 1)
                .max(1);
            rows.push(SelectableRow {
                prefix: Some(RowSegment::styled(select_indicator, select_style)),
                hang_prefix: None,
                segments: vec![
                    RowSegment::styled(
                        pill,
                        Style::default()
                            .fg(band.fg)
                            .bg(band.bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    RowSegment::styled(" ".repeat(right_pad), Style::default()),
                    RowSegment::styled(time, Style::default().fg(theme.dim())),
                ],
            });

            // 2. Card Headline: Headline text with selection accent
            let headline = one_line(&notice.text);
            let headline_prefix = if is_sel { "▌   " } else { "    " };
            rows.push(SelectableRow {
                prefix: Some(RowSegment::styled(headline_prefix, select_style)),
                hang_prefix: Some(RowSegment::styled(headline_prefix, select_style)),
                segments: vec![RowSegment::styled(
                    headline,
                    Style::default()
                        .fg(if is_sel { theme.fg() } else { theme.muted() })
                        .add_modifier(if is_sel {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                )],
            });

            // 3. Card Expandable Technical Detail (when selected and expanded)
            if is_sel && dialog.detail {
                rows.push(SelectableRow {
                    prefix: Some(RowSegment::styled(select_indicator, select_style)),
                    hang_prefix: None,
                    segments: vec![],
                });
                for line in notice.text.lines() {
                    rows.push(SelectableRow {
                        prefix: Some(RowSegment::styled(headline_prefix, select_style)),
                        hang_prefix: Some(RowSegment::styled(headline_prefix, select_style)),
                        segments: vec![RowSegment::styled(
                            line.to_string(),
                            Style::default().fg(theme.fg()),
                        )],
                    });
                }
                if let Some(detail) = notice.detail.as_deref() {
                    for line in detail.lines() {
                        rows.push(SelectableRow {
                            prefix: Some(RowSegment::styled(headline_prefix, select_style)),
                            hang_prefix: Some(RowSegment::styled(headline_prefix, select_style)),
                            segments: vec![RowSegment::styled(
                                line.to_string(),
                                Style::default().fg(theme.muted()),
                            )],
                        });
                    }
                }
            }

            // 4. Spatial gap between cards (no divider line)
            if i + 1 < lines.len() {
                rows.push(SelectableRow::from_segments(vec![]));
            }
        }
    }

    let follow = selected_row_idx.map(|idx| {
        rows[..idx]
            .iter()
            .map(|r| r.visual_row_count(body_width))
            .sum()
    });
    render_selectable_body(
        frame,
        body_rect,
        &rows,
        &mut dialog.scroll,
        follow,
        theme,
        selection,
        layout_map,
    );

    // Modal footer hints
    let hints: &[FooterHint] = if lines.is_empty() {
        &[FooterHint::always(Key::ESC.display(), "close")]
    } else {
        &[
            FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
            FooterHint::primary(
                Key::ENTER.display(),
                if dialog.detail { "collapse" } else { "expand" },
            ),
            FooterHint::secondary("c", "copy"),
            FooterHint::secondary("x", "clear"),
            FooterHint::always(Key::ESC.display(), "close"),
        ]
    };
    if let Some(footer) = f.footer {
        render_modal_footer(frame, footer, hints, theme);
    }

    bounds
}

#[cfg(test)]
mod tests;
