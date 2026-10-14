//! Usage-statistics overlay (`/usage`, ADR-0122): the durable cross-session
//! view over the day-partitioned store under `data/usage/`.
//!
//! Three sections in one scrolling body:
//! 1. **Daily totals** — one row per local day (newest first): total tokens,
//!    input/output split, request count.
//! 2. **Model breakdown** — one row per `(provider, model)` across all days,
//!    sorted by descending total.
//! 3. **Event log** — the most recent terminal request attempts (newest
//!    last), with lifecycle state, source, and token counts.
//!
//! Values use the same calm single-foreground palette as the Context Usage
//! modal; only lifecycle state is colored. A light bar chart of the last two
//! weeks of daily totals heads the body so daily shape is legible at a
//! glance.

use nuo_wire::RequestUsageStatus;
use nuo_wire::usage_stats::{UsageStatRecord, UsageStatsReport};
use nuotc::{
    Frame, Style, {Line, Span},
};

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

/// How many daily rows the bar chart covers (two weeks, newest at the right).
const CHART_DAYS: usize = 14;

/// Draw the overlay. `loading` marks the `QueryUsageStats` round-trip in
/// flight. Returns the painted panel rectangle (for outside-click dismiss).
/// The body is a selectable document: every visual row registers a
/// `MODAL_DOC` region, so the numbers can be drag-selected and copied.
pub fn draw_usage_stats_modal(
    frame: &mut Frame,
    report: &UsageStatsReport,
    loading: bool,
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

    let (header, body, footer) = if loading {
        (
            vec![HeaderPart::title("Usage Statistics")],
            vec![placeholder(
                "Loading usage statistics…",
                true,
                theme.muted(),
            )],
            vec![FooterHint::key_always(crate::keymap::Key::ESC, "close")],
        )
    } else {
        (
            vec![HeaderPart::title("Usage Statistics")],
            usage_body(report, body_width, theme),
            vec![FooterHint::always(keyvocab::ARROWS_UD, "scroll")],
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

/// The full overlay body: summary KV block, daily chart + table, model
/// breakdown, event log.
fn usage_body(report: &UsageStatsReport, body_width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut body = Vec::new();
    if report.days.is_empty() {
        body.push(placeholder(
            "No usage recorded yet — it appears after the first model request.",
            true,
            theme.muted(),
        ));
        return body;
    }

    // Summary
    let span_label = match (&report.first_day, &report.last_day) {
        (Some(first), Some(last)) if first == last => first.clone(),
        (Some(first), Some(last)) => format!("{first} → {last}"),
        _ => String::new(),
    };
    body.push(kv_line(
        "Range",
        &span_label,
        Style::default().fg(theme.fg()),
        theme,
    ));
    body.push(kv_line(
        "Total tokens",
        &fmt_tokens(report.grand_total.grand_total()),
        Style::default().fg(theme.fg()),
        theme,
    ));
    if report.grand_total.total_tokens > 0 {
        body.push(kv_line(
            "Input / output",
            &format!(
                "{} / {}",
                fmt_tokens(report.grand_total.prompt_tokens),
                fmt_tokens(report.grand_total.completion_tokens)
            ),
            Style::default().fg(theme.fg()),
            theme,
        ));
    }
    if report.grand_total.cache_read_tokens > 0 || report.grand_total.cache_write_tokens > 0 {
        body.push(kv_line(
            "Cache read / write",
            &format!(
                "{} / {}",
                fmt_tokens(report.grand_total.cache_read_tokens),
                fmt_tokens(report.grand_total.cache_write_tokens)
            ),
            Style::default().fg(theme.fg()),
            theme,
        ));
    }
    if report.grand_total.estimated_tokens > 0 {
        body.push(kv_line(
            "Estimated",
            &format!(
                "{} ({})",
                fmt_tokens(report.grand_total.estimated_tokens),
                pct(
                    report.grand_total.estimated_tokens,
                    report.grand_total.grand_total()
                )
            ),
            Style::default().fg(theme.muted()),
            theme,
        ));
    }
    body.push(kv_line(
        "Requests",
        &format!(
            "{} ({} completed)",
            report.grand_total.requests, report.grand_total.completed
        ),
        Style::default().fg(theme.fg()),
        theme,
    ));

    // Daily chart + table
    body.push(Line::from(""));
    body.push(section_line("Daily tokens", theme));
    body.push(daily_chart(report, body_width, theme));
    body.push(Line::from(""));
    daily_table(report, &mut body, theme);

    // Model breakdown
    if !report.models.is_empty() {
        body.push(Line::from(""));
        body.push(section_line("By model", theme));
        model_table(report, &mut body, theme);
    }

    // Event log
    if !report.events.is_empty() {
        body.push(Line::from(""));
        body.push(section_line("Recent requests", theme));
        event_log(report, &mut body, theme);
    }

    body
}

/// A section heading: uppercase muted label with a hairline rule.
fn section_line(title: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        title.to_uppercase(),
        Style::default().fg(theme.muted()),
    ))
}

fn kv_line(key: &str, value: &str, value_style: Style, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<18}"), Style::default().fg(theme.muted())),
        Span::styled(value.to_string(), value_style),
    ])
}

