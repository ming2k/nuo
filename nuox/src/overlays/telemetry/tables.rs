//! Activity tab tables: L1 rounds list and L2 turns list (sticky headers).

use nuotc::{Line, Modifier, Span, Style};

use super::model::*;
use crate::render::Theme;

// L1: Round Table Builder (Sticky Header + Data Rows)

pub(crate) fn build_rounds_table(
    rounds: &[TelemetryRound],
    selected_idx: usize,
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<Line<'static>>, Option<usize>) {
    let show_cache = width >= 86;
    let show_turns = width >= 72;

    let col_round = 8;
    let col_tokens = if width >= 76 { 20 } else { 14 };
    let col_cache = if show_cache { 12 } else { 0 };
    let col_tps = 15;
    let col_dur = 11;
    let col_turns = if show_turns { 10 } else { 0 };

    // 1. Fixed Header (1 line)
    let mut header_spans = vec![
        Span::styled(
            format!("  {:<w$}", "Round", w = col_round),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if width >= 76 {
                format!("{:<w$}", "Tokens (In / Out)", w = col_tokens)
            } else {
                format!("{:<w$}", "Tokens", w = col_tokens)
            },
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if show_cache {
        header_spans.push(Span::styled(
            format!("{:<w$}", "Cache %", w = col_cache),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ));
    }
    header_spans.push(Span::styled(
        format!("{:<w$}", "Stream TPS", w = col_tps),
        Style::default()
            .fg(theme.muted())
            .add_modifier(Modifier::BOLD),
    ));
    header_spans.push(Span::styled(
        format!("{:<w$}", "Duration", w = col_dur),
        Style::default()
            .fg(theme.muted())
            .add_modifier(Modifier::BOLD),
    ));
    if show_turns {
        header_spans.push(Span::styled(
            format!("{:<w$}", "Turns", w = col_turns),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ));
    }

    let header_lines = vec![Line::from(header_spans)];

    if rounds.is_empty() {
        let empty_rows = vec![Line::from(vec![Span::styled(
            "  No settled turns recorded in this session yet.",
            Style::default().fg(theme.muted()),
        )])];
        return (header_lines, empty_rows, None);
    }

    // 2. Data Rows
    let mut rows = Vec::with_capacity(rounds.len());
    for (i, r) in rounds.iter().enumerate() {
        let is_selected = i == selected_idx;
        let row_style = if is_selected {
            Style::default().bg(theme.selected_bg)
        } else {
            Style::default()
        };

        let mut row_spans = vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{:<w$}", format!("#{}", r.round_number), w = col_round),
                if is_selected {
                    Style::default()
                        .fg(theme.brand())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg())
                },
            ),
            Span::styled(
                if width >= 76 {
                    format!(
                        "{:<w$}",
                        format!(
                            "{} / {}",
                            fmt_tokens(r.prompt_tokens),
                            fmt_tokens(r.completion_tokens)
                        ),
                        w = col_tokens
                    )
                } else {
                    format!("{:<w$}", fmt_tokens(r.total_tokens), w = col_tokens)
                },
                Style::default().fg(theme.fg()),
            ),
        ];

        if show_cache {
            let cache_pct = r.cache_hit_rate();
            let cache_label = if cache_pct > 0.0 {
                format!("{:.0}%", cache_pct)
            } else {
                "–".to_string()
            };
            row_spans.push(Span::styled(
                format!("{:<w$}", cache_label, w = col_cache),
                if cache_pct > 0.0 {
                    Style::default().fg(theme.ok())
                } else {
                    Style::default().fg(theme.muted())
                },
            ));
        }

        let tps_label = fmt_tps(r.stream_tps());
        row_spans.push(Span::styled(
            format!("{:<w$}", tps_label, w = col_tps),
            Style::default().fg(theme.fg()),
        ));

        let dur_label = fmt_duration_ms(r.e2e_duration_ms);
        row_spans.push(Span::styled(
            format!("{:<w$}", dur_label, w = col_dur),
            Style::default().fg(theme.muted()),
        ));

        if show_turns {
            let turns_label = if r.turns_count > 1 {
                format!("{} turns", r.turns_count)
            } else {
                "1 turn".to_string()
            };
            row_spans.push(Span::styled(
                turns_label,
                Style::default().fg(theme.muted()),
            ));
        }

        let mut line = Line::from(row_spans);
        if is_selected {
            line = line.style(row_style);
        }
        rows.push(line);
    }

    let follow = if selected_idx < rounds.len() {
        Some(selected_idx)
    } else {
        None
    };

    (header_lines, rows, follow)
}

