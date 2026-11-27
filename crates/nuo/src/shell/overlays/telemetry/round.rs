//! L1 Round Inspector (`i` on a round in `/trace`, ADR-0263 `[INV-TRACE-03]`).
//!
//! Renders an objective, zero-LLM recap of one round:
//! - overview (prompt preview, model/provider attribution, turns & attempts);
//! - timing breakdown (wall-clock / observed span, model request time, tool
//!   execution time, human wait pauses, reasoning duration);
//! - activity (tool call totals, statuses, and per-tool counts derived from the
//!   thread transcript);
//! - token & cost totals.

use graticule::Style;
use nuo_ui_primitives::components::detail::DetailLayout;
use nuo_view::components::selectable_body::SelectableRow;

use super::model::*;
use crate::shell::render::Theme;

/// Truncate a single-line preview to `max_cols` display columns with `ellipsis`.
fn ellipsize_single_line(text: &str, max_cols: usize, ellipsis: &str) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    if text.width() <= max_cols {
        return text.to_string();
    }
    let ell_w = ellipsis.width();
    if max_cols <= ell_w {
        return ellipsis.to_string();
    }
    let budget = max_cols - ell_w;
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push_str(ellipsis);
    out
}

/// Build the L1 Round Inspector body for one round.
pub(crate) fn build_round_inspector_body(
    round: &TelemetryRound,
    summary: Option<&nuo_protocol::RoundSummary>,
    recap: &RoundTranscriptRecap,
    width: usize,
    theme: &Theme,
) -> Vec<SelectableRow> {
    let mut layout = DetailLayout::new(theme);

    // ── 1. Overview ─────────────────────────────────────────────────────────
    layout = layout.section("Overview");

    if let Some(prompt) = recap.prompt_preview.as_deref() {
        let value_budget = width.saturating_sub(24).max(16);
        let preview = ellipsize_single_line(prompt, value_budget, theme.glyphs.ellipsis);
        layout = layout.kv("Prompt", preview);
    }

    let models = round.models();
    if !models.is_empty() {
        let formatted: Vec<String> = models
            .into_iter()
            .map(|(model, provider)| {
                if provider.is_empty() {
                    model
                } else {
                    format!("{model} @ {provider}")
                }
            })
            .collect();
        layout = layout.kv_styled(
            if formatted.len() > 1 {
                "Models"
            } else {
                "Model"
            },
            formatted.join(", "),
            Style::default().fg(theme.brand()),
        );
    }

    let attempt_count = round.attempts.len();
    let failed_attempts = round.non_completed_attempts();
    let turns_label = if round.turns_count == 1 {
        "1 turn".to_string()
    } else {
        format!("{} turns", round.turns_count)
    };
    let mut exec_extras = Vec::new();
    if round.subagent_attempts_count > 0 {
        let word = if round.subagent_attempts_count == 1 {
            "subagent call"
        } else {
            "subagent calls"
        };
        exec_extras.push(format!("{} {word}", round.subagent_attempts_count));
    }
    if attempt_count > round.turns_count + round.subagent_attempts_count {
        exec_extras.push(format!("{attempt_count} attempts"));
    }
    if failed_attempts > 0 {
        exec_extras.push(format!("{failed_attempts} non-completed"));
    }
    let execution = if exec_extras.is_empty() {
        turns_label
    } else {
        format!("{turns_label} ({})", exec_extras.join(", "))
    };
    layout = layout.kv("Execution", execution);

    // ── 2. Timing Breakdown ─────────────────────────────────────────────────
    layout = layout.section("Timing Breakdown");

    let (total_ms, total_basis) = if let Some(s) = summary.filter(|s| s.duration_ms > 0) {
        (s.duration_ms, "wall-clock")
    } else if let Some(span) = round.wall_span_ms().filter(|&ms| ms > 0) {
        (span, "observed span")
    } else {
        (round.e2e_duration_ms, "model sum")
    };
    layout = layout.kv_bold(
        "Total Duration",
        format!("{} ({total_basis})", fmt_duration_ms(total_ms)),
    );

    let mut model_parts = vec![fmt_duration_ms(round.e2e_duration_ms)];
    if let Some(avg_ttft) = round.avg_ttft_us() {
        model_parts.push(format!("avg TTFT {}", fmt_duration_us(avg_ttft)));
    }
    if let Some(tps) = round.stream_tps() {
        model_parts.push(fmt_tps(Some(tps)));
    }
    layout = layout.kv("Model Requests", model_parts.join("  ·  "));

    if recap.tool_duration_ms > 0 {
        layout = layout.kv("Tool Execution", fmt_duration_ms(recap.tool_duration_ms));
    }

    if let Some(paused) = summary.map(|s| s.paused_ms).filter(|&ms| ms > 0) {
        layout = layout.kv_styled(
            "Human Wait",
            format!("{} (approval / input pause)", fmt_duration_ms(paused)),
            Style::default().fg(theme.warning),
        );
    }

    if recap.reasoning_duration_ms > 0 {
        let trace_word = if recap.reasoning_count == 1 {
            "trace"
        } else {
            "traces"
        };
        layout = layout.kv_muted(
            "Reasoning",
            format!(
                "{} ({} {trace_word})",
                fmt_duration_ms(recap.reasoning_duration_ms),
                recap.reasoning_count
            ),
        );
    }

    // ── 3. Activity ─────────────────────────────────────────────────────────
    layout = layout.section("Activity");

    if recap.tool_calls == 0 {
        layout = layout.kv_muted("Tool Calls", "None (direct model response)");
    } else {
        let mut status_parts = Vec::new();
        if recap.tool_ok > 0 {
            status_parts.push(format!("{} ok", recap.tool_ok));
        }
        if recap.tool_failed > 0 {
            status_parts.push(format!("{} failed", recap.tool_failed));
        }
        if recap.tool_denied > 0 {
            status_parts.push(format!("{} denied", recap.tool_denied));
        }
        if recap.tool_cancelled > 0 {
            status_parts.push(format!("{} cancelled", recap.tool_cancelled));
        }
        if recap.tool_interrupted > 0 {
            status_parts.push(format!("{} interrupted", recap.tool_interrupted));
        }
        let summary_text = if status_parts.is_empty() {
            format!("{} calls", recap.tool_calls)
        } else {
            format!("{} calls ({})", recap.tool_calls, status_parts.join(", "))
        };
        let summary_style = if recap.tool_failed > 0 || recap.tool_denied > 0 {
            Style::default().fg(theme.warning)
        } else {
            Style::default().fg(theme.fg())
        };
        layout = layout.kv_styled("Tool Calls", summary_text, summary_style);

        for tally in &recap.tool_counts {
            let count_str = if tally.failed > 0 {
                format!("×{} ({} non-ok)", tally.total, tally.failed)
            } else {
                format!("×{}", tally.total)
            };
            let style = if tally.failed > 0 {
                Style::default().fg(theme.warning)
            } else {
                Style::default().fg(theme.muted())
            };
            layout = layout.kv_indented(2, &tally.name, count_str, style);
        }
    }

    // ── 4. Tokens & Cost ────────────────────────────────────────────────────
    layout = layout.section("Tokens & Cost");

    layout = layout.kv_bold(
        "Total Tokens",
        format!(
            "{} ({})",
            fmt_tokens(round.total_tokens),
            fmt_num(round.total_tokens)
        ),
    );
    layout = layout.kv(
        "Input (Prompt)",
        format!("{} tokens", fmt_num(round.prompt_tokens)),
    );
    layout = layout.kv(
        "Output (Completion)",
        format!("{} tokens", fmt_num(round.completion_tokens)),
    );

    let hit_rate = round.cache_hit_rate();
    let hit_style = if hit_rate > 0.0 {
        Style::default().fg(theme.ok())
    } else {
        Style::default().fg(theme.muted())
    };
    layout = layout.kv_styled(
        "Cache Read",
        format!(
            "{} tokens ({hit_rate:.1}% hit rate)",
            fmt_num(round.cache_read_tokens)
        ),
        hit_style,
    );
    if round.cache_write_tokens > 0 {
        layout = layout.kv_muted(
            "Cache Written",
            format!("{} tokens", fmt_num(round.cache_write_tokens)),
        );
    }
    if round.reasoning_tokens > 0 {
        layout = layout.kv_muted(
            "Reasoning Tokens",
            format!("{} tokens", fmt_num(round.reasoning_tokens)),
        );
    }
    if let Some(cost) = round.cost_nano_usd.filter(|&c| c > 0) {
        layout = layout.kv("Cost", fmt_cost_nano_usd(cost));
    }

    layout.build()
}
