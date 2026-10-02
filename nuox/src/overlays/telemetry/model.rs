//! Telemetry data extraction, aggregation models, and rate calculations.

use nuo_contracts::{RequestPerformance, RequestUsageStatus, TokenSourceReport};
use std::collections::BTreeMap;

/// View properties for contextual tokens when displaying context limits.
#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)]
pub struct ContextUsageProps {
    pub snapshot: Option<nuo_contracts::ContextTokenSnapshot>,
    pub window_tokens: Option<usize>,
    pub draft_content_tokens: usize,
    pub draft_tokens: usize,
}

/// A parsed attempt record for telemetry presentation.
#[derive(Debug, Clone)]
pub struct TelemetryAttempt {
    pub round: u64,
    pub turn: u32,
    pub attempt: u32,
    pub model: String,
    pub provider: String,
    pub status: RequestUsageStatus,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub performance: Option<RequestPerformance>,
    pub e2e_duration_ms: u64,
    /// Epoch milliseconds when the attempt was dispatched. Lets the timeline
    /// show what happened *before* dispatch (the TUI knows when Enter was
    /// pressed; the ledger knows when the provider was called).
    pub started_at_ms: u64,
}

impl TelemetryAttempt {
    pub fn snapshot(&self) -> Option<nuo_contracts::TurnPerformanceSnapshot> {
        let perf = self.performance?;
        Some(nuo_contracts::TurnPerformanceSnapshot {
            round: self.round,
            turn: self.turn,
            attempt: self.attempt,
            completion_tokens: self.completion_tokens,
            usage_source: nuo_contracts::RequestUsageSource::Reported,
            performance: perf,
        })
    }

    /// The streaming rate for this attempt: its completion count over the
    /// first→last token span.
    pub fn stream_tps(&self) -> Option<f64> {
        self.snapshot().and_then(|s| s.stream_tps())
    }

    /// The first→last token span, the denominator behind [`Self::stream_tps`].
    pub fn stream_span_us(&self) -> Option<u64> {
        self.performance
            .and_then(|performance| performance.stream_us)
    }
}

/// A round containing terminal attempts.
#[derive(Debug, Clone)]
pub struct TelemetryRound {
    pub round_number: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_tokens: u64,
    pub turns_count: usize,
    pub e2e_duration_ms: u64,
    pub attempts: Vec<TelemetryAttempt>,
}

impl TelemetryRound {
    pub fn cache_hit_rate(&self) -> f64 {
        if self.prompt_tokens == 0 {
            0.0
        } else {
            (self.cache_read_tokens as f64 / self.prompt_tokens as f64) * 100.0
        }
    }

    /// The round's streaming rate: total tokens over total streamed seconds.
    ///
    /// Summing tokens and spans is the only aggregation that keeps the displayed
    /// rate reproducible — averaging per-attempt rates would let a short
    /// attempt outvote a long one.
    pub fn stream_tps(&self) -> Option<f64> {
        let mut tokens = 0u64;
        let mut span_us = 0u64;
        for attempt in &self.attempts {
            let Some(span) = attempt.stream_span_us() else {
                continue;
            };
            if attempt.stream_tps().is_none() {
                continue;
            }
            tokens += attempt.completion_tokens;
            span_us = span_us.saturating_add(span);
        }
        if tokens == 0 || span_us == 0 {
            return None;
        }
        let tps = tokens as f64 * 1_000_000.0 / span_us as f64;
        (tps.is_finite() && tps > 0.0 && tps <= nuo_contracts::MAX_PLAUSIBLE_STREAM_TPS)
            .then_some(tps)
    }
}

