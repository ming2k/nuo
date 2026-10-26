//! Thread Stats overview: context window, thread token totals, streaming performance.

use nuo_wire::TokenSourceReport;
use nuotc::{Line, Modifier, Span, Style};

use super::model::*;
use crate::shell::render::Theme;
use nuo_ui_primitives::components::detail::DetailLayout;
use nuo_view::components::selectable_body::SelectableRow;

pub(crate) fn build_overview_body(
    report: &TokenSourceReport,
    rounds: &[TelemetryRound],
    context: ContextUsageProps,
    _width: usize,
    theme: &Theme,
) -> Vec<SelectableRow> {
    let mut layout = DetailLayout::new(theme);

    // 1. Context Window
    layout = layout.section("Context Window");

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

    layout = layout.kv("Used Tokens", &used_text);
    if window_max > 0 {
        layout = layout.kv_muted("Capacity", format!("{} tokens", fmt_num(window_max)));
        let pct_str = format!("{:.1}%", ratio * 100.0);
        layout = layout.kv_progress("Utilization", ratio as f32, 20, Some(pct_str));
    }
    if let Some(snap) = context.snapshot {
        if let Some(h) = snap.history_tokens {
            layout = layout.kv_indented(
                2,
                "History",
                format!("{} tokens", fmt_num(h)),
                Style::default().fg(theme.muted()),
            );
        }
        if let Some(o) = snap.overhead_tokens {
            layout = layout.kv_indented(
                2,
                "Overhead",
                format!("{} tokens", fmt_num(o)),
                Style::default().fg(theme.muted()),
            );
        }
        if let Some(t) = snap.temporary_context_tokens {
            layout = layout.kv_indented(
                2,
                "Temporary",
                format!("{} tokens", fmt_num(t)),
                Style::default().fg(theme.muted()),
            );
        }
    }
    if context.draft_tokens > 0 {
        layout = layout.kv_muted(
            "Draft Input",
            format!("~{} tokens", fmt_num(context.draft_tokens)),
        );
    }

    // 2. Thread Token Totals
    layout = layout.section("Thread Token Totals");

    let total_prompt = report.grand_total.prompt_tokens as u64;
    let total_completion = report.grand_total.completion_tokens as u64;
    let total_cache_read = report.grand_total.cache_read_tokens as u64;
    let total_cache_write = report.grand_total.cache_write_tokens as u64;
    let grand_total = total_prompt + total_completion;

    layout = layout.kv_bold(
        "Grand Total",
        format!("{} ({})", fmt_tokens(grand_total), fmt_num(grand_total)),
    );
    layout = layout.kv(
        "Input (Prompt)",
        format!("{} tokens", fmt_num(total_prompt)),
    );
    layout = layout.kv(
        "Output (Completion)",
        format!("{} tokens", fmt_num(total_completion)),
    );

    let hit_rate = if total_prompt > 0 {
        (total_cache_read as f64 / total_prompt as f64) * 100.0
    } else {
        0.0
    };
    let hit_style = if hit_rate > 0.0 {
        Style::default().fg(theme.ok())
    } else {
        Style::default().fg(theme.muted())
    };
    layout = layout.kv_styled(
        "Cache Read",
        format!(
            "{} tokens ({:.1}% hit rate)",
            fmt_num(total_cache_read),
            hit_rate
        ),
        hit_style,
    );
    if total_cache_write > 0 {
        layout = layout.kv_muted(
            "Cache Written",
            format!("{} tokens", fmt_num(total_cache_write)),
        );
    }

    // 3. Streaming Performance
    layout = layout.section("Streaming Performance");

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
    layout = layout.kv("Streaming Rate", fmt_tps(rate));
    let span_desc = if rate.is_some() {
        format!("{} tok over {}", fmt_num(tokens), fmt_duration_us(span_us))
    } else {
        "–".to_string()
    };
    layout = layout.kv_indented(
        2,
        "Tokens / Span",
        &span_desc,
        Style::default().fg(theme.muted()),
    );

    // Latency is reported as a median: with a handful of turns, a mean is
    // decided by whichever request was unluckiest.
    let ttft_desc = match median(&mut ttft_ms) {
        Some(median) => format!("{median:.0}ms"),
        None => "–".to_string(),
    };
    layout = layout.kv("TTFT (median)", &ttft_desc);

    layout = layout.kv_muted("Total Duration", fmt_duration_ms(total_e2e_ms));

    layout = layout.kv_muted(
        "Activity Count",
        format!("{} rounds · {} tool turns", rounds.len(), total_turns),
    );

    layout.build()
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

/// Section header line helper (used by attempt inspector in Session Trace).
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