/// Horizontal bar chart of the newest `CHART_DAYS` days (newest at the
/// right). Each day renders as a `█`-repeat scaled to the window's max; days
/// with no usage render a dim `·` placeholder so gaps stay legible.
fn daily_chart(report: &UsageStatsReport, body_width: usize, theme: &Theme) -> Line<'static> {
    let newest: Vec<&nuo_wire::usage_stats::UsageDayTotals> =
        report.days.iter().rev().take(CHART_DAYS).collect();
    let max = newest
        .iter()
        .map(|d| d.totals.grand_total())
        .max()
        .unwrap_or(0)
        .max(1);
    // Two rows of axis labels (day number + compact total) plus one bar row
    // would crowd a modal; instead one row of bars whose height maps to the
    // character count, with the newest day's total shown beside the row.
    let bars_budget = body_width.saturating_sub(14).max(8);
    let cell_width = (bars_budget / newest.len().max(1)).max(1);
    let mut spans = Vec::new();
    for day in &newest {
        let total = day.totals.grand_total();
        let filled = if total <= 0 {
            0
        } else {
            ((total as f64 / max as f64) * cell_width as f64).round() as usize
        };
        let (text, style) = if total <= 0 {
            ("·".repeat(cell_width), Style::default().fg(theme.muted()))
        } else {
            (
                "█".repeat(filled.max(1)) + &" ".repeat(cell_width.saturating_sub(filled.max(1))),
                Style::default().fg(theme.fg()),
            )
        };
        spans.push(Span::styled(text, style));
    }
    if let Some(last) = newest.last() {
        spans.push(Span::styled(
            format!(" {}", fmt_tokens(last.totals.grand_total())),
            Style::default().fg(theme.muted()),
        ));
    }
    Line::from(spans)
}

