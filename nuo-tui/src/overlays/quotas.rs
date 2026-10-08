//! Provider Quotas Dashboard overlay (`/quota`, ADR-0036).
//!
//! Type-scoped batch inspection of provider allowances, sliding-window
//! quotas, and multi-account pool balances.

use nuo_wire::{ConnectionUsageState, ProviderQuotaData, ProviderQuotaSnapshot};
use nuotc::{Frame, Line, Modifier, Span, Style};

use super::common::placeholder;
use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::design::MODAL_INNER_H_PADDING;
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{
    ContentModalSpec, FooterHint, HeaderPart, content_modal_area, content_modal_probe, keyvocab,
    modal_chrome_rows, modal_frame, modal_header_parts, render_modal_footer,
};
use crate::render::Theme;

/// Draw the provider quota dashboard. `loading` marks the `QueryProviderQuotas`
/// round-trip in flight. Returns the painted panel rectangle.
#[allow(clippy::too_many_arguments)]
pub fn draw_quotas_modal(
    frame: &mut Frame,
    snapshot: Option<&ProviderQuotaSnapshot>,
    loading: bool,
    selected_index: usize,
    scroll: &mut usize,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> nuotc::Rect {
    let geometry = ContentModalSpec::USAGE_STATS;
    let probe = content_modal_probe(frame, geometry);
    let body_width = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);

    let (header, body, footer) = if loading && snapshot.is_none() {
        (
            vec![HeaderPart::title("Provider Quotas")],
            vec![placeholder("Querying provider quotas…", true, theme.muted())],
            vec![FooterHint::key_always(crate::keymap::Key::ESC, "close")],
        )
    } else if let Some(snap) = snapshot {
        let title = if let Some(ref filter) = snap.provider_filter {
            format!("Provider Quotas [{filter}]")
        } else {
            "Provider Quotas".to_string()
        };
        (
            vec![
                HeaderPart::title(Box::leak(title.into_boxed_str())),
                ],
            quotas_body(snap, selected_index, body_width, theme),
            vec![
                FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
                FooterHint::secondary("Space", "switch account"),
                FooterHint::secondary("r", "refresh"),
                FooterHint::key_always(crate::keymap::Key::ESC, "close"),
            ],
        )
    } else {
        (
            vec![HeaderPart::title("Provider Quotas")],
            vec![placeholder("No quota data available.", false, theme.muted())],
            vec![FooterHint::key_always(crate::keymap::Key::ESC, "close")],
        )
    };

    let desired = body.len() as u16 + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let modal = modal_frame(frame, area, theme, true, true);

    modal_header_parts(frame, modal.header, &header, theme);
    let rows: Vec<SelectableRow> = body.into_iter().map(SelectableRow::from_line).collect();
    render_selectable_body(
        frame, modal.body, &rows, scroll, None, theme, selection, layout_map,
    );
    if let Some(footer_area) = modal.footer {
        render_modal_footer(frame, footer_area, &footer, theme);
    }
    area
}

fn quotas_body(
    snapshot: &ProviderQuotaSnapshot,
    selected_index: usize,
    body_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if snapshot.entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "No quota-capable connections found.",
            Style::default().fg(theme.muted()),
        )));
        return lines;
    }

    for (idx, entry) in snapshot.entries.iter().enumerate() {
        let is_selected = idx == selected_index;
        let prefix = if is_selected { "▶ " } else { "  " };

        let status_color = match &entry.state {
            ConnectionUsageState::Available(_) => {
                if entry
                    .primary_balance
                    .as_deref()
                    .is_some_and(|b| b.starts_with("0%"))
                {
                    theme.warn()
                } else {
                    theme.ok()
                }
            }
            ConnectionUsageState::Error(_) => theme.err(),
            ConnectionUsageState::Fetching => theme.info(),
            ConnectionUsageState::Unsupported => theme.muted(),
        };

        let active_tag = if entry.is_default { " (active)" } else { "" };
        let name_span = Span::styled(
            format!("{prefix}{}{active_tag}", entry.name),
            if is_selected {
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg())
            },
        );

        let provider_span = Span::styled(
            format!("  [{}]", entry.provider),
            Style::default().fg(theme.muted()),
        );

        let bal_str = entry.primary_balance.as_deref().unwrap_or("-");
        let bal_span = Span::styled(
            bal_str.to_string(),
            Style::default().fg(status_color).add_modifier(Modifier::BOLD),
        );

        let left_len =
            prefix.len() + entry.name.len() + active_tag.len() + 4 + entry.provider.len();
        let pad = body_width.saturating_sub(left_len + bal_str.len()).max(1);

        lines.push(Line::from(vec![
            name_span,
            provider_span,
            Span::raw(" ".repeat(pad)),
            bal_span,
        ]));

        let mut meta_parts = Vec::new();
        if let Some(acct) = &entry.account_id {
            meta_parts.push(format!("account: {acct}"));
        }
        if let Some(reset_ms) = entry.earliest_reset_ms {
            let now = chrono::Utc::now().timestamp_millis() as u64;
            if reset_ms > now {
                let diff_secs = (reset_ms - now) / 1000;
                let hours = diff_secs / 3600;
                let mins = (diff_secs % 3600) / 60;
                meta_parts.push(format!("resets in {hours}h {mins}m"));
            }
        }
        if !meta_parts.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("    {}", meta_parts.join("  •  ")),
                Style::default().fg(theme.muted()),
            )));
        }

        if let Some(ProviderQuotaData::Periodic(p)) = &entry.quota {
            for bucket in &p.buckets {
                let pct = (bucket.used_fraction * 100.0).round() as u32;
                let win = bucket.window.map(|w| w.label()).unwrap_or("Window");
                let bar = quota_bar(bucket.used_fraction);
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("      {win} ({}): ", bucket.label),
                        Style::default().fg(theme.muted()),
                    ),
                    Span::styled(bar, Style::default().fg(theme.brand())),
                    Span::styled(format!(" {pct}% used"), Style::default().fg(theme.fg())),
                ]));
            }
        }

        lines.push(Line::from(""));
    }

    lines
}

fn quota_bar(used_fraction: f32) -> String {
    let width = 10usize;
    let filled = ((1.0 - used_fraction.clamp(0.0, 1.0)) * width as f32).round() as usize;
    let empty = width.saturating_sub(filled);
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}
