//! Provider Quotas Dashboard overlay (`/quotas`, ADR-0036).
//!
//! Two-tier design:
//! - L1: Compact summary `name provider [92%/5h 99%/week]`
//! - L2: Expanded detail with progress bars (Enter to toggle)

use nuo_wire::{ConnectionQuotaEntry, ConnectionUsageState, ProviderQuotaData, ProviderQuotaSnapshot, QuotaWindowKind};
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

/// Draw the provider quota dashboard with two-tier UI.
/// L1: Compact summary rows. L2: Expanded detail with progress bars.
#[allow(clippy::too_many_arguments)]
pub fn draw_quotas_modal(
    frame: &mut Frame,
    snapshot: Option<&ProviderQuotaSnapshot>,
    loading: bool,
    selected_index: usize,
    scroll: &mut usize,
    expanded_index: Option<usize>,
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
            format!("Quotas [{filter}]")
        } else {
            "Quotas".to_string()
        };
        (
            vec![HeaderPart::title(Box::leak(title.into_boxed_str()))],
            quotas_body_l1(snap, selected_index, expanded_index, body_width, theme),
            vec![
                FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
                FooterHint::secondary("Enter", "expand"),
                FooterHint::secondary("Space", "switch"),
                FooterHint::secondary("r", "refresh"),
                FooterHint::key_always(crate::keymap::Key::ESC, "close"),
            ],
        )
    } else {
        (
            vec![HeaderPart::title("Quotas")],
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

/// L1: Build the compact summary rows. Each row may expand to show L2 detail.
fn quotas_body_l1(
    snapshot: &ProviderQuotaSnapshot,
    selected_index: usize,
    expanded_index: Option<usize>,
    _body_width: usize,
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
        let is_expanded = expanded_index == Some(idx);

        // L1: Compact summary line
        let summary = compact_summary(entry, is_selected, theme);
        lines.push(summary);

        // L2: Expanded detail (progress bars, meta info)
        if is_expanded {
            lines.extend(quotas_body_l2(entry, theme));
        }
    }

    lines
}

/// L1: Render the compact summary line: `name provider [92%/5h 99%/week]`
fn compact_summary(
    entry: &ConnectionQuotaEntry,
    is_selected: bool,
    theme: &Theme,
) -> Line<'static> {
    let prefix = if is_selected { "▶ " } else { "  " };

    // Connection name
    let active_tag = if entry.is_default { " *" } else { "" };
    let name_style = if is_selected {
        Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg())
    };

    // Provider
    let provider_style = Style::default().fg(theme.muted());

    // Quota summary in brackets
    let quota_summary = build_quota_summary(entry, theme);

    Line::from(vec![
        Span::styled(format!("{}{}{active_tag}", prefix, entry.name), name_style),
        Span::styled(format!(" {}", entry.provider), provider_style),
        Span::raw(" "),
        quota_summary,
    ])
}

/// Build the quota summary string: `[92%/5h 99%/week]` or error state
fn build_quota_summary(entry: &ConnectionQuotaEntry, theme: &Theme) -> Span<'static> {
    match &entry.state {
        ConnectionUsageState::Available(_) => {
            // Extract quota windows from the periodic data
            let Some(ProviderQuotaData::Periodic(p)) = &entry.quota else {
                // Fallback: use primary_balance
                let balance = entry.primary_balance.as_deref().unwrap_or("-");
                return Span::styled(
                    format!("[{balance}]"),
                    Style::default().fg(theme.muted()),
                );
            };

            // Build compact quota strings for each window, sorted by priority
            let mut parts: Vec<String> = Vec::new();
            for bucket in &p.buckets {
                if let Some(window) = bucket.window {
                    let pct_used = (bucket.used_fraction * 100.0).round() as u32;
                    let pct_rem = 100 - pct_used;
                    let window_label = match window {
                        QuotaWindowKind::Rolling5Hour => "5h",
                        QuotaWindowKind::Daily => "24h",
                        QuotaWindowKind::Weekly => "wk",
                        QuotaWindowKind::Monthly => "mo",
                        QuotaWindowKind::Custom => "custom",
                    };
                    parts.push(format!("{pct_rem}%/{window_label}"));
                }
            }

            if parts.is_empty() {
                Span::styled(
                    format!("[{}]", entry.primary_balance.as_deref().unwrap_or("-")),
                    Style::default().fg(theme.muted()),
                )
            } else {
                let summary = format!("[{}]", parts.join(" "));
                // Color based on lowest remaining quota
                let lowest_pct = parts
                    .iter()
                    .filter_map(|p| p.strip_prefix(|c: char| c.is_ascii_digit()).and_then(|s| s.split('/').next()).and_then(|s| s.parse::<u32>().ok()))
                    .min()
                    .unwrap_or(100);

                let color = if lowest_pct < 20 {
                    theme.warn()
                } else if lowest_pct < 50 {
                    theme.brand() // Orange-ish
                } else {
                    theme.ok()
                };
                Span::styled(summary, Style::default().fg(color))
            }
        }
        ConnectionUsageState::Fetching => {
            Span::styled("[querying…]", Style::default().fg(theme.info()))
        }
        ConnectionUsageState::Error(e) => {
            Span::styled(format!("[err: {}]", e), Style::default().fg(theme.err()).add_modifier(Modifier::BOLD))
        }
        ConnectionUsageState::Unsupported => {
            Span::styled("[n/a]", Style::default().fg(theme.muted()))
        }
    }
}

