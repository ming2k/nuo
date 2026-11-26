//! Telemetry data extraction, aggregation models, and rate calculations.

use crate::shell::surfaces::TraceAttemptKey;
use protocol::{RequestPerformance, RequestUsageStatus, TokenSourceReport};
use std::collections::BTreeMap;
use view::model::document::{MessageKind, ToolStepStatus, TranscriptMessage};

/// View properties for contextual tokens when displaying context limits.
#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)]
pub struct ContextUsageProps {
    pub snapshot: Option<protocol::ContextTokenSnapshot>,
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
    pub actor_id: String,
    pub model: String,
    pub provider: String,
    pub status: RequestUsageStatus,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub generation_ms: u64,
    pub performance: Option<RequestPerformance>,
    pub e2e_duration_ms: u64,
    /// Epoch milliseconds when the attempt was dispatched. Lets the timeline
    /// show what happened *before* dispatch (the TUI knows when Enter was
    /// pressed; the ledger knows when the provider was called).
    pub started_at_ms: u64,
    pub cost_nano_usd: Option<u64>,
}

impl TelemetryAttempt {
    pub fn snapshot(&self) -> Option<protocol::TurnPerformanceSnapshot> {
        let perf = self.performance?;
        Some(protocol::TurnPerformanceSnapshot {
            round: self.round,
            turn: self.turn,
            attempt: self.attempt,
            completion_tokens: self.completion_tokens,
            prompt_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_nano_usd: None,
            usage_source: protocol::RequestUsageSource::Reported,
            performance: perf,
        })
    }

    /// The streaming rate for this attempt: its completion count over the
    /// first→last token span.
    pub fn stream_tps(&self) -> Option<f64> {
        self.snapshot().and_then(|s| s.stream_tps())
    }

    /// The first→last token span, the denominator behind `Self::stream_tps`.
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
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub turns_count: usize,
    pub subagent_attempts_count: usize,
    pub e2e_duration_ms: u64,
    pub generation_ms: u64,
    pub cost_nano_usd: Option<u64>,
    pub first_started_at_ms: Option<u64>,
    pub last_finished_at_ms: Option<u64>,
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

    /// Observed wall-clock span from the earliest attempt dispatch to the
    /// latest attempt completion, when epoch dispatch timestamps were recorded.
    pub fn wall_span_ms(&self) -> Option<u64> {
        match (self.first_started_at_ms, self.last_finished_at_ms) {
            (Some(start), Some(end)) if end >= start => Some(end - start),
            _ => None,
        }
    }

    /// Average measured TTFT across attempts in this round, in microseconds.
    pub fn avg_ttft_us(&self) -> Option<u64> {
        let mut sum = 0u64;
        let mut count = 0u64;
        for att in &self.attempts {
            if let Some(ttft) = att.performance.and_then(|p| p.ttft_us) {
                sum = sum.saturating_add(ttft);
                count += 1;
            }
        }
        (count > 0).then(|| sum / count)
    }

    /// Count of non-completed terminal attempts (failed, interrupted, abandoned).
    pub fn non_completed_attempts(&self) -> usize {
        self.attempts
            .iter()
            .filter(|a| a.status != RequestUsageStatus::Completed)
            .count()
    }

    /// Ordered, deduplicated `(model, provider)` pairs observed in this round.
    pub fn models(&self) -> Vec<(String, String)> {
        let mut seen = Vec::new();
        for att in self.attempts.iter().rev() {
            let pair = (att.model.clone(), att.provider.clone());
            if !seen.contains(&pair) {
                seen.push(pair);
            }
        }
        seen
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
        (tps.is_finite() && tps > 0.0 && tps <= protocol::MAX_PLAUSIBLE_STREAM_TPS).then_some(tps)
    }
}

/// Per-tool invocation tally inside one round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTally {
    pub name: String,
    pub total: usize,
    pub failed: usize,
}

