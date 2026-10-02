//! Overview tab: context window, session token totals, streaming performance.

use nuo_contracts::TokenSourceReport;
use nuotc::{Line, Modifier, Span, Style};

use super::model::*;
use crate::render::Theme;

pub(crate) fn build_overview_body(
    report: &TokenSourceReport,
    rounds: &[TelemetryRound],
    context: ContextUsageProps,
    _width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // 1. Context Window
    lines.push(section_header("Context window", theme));

    let window_max = context.window_tokens.unwrap_or(0);
    let used = context.snapshot.map(|s| s.tokens).unwrap_or(0);
    let ratio = if window_max == 0 {
        0.0
    } else {
        ((used as f64) / (window_max as f64)).clamp(0.0, 1.0)
    };

    let used_text = if window_max > 0 {
        format!("{} tokens ({:.1}%)", fmt_num(used), ratio * 100.0)
    } else {
        format!("{} tokens", fmt_num(used))
    };

    lines.push(kv_line(
        "Used Tokens",
        &used_text,
        Style::default().fg(theme.fg()),
        theme,
    ));
    if window_max > 0 {
        lines.push(kv_line(
            "Capacity",
            &format!("{} tokens", fmt_num(window_max)),
            Style::default().fg(theme.muted()),
            theme,
        ));
    }
    if context.draft_tokens > 0 {
        lines.push(kv_line(
            "Draft Input",
            &format!("~{} tokens", fmt_num(context.draft_tokens)),
            Style::default().fg(theme.muted()),
            theme,
        ));
    }

    lines.push(Line::from(""));

    // 2. Session Token Totals
    lines.push(section_header("Session token totals", theme));

    let total_prompt = report.grand_total.prompt_tokens as u64;
    let total_completion = report.grand_total.completion_tokens as u64;
    let total_cache_read = report.grand_total.cache_read_tokens as u64;
    let total_cache_write = report.grand_total.cache_write_tokens as u64;
    let grand_total = total_prompt + total_completion;

    lines.push(kv_line(
        "Grand Total",
        &format!("{} ({})", fmt_tokens(grand_total), fmt_num(grand_total)),
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
        theme,
    ));
    lines.push(kv_line(
        "Input (Prompt)",
        &format!("{} tokens", fmt_num(total_prompt)),
        Style::default().fg(theme.fg()),
        theme,
    ));
    lines.push(kv_line(
        "Output (Completion)",
        &format!("{} tokens", fmt_num(total_completion)),
        Style::default().fg(theme.fg()),
        theme,
    ));

    let hit_rate = if total_prompt > 0 {
        (total_cache_read as f64 / total_prompt as f64) * 100.0
    } else {
        0.0
    };
    lines.push(kv_line(
        "Cache Read",
        &format!(
            "{} tokens ({:.1}% hit rate)",
            fmt_num(total_cache_read),
            hit_rate
        ),
        Style::default().fg(if hit_rate > 0.0 {
            theme.ok()
        } else {
            theme.muted()
        }),
        theme,
    ));
    if total_cache_write > 0 {
        lines.push(kv_line(
            "Cache Written",
            &format!("{} tokens", fmt_num(total_cache_write)),
            Style::default().fg(theme.muted()),
            theme,
        ));
    }

    lines.push(Line::from(""));

    // 3. Streaming performance
    lines.push(section_header("Streaming performance", theme));

    // One rate, one aggregation: sum the tokens and the spans, then divide. The
    // reader can reproduce it from the per-turn numbers in the table below.
    let mut tokens: u64 = 0;
    let mut span_us: u64 = 0;
    let mut ttft_ms: Vec<f64> = Vec::new();
    let mut total_e2e_ms: u64 = 0;
    let mut total_turns: usize = 0;

    for r in rounds {
        total_turns += r.turns_count;
        total_e2e_ms += r.e2e_duration_ms;
        for att in &r.attempts {
            if att.stream_tps().is_some()
                && let Some(span) = att.stream_span_us()
            {
                tokens += att.completion_tokens;
                span_us = span_us.saturating_add(span);
            }
            if let Some(perf) = &att.performance
                && let Some(ttft_us) = perf.ttft_us
            {
                ttft_ms.push(ttft_us as f64 / 1000.0);
            }
        }
    }

    let rate = (tokens > 0 && span_us > 0)
        .then(|| tokens as f64 * 1_000_000.0 / span_us as f64)
        .filter(|rate| rate.is_finite() && *rate > 0.0);
    lines.push(kv_line(
        "Streaming Rate",
        &fmt_tps(rate),
        Style::default().fg(theme.fg()),
        theme,
    ));
    lines.push(kv_line(
        "  (tokens / span)",
        &if rate.is_some() {
            format!("{} tok over {}", fmt_num(tokens), fmt_duration_us(span_us))
        } else {
            "–".to_string()
        },
        Style::default().fg(theme.muted()),
        theme,
    ));

    // Latency is reported as a median: with a handful of turns, a mean is
    // decided by whichever request was unluckiest.
    lines.push(kv_line(
        "TTFT (median)",
        &match median(&mut ttft_ms) {
            Some(median) => format!("{median:.0}ms"),
            None => "–".to_string(),
        },
        Style::default().fg(theme.fg()),
        theme,
    ));

    lines.push(kv_line(
        "Total Duration",
        &fmt_duration_ms(total_e2e_ms),
        Style::default().fg(theme.muted()),
        theme,
    ));

    lines.push(kv_line(
        "Activity Count",
        &format!("{} rounds · {} tool turns", rounds.len(), total_turns),
        Style::default().fg(theme.muted()),
        theme,
    ));

    lines
}

/// Median of a non-empty slice (in place).
fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[middle])
    } else {
        Some((values[middle - 1] + values[middle]) / 2.0)
    }
}

pub(crate) fn section_header(title: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

pub(crate) fn kv_line(key: &str, value: &str, val_style: Style, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("    {:<22}", key),
            Style::default().fg(theme.muted()),
        ),
        Span::styled(value.to_string(), val_style),
    ])
}