/// Filter and extract only terminal attempts, grouped by round descending.
pub fn extract_telemetry_rounds(report: &TokenSourceReport) -> Vec<TelemetryRound> {
    let mut round_map = BTreeMap::<u64, Vec<TelemetryAttempt>>::new();

    for row in &report.rows {
        for req in &row.requests {
            if !req.status.is_terminal() {
                continue;
            }

            let prompt = req.prompt_tokens.max(0) as u64;
            let completion = req.completion_tokens.max(0) as u64;
            let cache_read = req.cache_read_tokens.max(0) as u64;
            let cache_write = req.cache_write_tokens.max(0) as u64;

            let e2e_duration_ms = req
                .performance
                .and_then(|p| p.e2e_us.map(|us| us / 1_000))
                .unwrap_or(req.generation_ms);

            let attempt = TelemetryAttempt {
                round: req.key.round,
                turn: req.key.turn,
                attempt: req.key.attempt,
                model: req.model.clone(),
                provider: req.provider.clone(),
                status: req.status,
                prompt_tokens: prompt,
                completion_tokens: completion,
                cache_read_tokens: cache_read,
                cache_write_tokens: cache_write,
                performance: req.performance,
                e2e_duration_ms,
                started_at_ms: req.started_at_ms,
            };

            round_map.entry(req.key.round).or_default().push(attempt);
        }
    }

    let mut result = Vec::new();
    for (round_num, mut attempts) in round_map.into_iter().rev() {
        attempts.sort_by_key(|attempt| std::cmp::Reverse((attempt.turn, attempt.attempt)));

        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        let mut cache_read_tokens = 0u64;
        let mut e2e_duration_ms = 0u64;

        let mut distinct_turns = std::collections::BTreeSet::new();

        for att in &attempts {
            prompt_tokens += att.prompt_tokens;
            completion_tokens += att.completion_tokens;
            cache_read_tokens += att.cache_read_tokens;
            distinct_turns.insert(att.turn);
            e2e_duration_ms += att.e2e_duration_ms;
        }

        let total_tokens = prompt_tokens + completion_tokens;
        let turns_count = distinct_turns.len();

        result.push(TelemetryRound {
            round_number: round_num,
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            total_tokens,
            turns_count,
            e2e_duration_ms,
            attempts,
        });
    }

    result
}

pub fn telemetry_round_count(report: &TokenSourceReport) -> usize {
    extract_telemetry_rounds(report).len()
}

pub fn telemetry_attempt_count(report: &TokenSourceReport, round_index: usize) -> usize {
    let rounds = extract_telemetry_rounds(report);
    rounds.get(round_index).map_or(0, |r| r.attempts.len())
}

pub fn telemetry_attempt_key(
    report: &TokenSourceReport,
    round_index: usize,
    attempt_index: usize,
) -> Option<(u32, u32)> {
    let rounds = extract_telemetry_rounds(report);
    let round = rounds.get(round_index)?;
    let attempt = round.attempts.get(attempt_index)?;
    Some((attempt.round as u32, attempt.attempt))
}

pub(crate) fn fmt_num<T: std::fmt::Display>(num: T) -> String {
    let s = num.to_string();
    let is_negative = s.starts_with('-');
    let remainder = if is_negative { &s[1..] } else { &s[..] };
    let (int_part, frac_part) = match remainder.find('.') {
        Some(pos) => (&remainder[..pos], Some(&remainder[pos..])),
        None => (remainder, None),
    };
    let len = int_part.len();
    let mut result = String::with_capacity(s.len() + len / 3);
    for (i, c) in int_part.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    if let Some(frac) = frac_part {
        result.push_str(frac);
    }
    if is_negative {
        format!("-{result}")
    } else {
        result
    }
}

pub(crate) fn fmt_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        format!("{count}")
    }
}

pub(crate) fn fmt_duration_ms(ms: u64) -> String {
    if ms >= 60_000 {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) as f64 / 1_000.0;
        format!("{mins}m {secs:.1}s")
    } else if ms >= 1_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{ms}ms")
    }
}

pub(crate) fn fmt_duration_us(us: u64) -> String {
    if us >= 1_000_000 {
        format!("{:.2}s", us as f64 / 1_000_000.0)
    } else if us >= 1_000 {
        format!("{:.0}ms", us as f64 / 1_000.0)
    } else {
        format!("{us}µs")
    }
}

pub(crate) fn fmt_tps(tps: Option<f64>) -> String {
    match tps {
        Some(rate)
            if rate > 0.0
                && rate.is_finite()
                && rate <= nuo_contracts::MAX_PLAUSIBLE_STREAM_TPS =>
        {
            format!("{rate:.1} tok/s")
        }
        _ => "–".to_string(),
    }
}

pub(crate) fn status_style(
    status: RequestUsageStatus,
    theme: &crate::render::Theme,
) -> nuotc::Style {
    match status {
        RequestUsageStatus::Completed => nuotc::Style::default().fg(theme.success),
        RequestUsageStatus::Interrupted => nuotc::Style::default().fg(theme.warning),
        RequestUsageStatus::Failed | RequestUsageStatus::Abandoned => {
            nuotc::Style::default().fg(theme.error_fg)
        }
        RequestUsageStatus::InFlight => nuotc::Style::default().fg(theme.brand()),
    }
}

pub(crate) fn status_label(status: RequestUsageStatus) -> &'static str {
    match status {
        RequestUsageStatus::Completed => "Done",
        RequestUsageStatus::Interrupted => "Interrupted",
        RequestUsageStatus::Failed => "Failed",
        RequestUsageStatus::Abandoned => "Abandoned",
        RequestUsageStatus::InFlight => "In-flight",
    }
}