/// Deterministic, transcript-derived activity facts for one round
/// (ADR-0263 `[INV-TRACE-03]`). Works identically for live and restored threads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoundTranscriptRecap {
    pub prompt_preview: Option<String>,
    pub tool_calls: usize,
    pub tool_ok: usize,
    pub tool_failed: usize,
    pub tool_denied: usize,
    pub tool_cancelled: usize,
    pub tool_interrupted: usize,
    pub tool_duration_ms: u64,
    pub tool_counts: Vec<ToolTally>,
    pub reasoning_count: usize,
    pub reasoning_duration_ms: u64,
}

/// Derive objective activity facts for `round_number` from the thread transcript.
pub fn extract_round_transcript_recap(
    messages: &[TranscriptMessage],
    round_number: u64,
) -> RoundTranscriptRecap {
    let mut recap = RoundTranscriptRecap::default();
    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();

    for msg in messages {
        if msg.round != Some(round_number) {
            continue;
        }
        match &msg.kind {
            MessageKind::Text if msg.role == tool::Role::User && recap.prompt_preview.is_none() => {
                let collapsed: String = msg.raw.split_whitespace().collect::<Vec<_>>().join(" ");
                if !collapsed.is_empty() {
                    recap.prompt_preview = Some(collapsed);
                }
            }
            MessageKind::ToolStep {
                name,
                status,
                duration_ms,
                ..
            } => {
                recap.tool_calls += 1;
                if let Some(ms) = duration_ms {
                    recap.tool_duration_ms = recap.tool_duration_ms.saturating_add(*ms);
                }
                let is_problem = match status {
                    ToolStepStatus::Ok => {
                        recap.tool_ok += 1;
                        false
                    }
                    ToolStepStatus::Failed => {
                        recap.tool_failed += 1;
                        true
                    }
                    ToolStepStatus::Denied => {
                        recap.tool_denied += 1;
                        true
                    }
                    ToolStepStatus::Cancelled => {
                        recap.tool_cancelled += 1;
                        true
                    }
                    ToolStepStatus::Interrupted => {
                        recap.tool_interrupted += 1;
                        true
                    }
                    ToolStepStatus::Ingesting | ToolStepStatus::Executing => false,
                };
                let entry = counts.entry(name.clone()).or_insert((0, 0));
                entry.0 += 1;
                if is_problem {
                    entry.1 += 1;
                }
            }
            MessageKind::Reasoning { duration_ms, .. } => {
                recap.reasoning_count += 1;
                if let Some(ms) = duration_ms {
                    recap.reasoning_duration_ms = recap.reasoning_duration_ms.saturating_add(*ms);
                }
            }
            _ => {}
        }
    }

    let mut tallies: Vec<ToolTally> = counts
        .into_iter()
        .map(|(name, (total, failed))| ToolTally {
            name,
            total,
            failed,
        })
        .collect();
    tallies.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.name.cmp(&b.name)));
    recap.tool_counts = tallies;
    recap
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
            let reasoning = req.reasoning_tokens.max(0) as u64;

            let e2e_duration_ms = req
                .performance
                .and_then(|p| p.e2e_us.map(|us| us / 1_000))
                .unwrap_or(req.generation_ms);

            let attempt = TelemetryAttempt {
                round: req.key.round,
                turn: req.key.turn,
                attempt: req.key.attempt,
                actor_id: req.key.actor_id.clone(),
                model: req.model.clone(),
                provider: req.provider.clone(),
                status: req.status,
                prompt_tokens: prompt,
                completion_tokens: completion,
                cache_read_tokens: cache_read,
                cache_write_tokens: cache_write,
                reasoning_tokens: reasoning,
                generation_ms: req.generation_ms,
                performance: req.performance,
                e2e_duration_ms,
                started_at_ms: req.started_at_ms,
                cost_nano_usd: req.cost_nano_usd,
            };

            round_map.entry(req.key.round).or_default().push(attempt);
        }
    }

    let mut result = Vec::new();
    for (round_num, mut attempts) in round_map.into_iter().rev() {
        attempts.sort_by(|a, b| {
            std::cmp::Reverse((
                a.turn,
                protocol::token_ledger::is_root_actor(&a.actor_id),
                a.attempt,
            ))
            .cmp(&std::cmp::Reverse((
                b.turn,
                protocol::token_ledger::is_root_actor(&b.actor_id),
                b.attempt,
            )))
            .then_with(|| a.actor_id.cmp(&b.actor_id))
        });

        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        let mut cache_read_tokens = 0u64;
        let mut cache_write_tokens = 0u64;
        let mut reasoning_tokens = 0u64;
        let mut e2e_duration_ms = 0u64;
        let mut generation_ms = 0u64;
        let mut cost_sum = 0u64;
        let mut has_cost = false;
        let mut first_started_at_ms: Option<u64> = None;
        let mut last_finished_at_ms: Option<u64> = None;
        let mut subagent_attempts_count = 0usize;

        let mut distinct_root_turns = std::collections::BTreeSet::new();
        let mut distinct_all_turns = std::collections::BTreeSet::new();

        for att in &attempts {
            prompt_tokens += att.prompt_tokens;
            completion_tokens += att.completion_tokens;
            cache_read_tokens += att.cache_read_tokens;
            cache_write_tokens += att.cache_write_tokens;
            reasoning_tokens += att.reasoning_tokens;
            distinct_all_turns.insert(att.turn);
            if protocol::token_ledger::is_root_actor(&att.actor_id) {
                distinct_root_turns.insert(att.turn);
            } else {
                subagent_attempts_count += 1;
            }
            e2e_duration_ms = e2e_duration_ms.saturating_add(att.e2e_duration_ms);
            generation_ms = generation_ms.saturating_add(att.generation_ms);
            if let Some(c) = att.cost_nano_usd {
                cost_sum = cost_sum.saturating_add(c);
                has_cost = true;
            }
            if att.started_at_ms > 0 {
                first_started_at_ms = Some(
                    first_started_at_ms
                        .map_or(att.started_at_ms, |prev| prev.min(att.started_at_ms)),
                );
                let finished = att.started_at_ms.saturating_add(att.e2e_duration_ms);
                last_finished_at_ms =
                    Some(last_finished_at_ms.map_or(finished, |prev| prev.max(finished)));
            }
        }

        let total_tokens = prompt_tokens + completion_tokens;
        let turns_count = if !distinct_root_turns.is_empty() {
            distinct_root_turns.len()
        } else {
            distinct_all_turns.len()
        };

        result.push(TelemetryRound {
            round_number: round_num,
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
            total_tokens,
            turns_count,
            subagent_attempts_count,
            e2e_duration_ms,
            generation_ms,
            cost_nano_usd: has_cost.then_some(cost_sum),
            first_started_at_ms,
            last_finished_at_ms,
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
) -> Option<TraceAttemptKey> {
    let rounds = extract_telemetry_rounds(report);
    let round = rounds.get(round_index)?;
    let attempt = round.attempts.get(attempt_index)?;
    Some(TraceAttemptKey {
        round: attempt.round,
        actor_id: attempt.actor_id.clone(),
        turn: attempt.turn,
        attempt: attempt.attempt,
    })
}

pub(crate) fn fmt_cost_nano_usd(nano: u64) -> String {
    let usd = nano as f64 / 1_000_000_000.0;
    if usd >= 1.0 {
        format!("${usd:.2}")
    } else if usd >= 0.01 {
        format!("${usd:.3}")
    } else {
        format!("${usd:.4}")
    }
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
            if rate > 0.0 && rate.is_finite() && rate <= protocol::MAX_PLAUSIBLE_STREAM_TPS =>
        {
            format!("{rate:.1} tok/s")
        }
        _ => "–".to_string(),
    }
}

pub(crate) fn status_style(
    status: RequestUsageStatus,
    theme: &crate::shell::render::Theme,
) -> graticule::Style {
    match status {
        RequestUsageStatus::Completed => graticule::Style::default().fg(theme.success),
        RequestUsageStatus::Interrupted => graticule::Style::default().fg(theme.warning),
        RequestUsageStatus::Failed | RequestUsageStatus::Abandoned => {
            graticule::Style::default().fg(theme.error_fg)
        }
        RequestUsageStatus::InFlight => graticule::Style::default().fg(theme.running()),
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
