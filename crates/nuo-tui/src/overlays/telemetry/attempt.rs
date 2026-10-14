//! L3 Attempt Inspector: identity header, context space, latency timeline.
//!
//! The timeline is a vertical ladder. One row per milestone; three aligned
//! columns — absolute timestamp, connector + stage name, delta from the
//! previous milestone. Deltas (not cumulative stamps) are the readable signal:
//! a long gap announces itself in the delta column without mental subtraction.

use nuotc::{Line, Modifier, Span, Style};

use super::model::*;
use super::overview::section_header;
use crate::render::Theme;

/// Column geometry of the ladder. The connector lives at a fixed column so
/// the vertical line threads through every node glyph.
const GUTTER: &str = "  ";
/// Stamp column: "  3.42s" — seconds with one decimal pair and the unit.
/// Turns longer than 9 minutes will spill past it; acceptable drift.
const STAMP_W: usize = 7;

/// Performance offsets are microseconds (see `RequestPerformance`); the
/// ladder's absolute stamps are seconds.
fn us_to_s(us: u64) -> f64 {
    us as f64 / 1_000_000.0
}

pub(crate) fn build_attempt_inspector_body(
    rounds: &[TelemetryRound],
    target_round: u32,
    target_attempt: u32,
    context: ContextUsageProps,
    _width: usize,
    submitted_at_ms: Option<u64>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let attempt = rounds
        .iter()
        .find(|r| r.round_number == target_round as u64)
        .and_then(|r| r.attempts.iter().find(|a| a.attempt == target_attempt));

    let mut lines = Vec::new();

    let Some(att) = attempt else {
        lines.push(Line::from(vec![Span::styled(
            "  Attempt record not found.",
            Style::default().fg(theme.text_muted),
        )]));
        return lines;
    };

    // ── Identity ────────────────────────────────────────────────────────────
    let connection_display = if att.provider.is_empty() {
        "default".to_string()
    } else {
        att.provider.clone()
    };

    lines.push(Line::from(vec![
        Span::styled(" Target:  ", Style::default().fg(theme.text_muted)),
        Span::styled(
            format!("{} @ {}", att.model, connection_display),
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" Status:  ", Style::default().fg(theme.text_muted)),
        Span::styled(
            format!("{:?}", att.status),
            status_style(att.status, theme).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled("Attempt: ", Style::default().fg(theme.text_muted)),
        Span::styled(
            format!("Turn #{} (attempt #{})", att.turn, att.attempt),
            Style::default().fg(theme.text),
        ),
    ]));
    lines.push(Line::from(""));

    // ── Context Space ───────────────────────────────────────────────────────
    lines.push(section_header("Context space", theme));

    let cache_pct = if att.prompt_tokens > 0 {
        (att.cache_read_tokens as f64 / att.prompt_tokens as f64) * 100.0
    } else {
        0.0
    };

    let window_max = context.window_tokens.unwrap_or(200_000);
    let ctx_pct = (att.prompt_tokens as f64 / window_max as f64) * 100.0;
    let bar_width = 24;
    let filled = ((ctx_pct / 100.0) * bar_width as f64).round() as usize;
    let filled = filled.min(bar_width);
    let empty = bar_width.saturating_sub(filled);
    let bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(empty));

    lines.push(Line::from(vec![
        Span::styled(
            "  Input Context:     ",
            Style::default().fg(theme.text_muted),
        ),
        Span::styled(
            format!("{:<10}", fmt_tokens(att.prompt_tokens)),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{bar} {ctx_pct:.1}% of {} max", fmt_num(window_max as u64)),
            Style::default().fg(theme.text_muted),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::styled(
            "   ├─ Cached Read:   ",
            Style::default().fg(theme.text_muted),
        ),
        Span::styled(
            format!("{:<10}", fmt_tokens(att.cache_read_tokens)),
            Style::default().fg(if att.cache_read_tokens > 0 {
                theme.success
            } else {
                theme.text
            }),
        ),
        Span::styled(
            format!("({cache_pct:.1}% Cache Hit)"),
            Style::default().fg(if cache_pct > 0.0 {
                theme.success
            } else {
                theme.text_muted
            }),
        ),
    ]));

    let fresh_input = att.prompt_tokens.saturating_sub(att.cache_read_tokens);
    lines.push(Line::from(vec![
        Span::styled(
            "   ├─ Fresh Input:   ",
            Style::default().fg(theme.text_muted),
        ),
        Span::styled(
            format!("{:<10}", fmt_tokens(fresh_input)),
            Style::default().fg(theme.text),
        ),
    ]));

    if att.cache_write_tokens > 0 {
        lines.push(Line::from(vec![
            Span::styled(
                "   ├─ Cache Created: ",
                Style::default().fg(theme.text_muted),
            ),
            Span::styled(
                format!("{:<10}", fmt_tokens(att.cache_write_tokens)),
                Style::default().fg(theme.warning),
            ),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled(
            "  Output Generated:  ",
            Style::default().fg(theme.text_muted),
        ),
        Span::styled(
            format!("{:<10}", fmt_tokens(att.completion_tokens)),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));

    // ── Latency Timeline ────────────────────────────────────────────────────
    lines.push(section_header("Latency timeline", theme));
    lines.push(Line::from(vec![Span::styled(
        "  Moments since you pressed send — Δ is the gap since the previous moment",
        Style::default().fg(theme.text_muted),
    )]));
    lines.push(Line::from(""));

    let perf = att.performance;
    let pre_dispatch_ms: Option<u64> = match (submitted_at_ms, att.started_at_ms) {
        (Some(submitted), started) if started >= submitted => Some(started - submitted),
        _ => None,
    };
    let base_ms = pre_dispatch_ms.unwrap_or(0) as f64 / 1000.0;

    let mut timeline = Timeline::new(theme);

    // User request (only when the composer timestamp is known).
    if pre_dispatch_ms.is_some() {
        timeline.node(
            Some(0.0),
            "User request",
            "you submitted the prompt".to_string(),
            muted(theme),
        );
    }

    // Dispatch: the ledger's t0. Everything before it is local work.
    timeline.node(
        Some(base_ms),
        "Dispatched",
        if pre_dispatch_ms.is_some() {
            format!("queue, context projection, hooks took {base_ms:.2}s")
        } else {
            "timeline starts here (composer timestamp unavailable)".to_string()
        },
        muted(theme),
    );

    // Connection: only the transport can place this moment, and only it can say
    // which regime the attempt ran under. A pooled socket really did pay
    // nothing; a cold start paid its phases and the ladder names them; with no
    // transport telemetry there is no connection moment to draw, and drawing one
    // from the response head would put it after the request was sent.
    //
    // Anchored on `connected_us` — the end of the last phase actually paid —
    // never on `stream_ready_us`, which is the response head and lands after
    // `Request sent` by construction.
    if let Some(perf) = perf
        && perf.pooled_connection().is_some()
        && let Some(connected_us) = perf.connected_us
    {
        let (detail, style) = if perf.pooled_connection() == Some(true) {
            (
                "reused pooled connection — no handshake".to_string(),
                good(theme),
            )
        } else {
            let phases: Vec<String> = [
                perf.dns_us.map(|us| format!("DNS {}", fmt_duration_us(us))),
                perf.tcp_us.map(|us| format!("TCP {}", fmt_duration_us(us))),
                perf.tls_us.map(|us| format!("TLS {}", fmt_duration_us(us))),
            ]
            .into_iter()
            .flatten()
            .collect();
            (format!("cold start: {}", phases.join(" + ")), accent(theme))
        };
        timeline.node(
            Some(base_ms + us_to_s(connected_us)),
            "Connected",
            detail,
            style,
        );
    }

    // Upload: dispatch → last byte handed to the kernel.
    let sent_us = perf.and_then(|p| p.request_sent_us);
    timeline.node(
        sent_us.map(|us| base_ms + us_to_s(us)),
        "Request sent",
        sent_us.map_or("not recorded".to_string(), |us| {
            format!("upload took {}", fmt_duration_us(us))
        }),
        muted(theme),
    );

    // Server started: response head + first origin frame collapse into one
    // node — the two events are usually milliseconds apart and the gap
    // carries no user-visible meaning. The detail keeps both costs.
    let head_us = perf.and_then(|p| p.stream_ready_us);
    let frame_us = perf.and_then(|p| p.first_frame_us);
    let server_detail = match (head_us, frame_us) {
        (Some(head), Some(frame)) => {
            format!(
                "headers {} · first frame {} from dispatch",
                fmt_duration_us(head),
                fmt_duration_us(frame)
            )
        }
        (Some(head), None) => format!("headers {} from dispatch", fmt_duration_us(head)),
        (None, Some(frame)) => format!("first frame {} from dispatch", fmt_duration_us(frame)),
        (None, None) => "not recorded".to_string(),
    };
    timeline.node(
        frame_us.or(head_us).map(|us| base_ms + us_to_s(us)),
        "Server started",
        server_detail,
        accent(theme),
    );

    // First token: TTFT against the request-sent anchor (the wire-wait truth).
    let ttft_us = perf.and_then(|p| p.ttft_us);
    let ttft_after_sent = match (sent_us, ttft_us) {
        (Some(sent), Some(ttft)) if ttft >= sent => Some(ttft - sent),
        _ => None,
    };
    timeline.node(
        ttft_us.map(|us| base_ms + us_to_s(us)),
        "First token",
        match (ttft_after_sent, ttft_us) {
            (Some(after), Some(total)) => format!(
                "TTFT {} after send · {} from dispatch",
                fmt_duration_us(after),
                fmt_duration_us(total)
            ),
            (None, Some(total)) => {
                format!(
                    "{} from dispatch (send anchor missing)",
                    fmt_duration_us(total)
                )
            }
            _ => "no output observed".to_string(),
        },
        good(theme),
    );

    // Last token: the stream span is the rate's denominator.
    let stream_us = perf.and_then(|p| p.stream_us);
    let last_token_us = match (ttft_us, stream_us) {
        (Some(ttft), Some(span)) => Some(ttft.saturating_add(span)),
        _ => None,
    };
    let rate = att.stream_tps();
    timeline.node(
        last_token_us.map(|us| base_ms + us_to_s(us)),
        "Last token",
        match (stream_us, rate) {
            (Some(span), Some(rate)) => format!(
                "streamed {} · {} tok @ {}",
                fmt_duration_us(span),
                fmt_num(att.completion_tokens),
                fmt_tps(Some(rate))
            ),
            (Some(span), None) => format!(
                "streamed {} · rate – (needs two events and a span)",
                fmt_duration_us(span)
            ),
            _ => "not recorded".to_string(),
        },
        good(theme),
    );

    // Stream closed (EOF after the last token).
    let tail_us = perf.and_then(|p| p.tail_us);
    let eof_us = match (last_token_us, tail_us) {
        (Some(last), Some(tail)) => Some(last.saturating_add(tail)),
        _ => None,
    };
    timeline.node(
        eof_us.map(|us| base_ms + us_to_s(us)),
        "Stream closed",
        tail_us.map_or("not recorded".to_string(), |tail| {
            format!("{} after the last token", fmt_duration_us(tail))
        }),
        muted(theme),
    );

    // Turn end: validated and settled.
    let e2e_us = perf
        .and_then(|p| p.e2e_us)
        .unwrap_or(att.e2e_duration_ms * 1_000);
    timeline.node(
        Some(base_ms + us_to_s(e2e_us)),
        "Turn end",
        format!("validated after {}", fmt_duration_us(e2e_us)),
        warn(theme),
    );

    lines.extend(timeline.lines);

    // One socket summary line only — RTT and retransmits live here, not in
    // per-node details. Both come from the same `TCP_INFO` sample, so a present
    // RTT is exactly what separates a measured zero retransmit count from a
    // socket nobody ever sampled.
    let rtt_us = perf.and_then(|p| p.rtt_us);
    if let Some(rtt_us) = rtt_us {
        let retransmits = perf.map(|p| p.retransmits).unwrap_or(0);
        lines.push(Line::from(vec![
            Span::styled(GUTTER, Style::default()),
            Span::styled(
                format!(
                    "socket: RTT {} · retransmits {retransmits}",
                    fmt_duration_us(rtt_us)
                ),
                Style::default().fg(theme.text_muted),
            ),
        ]));
    }

    lines
}

// ── Timeline renderer ───────────────────────────────────────────────────────

/// Renders the timeline as a plain bullet list. One row per moment:
///
/// ```text
/// - 0.41s  Dispatched   +0.41s  queue, context projection, hooks
/// ```
///
/// A row is a **moment** (the absolute stamp); its `+Δ` is the **interval**
/// since the previous row — the readable signal, no mental subtraction.
/// The one long interval (streaming) carries its duration in the detail.
struct Timeline<'a> {
    lines: Vec<Line<'static>>,
    theme: &'a Theme,
    /// Absolute time of the previously emitted row (seconds), for deltas.
    prev_at: Option<f64>,
}

/// Padded column width of the stage-name field.
const NAME_W: usize = 15;

impl<'a> Timeline<'a> {
    fn new(theme: &'a Theme) -> Self {
        Self {
            lines: Vec::new(),
            theme,
            prev_at: None,
        }
    }

    fn node(&mut self, at: Option<f64>, name: &str, detail: String, detail_style: Style) {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(5);
        spans.push(Span::styled("- ", Style::default().fg(self.theme.dim())));
        let stamp = at.map_or_else(
            || format!("{:>width$}", "–", width = STAMP_W),
            |at| format!("{at:>width$.2}s", width = STAMP_W),
        );
        spans.push(Span::styled(
            format!("{stamp}  "),
            Style::default()
                .fg(self.theme.brand())
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("{name:<NAME_W$}"),
            Style::default()
                .fg(self.theme.text)
                .add_modifier(Modifier::BOLD),
        ));

        // Delta from the previous moment — the interval being crossed.
        let delta = match (self.prev_at, at) {
            (Some(prev), Some(now)) if now >= prev => format!("+{:.2}s  ", now - prev),
            _ => "·  ".to_string(),
        };
        spans.push(Span::styled(delta, Style::default().fg(self.theme.dim())));
        spans.push(Span::styled(detail, detail_style));

        self.lines.push(Line::from(spans));
        self.prev_at = at;
    }
}

// Shared style helpers (attempt-view palette).

fn muted(theme: &Theme) -> Style {
    Style::default().fg(theme.text_muted)
}
fn accent(theme: &Theme) -> Style {
    Style::default().fg(theme.brand())
}
fn good(theme: &Theme) -> Style {
    Style::default().fg(theme.success)
}
fn warn(theme: &Theme) -> Style {
    Style::default().fg(theme.warning)
}
