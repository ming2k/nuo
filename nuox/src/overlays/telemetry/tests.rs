use super::TelemetryTab;
use super::attempt::build_attempt_inspector_body;
use super::draw::tab_strip_line;
use super::model::*;
use super::overview::build_overview_body;
use super::tables::{build_rounds_table, build_turns_table};
use crate::render::Theme;
use nuo_contracts::{
    RequestPerformance, RequestUsageKey, RequestUsageRecord, RequestUsageSource,
    RequestUsageStatus, TokenSourceReport, TokenSourceRow, TransportObservation,
};

#[test]
fn test_extract_telemetry_rounds_filters_terminal_only() {
    let mut report = TokenSourceReport::default();
    let row = TokenSourceRow {
        provider: "anthropic".to_string(),
        model: "claude-3-7-sonnet".to_string(),
        turns: Vec::new(),
        requests: vec![
            RequestUsageRecord {
                key: RequestUsageKey {
                    session_id: "s1".to_string(),
                    round: 1,
                    turn: 1,
                    attempt: 1,
                    actor_id: "root".to_string(),
                },
                provider: "anthropic".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                status: RequestUsageStatus::InFlight, // Non-terminal
                source: RequestUsageSource::Reported,
                prompt_tokens: 100,
                completion_tokens: 10,
                total_tokens: 110,
                generation_ms: 500,
                ..Default::default()
            },
            RequestUsageRecord {
                key: RequestUsageKey {
                    session_id: "s1".to_string(),
                    round: 1,
                    turn: 1,
                    attempt: 2,
                    actor_id: "root".to_string(),
                },
                provider: "anthropic".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                status: RequestUsageStatus::Completed, // Terminal
                source: RequestUsageSource::Reported,
                prompt_tokens: 1000,
                completion_tokens: 200,
                cache_read_tokens: 800,
                total_tokens: 1200,
                generation_ms: 1500,
                performance: Some(RequestPerformance {
                    stream_ready_us: Some(100_000),
                    ttft_us: Some(300_000),
                    stream_us: Some(1_200_000),
                    tail_us: Some(20_000),
                    e2e_us: Some(1_520_000),
                    streamed_output_tokens: 200,
                    first_output_tokens: 1,
                    output_events: 50,
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
        totals: Default::default(),
    };
    report.rows.push(row);

    let rounds = extract_telemetry_rounds(&report);
    assert_eq!(rounds.len(), 1);
    let r1 = &rounds[0];
    assert_eq!(r1.round_number, 1);
    assert_eq!(r1.attempts.len(), 1); // Running attempt filtered out!
    assert_eq!(r1.prompt_tokens, 1000);
    assert_eq!(r1.completion_tokens, 200);
    assert_eq!(r1.cache_read_tokens, 800);
    assert_eq!(r1.cache_hit_rate(), 80.0);
    assert!(r1.stream_tps().is_some());
}

#[test]
fn test_telemetry_round_and_turn_helpers() {
    let mut report = TokenSourceReport::default();
    let row = TokenSourceRow {
        provider: "anthropic".to_string(),
        model: "claude-3-7-sonnet".to_string(),
        turns: Vec::new(),
        requests: vec![
            RequestUsageRecord {
                key: RequestUsageKey {
                    session_id: "s1".to_string(),
                    round: 2,
                    turn: 1,
                    attempt: 1,
                    actor_id: "root".to_string(),
                },
                provider: "anthropic".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                status: RequestUsageStatus::Completed,
                source: RequestUsageSource::Reported,
                prompt_tokens: 2000,
                completion_tokens: 150,
                cache_read_tokens: 1600,
                total_tokens: 2150,
                generation_ms: 1200,
                performance: Some(RequestPerformance {
                    stream_ready_us: Some(150_000),
                    ttft_us: Some(350_000),
                    stream_us: Some(1_000_000),
                    tail_us: Some(30_000),
                    e2e_us: Some(1_200_000),
                    streamed_output_tokens: 150,
                    ..Default::default()
                }),
                ..Default::default()
            },
            RequestUsageRecord {
                key: RequestUsageKey {
                    session_id: "s1".to_string(),
                    round: 1,
                    turn: 1,
                    attempt: 1,
                    actor_id: "root".to_string(),
                },
                provider: "anthropic".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                status: RequestUsageStatus::Completed,
                source: RequestUsageSource::Reported,
                prompt_tokens: 500,
                completion_tokens: 50,
                total_tokens: 550,
                generation_ms: 600,
                ..Default::default()
            },
        ],
        totals: Default::default(),
    };
    report.rows.push(row);

    assert_eq!(telemetry_round_count(&report), 2);
    // Round 2 is first (descending)
    assert_eq!(telemetry_attempt_count(&report, 0), 1);
    assert_eq!(telemetry_attempt_count(&report, 1), 1);
    assert_eq!(telemetry_attempt_key(&report, 0, 0), Some((2, 1)));
    assert_eq!(telemetry_attempt_key(&report, 1, 0), Some((1, 1)));
}

#[test]
fn test_build_attempt_inspector_waterfall_nodes() {
    let theme = Theme::from_color_scheme("dark", &Default::default());
    let rounds = vec![TelemetryRound {
        round_number: 1,
        prompt_tokens: 4000,
        completion_tokens: 300,
        cache_read_tokens: 3000,
        total_tokens: 4300,
        turns_count: 1,
        e2e_duration_ms: 3_500,
        attempts: vec![TelemetryAttempt {
            round: 1,
            turn: 1,
            attempt: 1,
            model: "claude-3-7-sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: RequestUsageStatus::Completed,
            prompt_tokens: 4000,
            completion_tokens: 300,
            cache_read_tokens: 3000,
            cache_write_tokens: 0,
            performance: Some(RequestPerformance {
                connected_us: Some(150_000),
                request_sent_us: Some(400_000),
                stream_ready_us: Some(500_000),
                ttft_us: Some(680_000),
                stream_us: Some(3_000_000),
                output_events: 10,
                tail_us: Some(25_000),
                e2e_us: Some(3_425_000),
                streamed_output_tokens: 300,
                first_output_tokens: 0,
                dns_us: Some(20_000),
                tcp_us: Some(40_000),
                tls_us: Some(90_000),
                rtt_us: Some(42_000),
                retransmits: 0,
                observation: TransportObservation::ColdConnection,
                ..Default::default()
            }),
            e2e_duration_ms: 3500,
            started_at_ms: 200,
        }],
    }];

    let lines = build_attempt_inspector_body(
        &rounds,
        1,
        1,
        ContextUsageProps {
            window_tokens: Some(200_000),
            ..Default::default()
        },
        100,
        Some(0),
        &theme,
    );

    let full_text: String = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(full_text.contains("Target:  claude-3-7-sonnet @ anthropic"));
    assert!(full_text.contains("Context space"));
    assert!(full_text.contains("75.0% Cache Hit"));
    // The timeline names every moment from the user's send to the settled turn.
    assert!(full_text.contains("Latency timeline"));
    // Response headers and first origin frame collapse into one moment:
    // the two instants are usually milliseconds apart and the detail line
    // carries both costs, so the list stays readable.
    for moment in [
        "User request",
        "Dispatched",
        "Connected",
        "Request sent",
        "Server started",
        "First token",
        "Last token",
        "Stream closed",
        "Turn end",
    ] {
        assert!(full_text.contains(moment), "missing moment: {moment}");
    }
    // The connection moment is the cold start this record actually paid, named
    // with its phases rather than claimed as a pool hit.
    assert!(full_text.contains("cold start: DNS 20ms + TCP 40ms + TLS 90ms"));
    assert!(!full_text.contains("reused"));
    // And it sits *before* the request was sent: anchoring it on the response
    // head (the old behaviour) put it after, which made the timeline run
    // backwards. Position, not just presence, is the property being held.
    let connected_at = full_text.find("Connected").expect("connection moment");
    let sent_at = full_text.find("Request sent").expect("upload moment");
    let ready_at = full_text.find("Server started").expect("head moment");
    assert!(
        connected_at < sent_at && sent_at < ready_at,
        "the ladder must advance: Connected {connected_at} → Request sent {sent_at} → \
         Server started {ready_at}"
    );
    // `TCP_INFO` sampled this record, so the retransmit count is a measurement.
    assert!(full_text.contains("socket: RTT 42ms · retransmits 0"));
    // No decorative glyphs or connector lines may return.
    for line in &lines {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let stripped = text.trim_start();
        assert!(
            !stripped.starts_with('│') && !stripped.starts_with('●') && !stripped.starts_with('■'),
            "decorative glyphs must not return: {text:?}"
        );
        if text.contains("First token") {
            assert!(
                text.contains("+0.18s"),
                "First token row must carry its interval: {text:?}"
            );
        }
    }
}

/// A record with no transport telemetry has no connection moment, and inventing
/// one from the response head is what made the ladder misorder itself.
#[test]
fn test_attempt_inspector_omits_a_connection_moment_it_cannot_place() {
    let theme = Theme::from_color_scheme("dark", &Default::default());
    let rounds = vec![TelemetryRound {
        round_number: 1,
        prompt_tokens: 4000,
        completion_tokens: 300,
        cache_read_tokens: 0,
        total_tokens: 4300,
        turns_count: 1,
        e2e_duration_ms: 3_500,
        attempts: vec![TelemetryAttempt {
            round: 1,
            turn: 1,
            attempt: 1,
            model: "claude-3-7-sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: RequestUsageStatus::Completed,
            prompt_tokens: 4000,
            completion_tokens: 300,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            performance: Some(RequestPerformance {
                stream_ready_us: Some(120_000),
                ttft_us: Some(280_000),
                stream_us: Some(3_000_000),
                output_events: 10,
                tail_us: Some(25_000),
                e2e_us: Some(3_425_000),
                streamed_output_tokens: 300,
                first_output_tokens: 0,
                // Observation stays `Unreported`: nothing watched the socket.
                ..Default::default()
            }),
            e2e_duration_ms: 3500,
            started_at_ms: 200,
        }],
    }];

    let full_text: String = build_attempt_inspector_body(
        &rounds,
        1,
        1,
        ContextUsageProps {
            window_tokens: Some(200_000),
            ..Default::default()
        },
        100,
        Some(0),
        &theme,
    )
    .iter()
    .map(|line| {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    })
    .collect::<Vec<_>>()
    .join("\n");

    assert!(!full_text.contains("Connected"), "{full_text}");
    assert!(!full_text.contains("reused"), "{full_text}");
    assert!(!full_text.contains("cold start"), "{full_text}");
    // The rest of the ladder is unaffected: the head moment still carries its
    // own timestamp through `Server started`.
    assert!(full_text.contains("Server started"), "{full_text}");
    assert!(
        full_text.contains("headers 120ms from dispatch"),
        "{full_text}"
    );
    // An unsampled socket shows no retransmit claim at all: a zero here would
    // read as a clean socket rather than an untouched field.
    assert!(!full_text.contains("retransmits"), "{full_text}");
}

#[test]
fn test_build_overview_and_sticky_table_headers() {
    let theme = Theme::from_color_scheme("dark", &Default::default());
    let rounds = vec![TelemetryRound {
        round_number: 1,
        prompt_tokens: 4000,
        completion_tokens: 300,
        cache_read_tokens: 3000,
        total_tokens: 4300,
        turns_count: 1,
        e2e_duration_ms: 3_500,
        attempts: vec![TelemetryAttempt {
            round: 1,
            turn: 1,
            attempt: 1,
            model: "claude-3-7-sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: RequestUsageStatus::Completed,
            prompt_tokens: 4000,
            completion_tokens: 300,
            cache_read_tokens: 3000,
            cache_write_tokens: 500,
            performance: Some(RequestPerformance {
                stream_ready_us: Some(120_000),
                ttft_us: Some(280_000),
                stream_us: Some(3_000_000),
                output_events: 10,
                tail_us: Some(25_000),
                e2e_us: Some(3_425_000),
                streamed_output_tokens: 300,
                first_output_tokens: 0,
                ..Default::default()
            }),
            e2e_duration_ms: 3500,
            started_at_ms: 0,
        }],
    }];

    let report = TokenSourceReport {
        rows: Vec::new(),
        grand_total: nuo_contracts::TokenSourceTotals {
            prompt_tokens: 4000,
            completion_tokens: 300,
            cache_read_tokens: 3000,
            cache_write_tokens: 500,
            reported_tokens: 4300,
            ..Default::default()
        },
    };

    // 1. Test Overview Tab
    let overview = build_overview_body(
        &report,
        &rounds,
        ContextUsageProps {
            snapshot: Some(nuo_contracts::ContextTokenSnapshot {
                tokens: 24_500,
                source: nuo_contracts::ContextTokenSource::Api,
                overhead_tokens: None,
                history_tokens: None,
                temporary_context_tokens: None,
            }),
            window_tokens: Some(200_000),
            draft_content_tokens: 50,
            draft_tokens: 60,
        },
        80,
        &theme,
    );
    let ov_text: String = overview
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(ov_text.contains("Context window"));
    assert!(ov_text.contains("Used Tokens"));
    assert!(ov_text.contains("24,500 tokens (12.2%)"));
    assert!(ov_text.contains("Capacity"));
    assert!(ov_text.contains("200,000 tokens"));
    assert!(ov_text.contains("Session token totals"));
    assert!(ov_text.contains("Grand Total"));
    assert!(ov_text.contains("4.3k (4,300)"));
    assert!(ov_text.contains("75.0% hit rate"));
    assert!(ov_text.contains("Streaming performance"));
    assert!(ov_text.contains("Streaming Rate"));
    assert!(ov_text.contains("TTFT (median)"));

    // 2. Test Rounds Sticky Table (Header is separated from Rows)
    let (header, rows, follow) = build_rounds_table(&rounds, 0, 80, &theme);
    assert_eq!(header.len(), 1, "header must be 1 fixed row");
    assert_eq!(rows.len(), 1, "rows must contain only data lines");
    assert_eq!(follow, Some(0));

    let header_str = header[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(header_str.contains("Round"));
    assert!(header_str.contains("Tokens"));
    assert!(header_str.contains("Stream TPS"));

    // 3. Test Turns Sticky Table
    let (turns_header, turns_rows, turn_follow) = build_turns_table(&rounds, 0, 0, 80, &theme);
    assert_eq!(turns_header.len(), 1);
    assert_eq!(turns_rows.len(), 1);
    assert_eq!(turn_follow, Some(0));

    let turns_header_str = turns_header[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(turns_header_str.contains("Turn"));
    assert!(turns_header_str.contains("TTFT"));
    assert!(turns_header_str.contains("Status"));

    // 4. Test Tab Strip
    let ov_tab = tab_strip_line(TelemetryTab::Overview, 1, &theme);
    let ov_tab_str = ov_tab
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(ov_tab_str.contains("[ 1 Overview ]"));
    assert!(ov_tab_str.contains("2 Activity (1)"));

    let act_tab = tab_strip_line(TelemetryTab::Activity, 1, &theme);
    let act_tab_str = act_tab
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(act_tab_str.contains("1 Overview"));
    assert!(act_tab_str.contains("[ 2 Activity (1) ]"));
}

#[test]
fn test_telemetry_burst_arrival_defensible_tps_fallback() {
    let theme = Theme::from_color_scheme("dark", &Default::default());
    // Simulates a provider (e.g. Gemini) returning 200 tokens in a sub-20ms burst (200µs).
    // Naive division 200 / 0.0002s would give 1,000,000 tok/s.
    // Defensible calculation should filter the burst and fall back to e2e rate (200 / 1.5s = 133.3 tok/s).
    let burst_attempt = TelemetryAttempt {
        round: 1,
        turn: 1,
        attempt: 1,
        model: "gemini-2.5-pro".to_string(),
        provider: "google".to_string(),
        status: RequestUsageStatus::Completed,
        prompt_tokens: 1000,
        completion_tokens: 200,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        performance: Some(RequestPerformance {
            stream_ready_us: Some(50_000),
            ttft_us: Some(200_000),
            stream_us: Some(200),    // sub-20ms burst!
            output_events: 1,        // single event arrival
            e2e_us: Some(1_500_000), // 1.5s total e2e
            streamed_output_tokens: 200,
            first_output_tokens: 200,
            ..Default::default()
        }),
        e2e_duration_ms: 1500,
        started_at_ms: 0,
    };

    // A burst has no defensible rate: there is exactly one scheme, and it
    // refuses rather than substituting a different question's answer.
    assert!(
        burst_attempt.stream_tps().is_none(),
        "a sub-20 ms span must report no rate"
    );

    let rounds = vec![TelemetryRound {
        round_number: 1,
        prompt_tokens: 1000,
        completion_tokens: 200,
        cache_read_tokens: 0,
        total_tokens: 1200,
        turns_count: 1,
        e2e_duration_ms: 1500,
        attempts: vec![burst_attempt],
    }];

    // A single sub-20 ms span has no defensible rate: the round reports `–`
    // rather than falling back to an end-to-end number.
    assert!(rounds[0].stream_tps().is_none());

    let report = TokenSourceReport::default();
    let overview = build_overview_body(&report, &rounds, ContextUsageProps::default(), 80, &theme);
    let ov_text = overview
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        ov_text.contains("Streaming Rate") && ov_text.contains("–"),
        "Overview must show the rate column with an honest dash: {ov_text}"
    );
}

#[test]
fn test_fmt_num_separators() {
    assert_eq!(fmt_num(0), "0");
    assert_eq!(fmt_num(999), "999");
    assert_eq!(fmt_num(1000), "1,000");
    assert_eq!(fmt_num(10000), "10,000");
    assert_eq!(fmt_num(200000), "200,000");
    assert_eq!(fmt_num(1234567), "1,234,567");
    assert_eq!(fmt_num(-10000), "-10,000");
}

#[test]
fn test_round_turns_sorted_descending_and_turn_labels() {
    let theme = Theme::from_color_scheme("dark", &Default::default());
    let mut report = TokenSourceReport::default();
    let row = TokenSourceRow {
        provider: "anthropic".to_string(),
        model: "claude-3-7-sonnet".to_string(),
        turns: Vec::new(),
        requests: vec![
            RequestUsageRecord {
                key: RequestUsageKey {
                    session_id: "s1".to_string(),
                    round: 4,
                    turn: 1,
                    attempt: 1,
                    actor_id: "root".to_string(),
                },
                provider: "anthropic".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                status: RequestUsageStatus::Completed,
                prompt_tokens: 100,
                completion_tokens: 10,
                generation_ms: 100,
                ..Default::default()
            },
            RequestUsageRecord {
                key: RequestUsageKey {
                    session_id: "s1".to_string(),
                    round: 4,
                    turn: 2,
                    attempt: 1,
                    actor_id: "root".to_string(),
                },
                provider: "anthropic".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                status: RequestUsageStatus::Completed,
                prompt_tokens: 200,
                completion_tokens: 20,
                generation_ms: 200,
                ..Default::default()
            },
        ],
        totals: Default::default(),
    };
    report.rows.push(row);

    let rounds = extract_telemetry_rounds(&report);
    assert_eq!(rounds.len(), 1);
    let r = &rounds[0];
    assert_eq!(r.round_number, 4);
    // Turns sorted descending: turn 2 first, then turn 1
    assert_eq!(r.attempts.len(), 2);
    assert_eq!(r.attempts[0].turn, 2);
    assert_eq!(r.attempts[1].turn, 1);

    // Build turns table: check first column shows #2 and #1, without redundant "Turn 4.1"
    let (_, rows, _) = build_turns_table(&rounds, 0, 0, 80, &theme);
    assert_eq!(rows.len(), 2);
    let row0_text = rows[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    let row1_text = rows[1]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(row0_text.contains("#2"));
    assert!(!row0_text.contains("Turn 4.2"));
    assert!(row1_text.contains("#1"));
    assert!(!row1_text.contains("Turn 4.1"));
}