/// The daily table, newest day first.
fn daily_table(report: &UsageStatsReport, body: &mut Vec<Line<'static>>, theme: &Theme) {
    let days: Vec<&nuo_wire::usage_stats::UsageDayTotals> =
        report.days.iter().rev().collect();
    // Column widths sized to content.
    let mut tokens_w = "Tokens".len();
    let mut io_w = "In / Out".len();
    let mut req_w = "Req".len();
    for day in &days {
        tokens_w = tokens_w.max(fmt_tokens(day.totals.grand_total()).len());
        io_w = io_w.max(
            format!(
                "{} / {}",
                fmt_tokens(day.totals.prompt_tokens),
                fmt_tokens(day.totals.completion_tokens)
            )
            .len(),
        );
        req_w = req_w.max(day.totals.requests.to_string().len());
    }
    let header_bg = theme.panel();
    body.push(Line::from(vec![
        Span::styled(
            format!("{:<10}", "Day"),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
        Span::styled("  ", Style::default().bg(header_bg)),
        Span::styled(
            format!("{:>width$}", "Tokens", width = tokens_w),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
        Span::styled("  ", Style::default().bg(header_bg)),
        Span::styled(
            format!("{:<width$}", "In / Out", width = io_w),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
        Span::styled("  ", Style::default().bg(header_bg)),
        Span::styled(
            format!("{:>width$}", "Req", width = req_w),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
    ]));
    for day in &days {
        let io = if day.totals.total_tokens > 0 {
            format!(
                "{} / {}",
                fmt_tokens(day.totals.prompt_tokens),
                fmt_tokens(day.totals.completion_tokens)
            )
        } else {
            "—".to_string()
        };
        body.push(Line::from(vec![
            Span::styled(format!("{:<10}", day.day), Style::default().fg(theme.fg())),
            Span::raw("  "),
            Span::styled(
                format!(
                    "{:>width$}",
                    fmt_tokens(day.totals.grand_total()),
                    width = tokens_w
                ),
                Style::default().fg(theme.fg()),
            ),
            Span::raw("  "),
            Span::styled(
                format!("{io:<width$}", width = io_w),
                Style::default().fg(theme.muted()),
            ),
            Span::raw("  "),
            Span::styled(
                format!("{:>width$}", day.totals.requests, width = req_w),
                Style::default().fg(theme.muted()),
            ),
        ]));
    }
}

/// The per-`(provider, model)` table, sorted by descending total.
fn model_table(report: &UsageStatsReport, body: &mut Vec<Line<'static>>, theme: &Theme) {
    let mut model_w = "Model".len();
    let mut tokens_w = "Tokens".len();
    for row in &report.models {
        model_w = model_w.max(row.model.len().min(34));
        tokens_w = tokens_w.max(fmt_tokens(row.totals.grand_total()).len());
    }
    let header_bg = theme.panel();
    body.push(Line::from(vec![
        Span::styled(
            format!("{:<12}", "Provider"),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
        Span::styled("  ", Style::default().bg(header_bg)),
        Span::styled(
            format!("{:<width$}", "Model", width = model_w),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
        Span::styled("  ", Style::default().bg(header_bg)),
        Span::styled(
            format!("{:>width$}", "Tokens", width = tokens_w),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
        Span::styled("  ", Style::default().bg(header_bg)),
        Span::styled(
            format!("{:>7}", "Req"),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
    ]));
    for row in &report.models {
        let model = truncate(&row.model, model_w);
        body.push(Line::from(vec![
            Span::styled(
                format!("{:<12}", truncate(&row.provider, 12)),
                Style::default().fg(theme.fg()),
            ),
            Span::raw("  "),
            Span::styled(
                format!("{model:<width$}", width = model_w),
                Style::default().fg(theme.fg()),
            ),
            Span::raw("  "),
            Span::styled(
                format!(
                    "{:>width$}",
                    fmt_tokens(row.totals.grand_total()),
                    width = tokens_w
                ),
                Style::default().fg(theme.fg()),
            ),
            Span::raw("  "),
            Span::styled(
                format!("{:>7}", row.totals.requests),
                Style::default().fg(theme.muted()),
            ),
        ]));
    }
}

/// The recent terminal-request log, newest last (append order reads as a
/// timeline).
fn event_log(report: &UsageStatsReport, body: &mut Vec<Line<'static>>, theme: &Theme) {
    let mut time_w = "Time".len();
    let mut model_w = "Model".len();
    for event in &report.events {
        time_w = time_w.max(event_time(event).len());
        model_w = model_w.max(event.record.model.len().min(24));
    }
    let header_bg = theme.panel();
    body.push(Line::from(vec![
        Span::styled(
            format!("{:<width$}", "Time", width = time_w),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
        Span::styled("  ", Style::default().bg(header_bg)),
        Span::styled(
            format!("{:<12}", "State"),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
        Span::styled("  ", Style::default().bg(header_bg)),
        Span::styled(
            format!("{:<width$}", "Model", width = model_w),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
        Span::styled("  ", Style::default().bg(header_bg)),
        Span::styled(
            format!("{:>10}", "Tokens"),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
    ]));
    for event in &report.events {
        let (state, state_style) = event_state(event, theme);
        let tokens = if event.record.total_tokens > 0 || event.is_reported() {
            fmt_tokens(
                event
                    .record
                    .total_tokens
                    .max(event.record.projected_prompt_tokens),
            )
        } else {
            "—".to_string()
        };
        let tokens = if event.record.source == nuo_wire::RequestUsageSource::Estimated {
            format!("~{tokens}")
        } else {
            tokens
        };
        body.push(Line::from(vec![
            Span::styled(
                format!("{:<width$}", event_time(event), width = time_w),
                Style::default().fg(theme.muted()),
            ),
            Span::raw("  "),
            Span::styled(format!("{state:<12}"), state_style),
            Span::raw("  "),
            Span::styled(
                format!(
                    "{:<width$}",
                    truncate(&event.record.model, model_w),
                    width = model_w
                ),
                Style::default().fg(theme.fg()),
            ),
            Span::raw("  "),
            Span::styled(format!("{tokens:>10}"), Style::default().fg(theme.fg())),
        ]));
    }
}

fn event_state(event: &UsageStatRecord, theme: &Theme) -> (&'static str, Style) {
    let label = match event.record.status {
        RequestUsageStatus::Completed => "completed",
        RequestUsageStatus::Interrupted => "interrupted",
        RequestUsageStatus::Failed => "failed",
        RequestUsageStatus::Abandoned => "abandoned",
        RequestUsageStatus::InFlight => "in-flight",
    };
    let color = match event.record.status {
        RequestUsageStatus::Completed => theme.ok(),
        RequestUsageStatus::Interrupted | RequestUsageStatus::Abandoned => theme.warn(),
        RequestUsageStatus::Failed => theme.err(),
        RequestUsageStatus::InFlight => theme.info(),
    };
    (label, Style::default().fg(color))
}

/// `MM-DD HH:MM` in the local timezone — the day is already the row's
/// bucket, so the time only needs to disambiguate within a day.
fn event_time(event: &UsageStatRecord) -> String {
    use chrono::TimeZone;
    let secs = (event.recorded_at_ms / 1_000) as i64;
    let nanos = ((event.recorded_at_ms % 1_000) * 1_000_000) as u32;
    chrono::Local
        .timestamp_opt(secs, nanos)
        .single()
        .map(|dt| dt.format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "--:--".to_string())
}

fn fmt_tokens(n: i64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn pct(part: i64, whole: i64) -> String {
    if whole <= 0 {
        return "0%".to_string();
    }
    format!("{}%", (part as f64 / whole as f64 * 100.0).round() as i64)
}

fn truncate(text: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let mut out: String = text.chars().take(max_width.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_wire::usage_stats::{
        UsageDayTotals, UsageModelRow, UsageModelTotals, UsageStatRecord,
    };
    use nuo_wire::{RequestUsageKey, RequestUsageRecord, RequestUsageSource};

    fn sample_report() -> UsageStatsReport {
        let record = |total: i64| UsageStatRecord {
            day: "2026-08-20".to_string(),
            recorded_at_ms: 1_700_000_000_000,
            project: "bucket".to_string(),
            record: RequestUsageRecord {
                key: RequestUsageKey::default(),
                provider: "anthropic".to_string(),
                model: "claude-sonnet-4-5".to_string(),
                status: RequestUsageStatus::Completed,
                source: RequestUsageSource::Reported,
                prompt_tokens: total - 90,
                completion_tokens: 90,
                total_tokens: total,
                ..Default::default()
            },
        };
        UsageStatsReport {
            days: vec![UsageDayTotals {
                day: "2026-08-20".to_string(),
                totals: UsageModelTotals {
                    requests: 3,
                    completed: 3,
                    total_tokens: 12_000,
                    prompt_tokens: 10_000,
                    completion_tokens: 2_000,
                    ..Default::default()
                },
            }],
            models: vec![UsageModelRow {
                provider: "anthropic".to_string(),
                model: "claude-sonnet-4-5".to_string(),
                totals: UsageModelTotals {
                    requests: 3,
                    completed: 3,
                    total_tokens: 12_000,
                    ..Default::default()
                },
            }],
            grand_total: UsageModelTotals {
                requests: 3,
                completed: 3,
                total_tokens: 12_000,
                prompt_tokens: 10_000,
                completion_tokens: 2_000,
                ..Default::default()
            },
            events: vec![record(4_000)],
            first_day: Some("2026-08-20".to_string()),
            last_day: Some("2026-08-20".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn body_renders_all_sections() {
        let theme = Theme::default();
        let body = usage_body(&sample_report(), 80, &theme);
        let text: Vec<String> = body
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect();
        let joined = text.join("\n");
        assert!(joined.contains("DAILY TOKENS"), "daily section: {joined}");
        assert!(joined.contains("BY MODEL"), "model section: {joined}");
        assert!(joined.contains("RECENT REQUESTS"), "events: {joined}");
        assert!(joined.contains("2026-08-20"));
        assert!(joined.contains("claude-sonnet-4-5"));
        assert!(joined.contains("12.0k"));
    }

    #[test]
    fn empty_report_shows_placeholder() {
        let theme = Theme::default();
        let body = usage_body(&UsageStatsReport::default(), 80, &theme);
        assert_eq!(body.len(), 1);
    }

    #[test]
    fn chart_scales_to_window_max() {
        let theme = Theme::default();
        let mut report = sample_report();
        report.days.push(UsageDayTotals {
            day: "2026-08-21".to_string(),
            totals: UsageModelTotals {
                requests: 1,
                completed: 1,
                total_tokens: 24_000,
                ..Default::default()
            },
        });
        let line = daily_chart(&report, 60, &theme);
        assert!(!line.spans.is_empty());
    }

    #[test]
    fn truncation_marks_long_models() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a-very-long-model-name", 8), "a-very-…");
    }
}