/// L2: Render the expanded detail block with progress bars and meta info.
fn quotas_body_l2(entry: &ConnectionQuotaEntry, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let indent = "    ";

    // Meta info: account, reset time
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
            format!("{indent}{}", meta_parts.join("  •  ")),
            Style::default().fg(theme.muted()),
        )));
    }

    // Progress bars for each quota window
    if let Some(ProviderQuotaData::Periodic(p)) = &entry.quota {
        for bucket in &p.buckets {
            let pct = (bucket.used_fraction * 100.0).round() as u32;
            let win = bucket.window.map(|w| w.label()).unwrap_or("Window");
            let bar = quota_bar(bucket.used_fraction);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{indent}{win} ({}): ", bucket.label),
                    Style::default().fg(theme.muted()),
                ),
                Span::styled(bar, Style::default().fg(theme.brand())),
                Span::styled(format!(" {pct}% used"), Style::default().fg(theme.fg())),
            ]));
        }
    }

    // Add a blank line after expanded block
    lines.push(Line::from(""));

    lines
}

fn quota_bar(used_fraction: f32) -> String {
    let width = 10usize;
    let filled = ((1.0 - used_fraction.clamp(0.0, 1.0)) * width as f32).round() as usize;
    let empty = width.saturating_sub(filled);
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_wire::{ConnectionQuotaEntry, ConnectionUsageState, QuotaWindowBucket, ProviderQuotaData, PeriodicQuota};

    fn line_to_string(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect::<Vec<_>>().join("")
    }

    fn make_theme() -> Theme {
        Theme::ansi16()
    }

    #[test]
    fn compact_summary_renders_name_and_provider() {
        let theme = make_theme();
        let entry = ConnectionQuotaEntry {
            name: "antigravity-1".into(),
            provider: "google-antigravity".into(),
            provider_label: "Google Antigravity".into(),
            account_id: Some("dev@example.com".into()),
            is_default: true,
            primary_balance: Some("85%".into()),
            quota: None,
            plan: None,
            state: ConnectionUsageState::Available(Box::new(nuo_wire::ProviderUsage {
                primary_balance: Some("85%".into()),
                ..Default::default()
            })),
            earliest_reset_ms: None,
        };

        let line = compact_summary(&entry, false, &theme);
        let s = line_to_string(&line);
        assert!(s.contains("antigravity-1"));
        assert!(s.contains("google-antigravity"));
    }

    #[test]
    fn compact_summary_shows_error_state() {
        let theme = make_theme();
        let entry = ConnectionQuotaEntry {
            name: "antigravity-2".into(),
            provider: "google-antigravity".into(),
            provider_label: "Google Antigravity".into(),
            account_id: None,
            is_default: false,
            primary_balance: None,
            quota: None,
            plan: None,
            state: ConnectionUsageState::Error("401 Unauthorized".into()),
            earliest_reset_ms: None,
        };

        let line = compact_summary(&entry, false, &theme);
        let s = line_to_string(&line);
        assert!(s.contains("antigravity-2"));
        assert!(s.contains("err"));
    }

    #[test]
    fn l2_expansion_renders_progress_bars() {
        let theme = make_theme();
        let entry = ConnectionQuotaEntry {
            name: "antigravity-1".into(),
            provider: "google-antigravity".into(),
            provider_label: "Google Antigravity".into(),
            account_id: None,
            is_default: false,
            primary_balance: Some("85%".into()),
            quota: Some(ProviderQuotaData::Periodic(PeriodicQuota {
                buckets: vec![
                    QuotaWindowBucket {
                        window: Some(QuotaWindowKind::Rolling5Hour),
                        label: "Gemini 2.5 Flash".into(),
                        group: Some("Chat Models".into()),
                        used_fraction: 0.15,
                        total_limit: Some(1000.0),
                        used_amount: Some(150.0),
                        ..Default::default()
                    },
                    QuotaWindowBucket {
                        window: Some(QuotaWindowKind::Weekly),
                        label: "Gemini 2.5 Pro".into(),
                        group: Some("Chat Models".into()),
                        used_fraction: 0.42,
                        total_limit: Some(5000.0),
                        used_amount: Some(2100.0),
                        ..Default::default()
                    },
                ],
            })),
            plan: None,
            state: ConnectionUsageState::Available(Box::new(nuo_wire::ProviderUsage::default())),
            earliest_reset_ms: None,
        };

        let lines = quotas_body_l2(&entry, &theme);
        let rendered: Vec<String> = lines.iter().map(line_to_string).collect();
        assert!(rendered.iter().any(|s| s.contains("5h Window")));
        assert!(rendered.iter().any(|s| s.contains("Weekly")));
        assert!(rendered.iter().any(|s| s.contains("█")));
    }
}