// L2: Turn Table Builder (Sticky Header + Data Rows)

pub(crate) fn build_turns_table(
    rounds: &[TelemetryRound],
    selected_round_idx: usize,
    selected_turn_idx: usize,
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<Line<'static>>, Option<usize>) {
    let round = rounds.get(selected_round_idx);
    let attempts = round.map_or(&[] as &[TelemetryAttempt], |r| &r.attempts);

    let col_turn = 10;
    let col_tokens = if width >= 76 { 20 } else { 14 };
    let col_ttft = 12;
    let col_tps = 15;
    let col_dur = 11;
    let col_status = 12;

    let header_spans = vec![
        Span::styled(
            format!("  {:<w$}", "Turn", w = col_turn),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if width >= 76 {
                format!("{:<w$}", "Tokens (In / Out)", w = col_tokens)
            } else {
                format!("{:<w$}", "Tokens", w = col_tokens)
            },
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<w$}", "TTFT", w = col_ttft),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<w$}", "Stream TPS", w = col_tps),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<w$}", "Duration", w = col_dur),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<w$}", "Status", w = col_status),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
    ];

    let header_lines = vec![Line::from(header_spans)];

    if attempts.is_empty() {
        let empty_rows = vec![Line::from(vec![Span::styled(
            "  No attempts recorded for this round.",
            Style::default().fg(theme.muted()),
        )])];
        return (header_lines, empty_rows, None);
    }

    let mut rows = Vec::with_capacity(attempts.len());
    for (i, att) in attempts.iter().enumerate() {
        let is_selected = i == selected_turn_idx;
        let row_style = if is_selected {
            Style::default().bg(theme.selected_bg)
        } else {
            Style::default()
        };

        let turn_label = if att.attempt > 1 {
            format!("#{}.{}", att.turn, att.attempt)
        } else {
            format!("#{}", att.turn)
        };

        let mut row_spans = vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{:<w$}", turn_label, w = col_turn),
                if is_selected {
                    Style::default()
                        .fg(theme.brand())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg())
                },
            ),
            Span::styled(
                if width >= 76 {
                    format!(
                        "{:<w$}",
                        format!(
                            "{} / {}",
                            fmt_tokens(att.prompt_tokens),
                            fmt_tokens(att.completion_tokens)
                        ),
                        w = col_tokens
                    )
                } else {
                    format!(
                        "{:<w$}",
                        fmt_tokens(att.prompt_tokens + att.completion_tokens),
                        w = col_tokens
                    )
                },
                Style::default().fg(theme.fg()),
            ),
        ];

        let ttft_label = att
            .performance
            .as_ref()
            .and_then(|p| p.ttft_us)
            .map(|us| format!("{:.0}ms", us as f64 / 1000.0))
            .unwrap_or_else(|| "–".to_string());
        row_spans.push(Span::styled(
            format!("{:<w$}", ttft_label, w = col_ttft),
            Style::default().fg(theme.fg()),
        ));

        let tps_label = fmt_tps(att.stream_tps());
        row_spans.push(Span::styled(
            format!("{:<w$}", tps_label, w = col_tps),
            Style::default().fg(theme.fg()),
        ));

        let dur_label = fmt_duration_ms(att.e2e_duration_ms);
        row_spans.push(Span::styled(
            format!("{:<w$}", dur_label, w = col_dur),
            Style::default().fg(theme.muted()),
        ));

        let status_lbl = status_label(att.status);
        let status_st = status_style(att.status, theme);
        row_spans.push(Span::styled(status_lbl, status_st));

        let mut line = Line::from(row_spans);
        if is_selected {
            line = line.style(row_style);
        }
        rows.push(line);
    }

    let follow = if selected_turn_idx < attempts.len() {
        Some(selected_turn_idx)
    } else {
        None
    };

    (header_lines, rows, follow)
}
