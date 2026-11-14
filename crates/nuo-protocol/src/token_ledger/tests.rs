//! Token ledger tests.

use super::*;
use std::sync::Mutex as StdMutex;

/// Collecting sink for tests: records everything it receives.
#[derive(Default)]
struct CollectingSink {
    received: StdMutex<Vec<(u64, String, RequestUsageRecord)>>,
}

impl UsageStatSink for CollectingSink {
    fn persist_usage<'a>(
        &'a self,
        recorded_at_ms: u64,
        project: &'a str,
        record: RequestUsageRecord,
    ) -> futures::future::BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.received
                .lock()
                .unwrap()
                .push((recorded_at_ms, project.to_string(), record));
            Ok(())
        })
    }
}

#[test]
fn settled_requests_are_mirrored_to_the_usage_sink() {
    let ledger = TokenSourceLedger::new();
    let sink = Arc::new(CollectingSink::default());
    ledger.install_usage_sink(sink.clone());
    ledger.set_usage_project("bucket-42");

    let key = ledger.begin_request("s1", "openai", "gpt", 3, 1, 1_000);
    ledger.settle_request(
        &key,
        RequestUsageStatus::Completed,
        Some(nuo_tool::TokenUsage {
            prompt_tokens: 900,
            completion_tokens: 100,
            total_tokens: 1_000,
            ..Default::default()
        }),
        0,
        2_000,
    );
    // A duplicate terminal event must not mirror twice (the reported
    // idempotency fence fires before the sink forward).
    ledger.settle_request(&key, RequestUsageStatus::Failed, None, 5, 0);

    futures::executor::block_on(ledger.persist_request(&key)).unwrap();
    let received = sink.received.lock().unwrap();
    assert_eq!(received.len(), 1, "one terminal settle → one sink record");
    assert_eq!(received[0].1, "bucket-42");
    assert_eq!(received[0].2.total_tokens, 1_000);
    assert_eq!(received[0].2.status, RequestUsageStatus::Completed);
}

#[test]
fn ledger_without_sink_still_settles() {
    let ledger = TokenSourceLedger::new();
    let key = ledger.begin_request("s1", "openai", "gpt", 1, 1, 100);
    ledger.settle_request(&key, RequestUsageStatus::Failed, None, 10, 0);
    assert_eq!(ledger.records_for_session("s1").len(), 1);
}

#[test]
fn lifecycle_attempts_are_keyed_idempotent_and_session_scoped() {
    let ledger = TokenSourceLedger::new();
    let first = ledger.begin_request("s1", "openai", "gpt", 3, 1, 1_000);
    let retry = ledger.begin_request("s1", "openai", "gpt", 3, 1, 1_000);
    let other = ledger.begin_request("s2", "anthropic", "claude", 1, 1, 500);
    let subagent = ledger.begin_request_for_actor(BeginRequestParams {
        session_id: "s1",
        actor_id: "subagent:call-1",
        provider: "openai",
        model: "gpt",
        round: 3,
        turn: 1,
        projected_prompt_tokens: 300,
        started_at_ms: 1_700_000_000_000,
    });
    assert_eq!(first.attempt, 1);
    assert_eq!(retry.attempt, 2);
    assert_eq!(other.attempt, 1);
    assert_eq!(subagent.attempt, 1, "a distinct actor has its own attempts");

    ledger.settle_request(&first, RequestUsageStatus::Failed, None, 25, 0);
    ledger.settle_request(
        &retry,
        RequestUsageStatus::Completed,
        Some(nuo_tool::TokenUsage {
            prompt_tokens: 990,
            completion_tokens: 110,
            total_tokens: 1_100,
            ..Default::default()
        }),
        0,
        2_000,
    );
    // A duplicate weaker terminal event cannot downgrade reported usage.
    ledger.settle_request(&retry, RequestUsageStatus::Failed, None, 999, 9_999);
    ledger.settle_request(&subagent, RequestUsageStatus::Completed, None, 30, 1_000);

    let report = ledger.snapshot_for_session("s1");
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].requests.len(), 3);
    assert_eq!(report.rows[0].totals.reported_tokens, 1_100);
    assert_eq!(report.rows[0].totals.estimated_tokens, 1_355);
    assert_eq!(
        report.rows[0].requests[1].status,
        RequestUsageStatus::Completed
    );
    assert_eq!(
        report.rows[0].requests[1].source,
        RequestUsageSource::Reported
    );
    // The per-attempt generation span is booked at settle, and the
    // idempotency fence keeps a replayed settle from overwriting it.
    assert_eq!(report.rows[0].requests[1].generation_ms, 2_000);
    assert_eq!(report.rows[0].requests[0].generation_ms, 0);
}

#[test]
fn attempt_records_timestamp_and_error() {
    let ledger = TokenSourceLedger::new();
    let key = ledger.begin_request("s1", "p1", "m1", 1, 1, 500);
    assert!(ledger.records_for_session("s1")[0].started_at_ms > 0);

    ledger.settle_request_with_error(
        &key,
        RequestUsageStatus::Failed,
        None,
        0,
        120,
        Some("429 Too Many Requests: Rate limit exceeded".to_string()),
    );

    let records = ledger.records_for_session("s1");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, RequestUsageStatus::Failed);
    assert_eq!(records[0].generation_ms, 120);
    assert_eq!(
        records[0].error.as_deref(),
        Some("429 Too Many Requests: Rate limit exceeded")
    );
}

#[test]
fn restore_marks_crash_residue_abandoned() {
    let ledger = TokenSourceLedger::new();
    let key = RequestUsageKey {
        session_id: "old".to_string(),
        actor_id: "master".to_string(),
        round: 4,
        turn: 2,
        attempt: 1,
    };
    ledger.restore_session(
        "restored",
        vec![RequestUsageRecord {
            key,
            provider: "relay".to_string(),
            model: "model".to_string(),
            status: RequestUsageStatus::InFlight,
            projected_prompt_tokens: 700,
            ..Default::default()
        }],
    );
    let records = ledger.records_for_session("restored");
    assert_eq!(records[0].status, RequestUsageStatus::Abandoned);
    assert_eq!(records[0].source, RequestUsageSource::Estimated);
    assert_eq!(records[0].prompt_tokens, 700);
    assert_eq!(records[0].total_tokens, 700);
}

/// The quadratic `observe_output` bug (summing `StreamingCounter::push`'s
/// *running total* once per delta) persisted absurd completion counts on
/// interrupted/failed attempts — e.g. a real turn booked 14 786 219
/// completion tokens over 113 s and rendered as 130 050 tok/s. Both the
/// load path and the settle path must repair such records, judging by the
/// implied rate (real models peak ≈138 tok/s; the ceiling is 10 000).
#[test]
fn implausible_estimated_completion_is_repaired() {
    // Settle path: a caller passing a poisoned estimate is clamped.
    let ledger = TokenSourceLedger::new();
    let key = ledger.begin_request("s1", "p1", "m1", 1, 1, 800);
    ledger.settle_request(
        &key,
        RequestUsageStatus::Interrupted,
        None,
        14_786_219,
        113_696,
    );
    let records = ledger.records_for_session("s1");
    assert_eq!(records[0].status, RequestUsageStatus::Interrupted);
    assert_eq!(records[0].completion_tokens, 0);
    assert_eq!(records[0].total_tokens, records[0].prompt_tokens);

    // A *small* poisoned count whose implied rate is still impossible
    // (2 393 tokens in 2.165 s → 1 105 tok/s... is under the 10 000
    // ceiling and survives; 9 500 in 2 s → 4 750 tok/s also survives).
    // The rate ceiling only fires far beyond physical reality, so these
    // remain — the ceiling catches the quadratic blow-up (which always
    // rockets past 10 000 tok/s within a few hundred deltas), not
    // merely-fast streams.
    let ledger = TokenSourceLedger::new();
    let key = ledger.begin_request("s2", "p1", "m1", 1, 1, 800);
    ledger.settle_request(&key, RequestUsageStatus::Interrupted, None, 9_500, 2_000);
    let records = ledger.records_for_session("s2");
    assert_eq!(records[0].completion_tokens, 9_500);

    // A count implying >10 000 tok/s is repaired even at modest size.
    let ledger = TokenSourceLedger::new();
    let key = ledger.begin_request("s3", "p1", "m1", 1, 1, 800);
    ledger.settle_request(&key, RequestUsageStatus::Failed, None, 25_000, 2_000);
    let records = ledger.records_for_session("s3");
    assert_eq!(records[0].completion_tokens, 0);
    assert_eq!(records[0].total_tokens, 800);

    // Untimed poison: an eight-figure count with no measured span is
    // repaired via the absolute companion ceiling.
    let ledger = TokenSourceLedger::new();
    let key = ledger.begin_request("s4", "p1", "m1", 1, 1, 800);
    ledger.settle_request(&key, RequestUsageStatus::Failed, None, 98_732_687, 0);
    let records = ledger.records_for_session("s4");
    assert_eq!(records[0].completion_tokens, 0);

    // Load path: a poisoned record persisted by an older build is
    // repaired on restore.
    let ledger = TokenSourceLedger::new();
    let poisoned = RequestUsageRecord {
        key: RequestUsageKey {
            session_id: "old".to_string(),
            actor_id: "master".to_string(),
            round: 1,
            turn: 44,
            attempt: 1,
        },
        provider: "relay".to_string(),
        model: "model".to_string(),
        status: RequestUsageStatus::Interrupted,
        source: RequestUsageSource::Estimated,
        projected_prompt_tokens: 64_572,
        prompt_tokens: 64_572,
        completion_tokens: 14_786_219,
        total_tokens: 14_850_791,
        generation_ms: 113_696,
        ..Default::default()
    };
    ledger.restore_session("repaired", vec![poisoned]);
    let records = ledger.records_for_session("repaired");
    assert_eq!(records[0].completion_tokens, 0);
    assert_eq!(records[0].total_tokens, 64_572);
    // The generation span survives — the *rate* column falls back to `–`
    // (zero completion), not a fabricated figure.

    // A plausible estimated completion is untouched.
    let plausible = RequestUsageRecord {
        key: RequestUsageKey {
            session_id: "old".to_string(),
            actor_id: "master".to_string(),
            round: 2,
            turn: 1,
            attempt: 1,
        },
        provider: "relay".to_string(),
        model: "model".to_string(),
        status: RequestUsageStatus::Completed,
        source: RequestUsageSource::Estimated,
        projected_prompt_tokens: 1_000,
        prompt_tokens: 1_000,
        completion_tokens: 2_400,
        total_tokens: 3_400,
        generation_ms: 40_000,
        ..Default::default()
    };
    ledger.restore_session("plausible", vec![plausible.clone()]);
    let restored = ledger.records_for_session("plausible");
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].completion_tokens, 2_400);
    assert_eq!(restored[0].total_tokens, 3_400);

    // A provider-reported count is authoritative and never clamped.
    let reported = RequestUsageRecord {
        key: RequestUsageKey {
            session_id: "old".to_string(),
            actor_id: "master".to_string(),
            round: 3,
            turn: 1,
            attempt: 1,
        },
        provider: "relay".to_string(),
        model: "model".to_string(),
        status: RequestUsageStatus::Completed,
        source: RequestUsageSource::Reported,
        prompt_tokens: 50_000,
        completion_tokens: 12_000_000,
        total_tokens: 12_050_000,
        generation_ms: 600_000,
        ..Default::default()
    };
    ledger.restore_session("reported", vec![reported.clone()]);
    let restored = ledger.records_for_session("reported");
    assert_eq!(restored.len(), 1);
    // Authoritative provider counts are never clamped.
    assert_eq!(restored[0].completion_tokens, 12_000_000);
    assert_eq!(restored[0].total_tokens, 12_050_000);
}

#[test]
fn records_reported_and_estimated_separately() {
    let ledger = TokenSourceLedger::new();
    ledger.record("openai", "gpt-4o", 100, true);
    ledger.record("openai", "gpt-4o", 50, false);
    let report = ledger.snapshot();
    assert_eq!(report.rows.len(), 1);
    let row = &report.rows[0];
    assert_eq!(row.provider, "openai");
    assert_eq!(row.model, "gpt-4o");
    assert_eq!(row.totals.reported_tokens, 100);
    assert_eq!(row.totals.estimated_tokens, 50);
    assert_eq!(row.totals.total(), 150);
}

#[test]
fn separates_providers_and_models() {
    let ledger = TokenSourceLedger::new();
    ledger.record("openai", "gpt-4o", 100, true);
    ledger.record("google", "gemini-2.5", 80, true);
    ledger.record("kimi", "k2", 30, false);
    let report = ledger.snapshot();
    assert_eq!(report.rows.len(), 3);
    assert_eq!(report.grand_total.reported_tokens, 180);
    assert_eq!(report.grand_total.estimated_tokens, 30);
}

#[test]
fn ignores_non_positive_tokens() {
    let ledger = TokenSourceLedger::new();
    ledger.record("openai", "gpt-4o", 0, true);
    ledger.record("openai", "gpt-4o", -5, false);
    assert!(ledger.snapshot().rows.is_empty());
}

#[test]
fn snapshot_is_stable_order() {
    let ledger = TokenSourceLedger::new();
    ledger.record("zeta", "z1", 10, true);
    ledger.record("alpha", "a1", 10, true);
    let report = ledger.snapshot();
    // BTreeMap keeps alphabetical order by the composite key.
    assert_eq!(report.rows[0].provider, "alpha");
    assert_eq!(report.rows[1].provider, "zeta");
}

#[test]
fn round_trips_provider_model_containing_the_old_separator() {
    // Regression: the old `\u{1f}`-joined string key would mis-split a
    // provider/model that itself contained the separator byte. A tuple key
    // makes the boundary structural and unambiguous.
    let ledger = TokenSourceLedger::new();
    ledger.record("custom\u{1f}relay", "model\u{1f}v2", 40, true);
    let row = &ledger.snapshot().rows[0];
    assert_eq!(row.provider, "custom\u{1f}relay");
    assert_eq!(row.model, "model\u{1f}v2");
    assert_eq!(row.totals.reported_tokens, 40);
}

#[test]
fn record_reported_books_cache_breakout() {
    // The cache-aware overload folds write/read into reported_tokens AND
    // accumulates them as a separate breakout, so the report can show
    // hit-rate without losing the real billed total.
    let ledger = TokenSourceLedger::new();
    // Turn 1: a cache write (the first turn populates the cache).
    ledger.record_reported("anthropic", "claude-sonnet-4-5", 13200, 5000, 0);
    // Turn 2: a cache read (subsequent turn hits the cache).
    ledger.record_reported("anthropic", "claude-sonnet-4-5", 8200, 0, 8000);
    let row = &ledger.snapshot().rows[0];
    assert_eq!(
        row.totals.reported_tokens, 21400,
        "all reported tokens summed"
    );
    assert_eq!(row.totals.cache_write_tokens, 5000);
    assert_eq!(row.totals.cache_read_tokens, 8000);
    assert_eq!(row.totals.estimated_tokens, 0);
}

#[test]
fn record_reported_clamps_negative_cache_counts() {
    // A malformed usage object shouldn't corrupt the ledger: negative cache
    // counts are clamped to zero rather than subtracting from the total.
    let ledger = TokenSourceLedger::new();
    ledger.record_reported("anthropic", "claude", 1000, -50, -10);
    let row = &ledger.snapshot().rows[0];
    assert_eq!(row.totals.reported_tokens, 1000);
    assert_eq!(row.totals.cache_write_tokens, 0);
    assert_eq!(row.totals.cache_read_tokens, 0);
}

#[test]
fn record_reported_ignores_non_positive_total() {
    // Parity with the plain `record` guard: a zero/negative total is a
    // no-op even when cache counts are present.
    let ledger = TokenSourceLedger::new();
    ledger.record_reported("anthropic", "claude", 0, 100, 200);
    ledger.record_reported("anthropic", "claude", -5, 100, 200);
    assert!(ledger.snapshot().rows.is_empty());
}

#[test]
fn grand_total_aggregates_cache_counters() {
    // `snapshot` folds cache counters into the grand total via `add`, so a
    // multi-provider report surfaces the session-wide cache hit volume.
    let ledger = TokenSourceLedger::new();
    ledger.record_reported("anthropic", "claude-opus", 5000, 1000, 3000);
    ledger.record_reported("openai", "gpt-4o", 2000, 0, 0);
    let report = ledger.snapshot();
    assert_eq!(report.grand_total.reported_tokens, 7000);
    assert_eq!(report.grand_total.cache_write_tokens, 1000);
    assert_eq!(report.grand_total.cache_read_tokens, 3000);
}

#[test]
fn record_keeps_per_round_line_items() {
    // Each booking appends an ordered line item and splits input/output for
    // reported turns, powering the detail drill-in.
    let ledger = TokenSourceLedger::new();
    ledger.record_turn(
        "anthropic",
        "claude",
        TokenTurn {
            turn: 0,
            round: 0,
            reported: true,
            prompt_tokens: 1000,
            completion_tokens: 200,
            total_tokens: 1200,
            cache_write_tokens: 800,
            cache_read_tokens: 0,
            cache_miss_tokens: 0,
            reasoning_tokens: 0,
        },
    );
    ledger.record("anthropic", "claude", 50, false);
    let row = &ledger.snapshot().rows[0];
    assert_eq!(row.turns.len(), 2);
    assert!(row.turns[0].reported);
    assert_eq!(row.turns[0].prompt_tokens, 1000);
    assert_eq!(row.turns[0].completion_tokens, 200);
    assert!(!row.turns[1].reported);
    assert_eq!(row.turns[1].total_tokens, 50);
    assert_eq!(row.totals.prompt_tokens, 1000);
    assert_eq!(row.totals.completion_tokens, 200);
    assert_eq!(row.totals.reported_tokens, 1200);
    assert_eq!(row.totals.estimated_tokens, 50);
}

#[test]
fn last_reported_turn_returns_most_recent_reported_for_key() {
    // The context meter anchors on the newest reported turn for the
    // active (provider, model). Estimated turns are skipped, other keys
    // are ignored, and the most-recent reported turn wins.
    let ledger = TokenSourceLedger::new();
    // Older reported turn for the active key.
    ledger.record_turn(
        "openai",
        "gpt-4o",
        TokenTurn {
            reported: true,
            prompt_tokens: 500,
            completion_tokens: 50,
            total_tokens: 550,
            ..Default::default()
        },
    );
    // A stray estimated turn for the active key must not be returned.
    ledger.record("openai", "gpt-4o", 40, false);
    // Noise in a different key.
    ledger.record_reported("anthropic", "claude", 9999, 0, 0);

    // Newest reported turn for the active key.
    ledger.record_turn(
        "openai",
        "gpt-4o",
        TokenTurn {
            reported: true,
            prompt_tokens: 4000,
            completion_tokens: 300,
            total_tokens: 4300,
            ..Default::default()
        },
    );

    let last = ledger
        .last_reported_turn("openai", "gpt-4o")
        .expect("a reported turn exists for the key");
    assert_eq!(last.prompt_tokens, 4000);
    assert_eq!(last.completion_tokens, 300);
    assert_eq!(last.total_tokens, 4300);

    // Missing key / never-reported key -> None.
    assert!(ledger.last_reported_turn("openai", "gpt-5").is_none());
    assert!(ledger.last_reported_turn("mistral", "large").is_none());
}

fn sample_performance() -> RequestPerformance {
    RequestPerformance {
        stream_ready_us: Some(41_000),
        ttft_us: Some(120_000),
        stream_us: Some(1_000_000),
        tail_us: Some(8_000),
        e2e_us: Some(1_128_000),
        streamed_output_tokens: 101,
        first_output_tokens: 1,
        output_events: 101,
        timing_source: PerformanceTimingSource::ClientObserved,
        stream_token_source: StreamTokenSource::Cl100k,
        ..Default::default()
    }
}

#[test]
fn performance_settlement_stores_telemetry_and_rate_helpers() {
    let ledger = TokenSourceLedger::new();
    let key = ledger.begin_request("s1", "openai", "gpt-4o", 1, 1, 100);
    ledger.settle_request_with_performance_and_error(
        &key,
        RequestUsageStatus::Completed,
        Some(nuo_tool::TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 101,
            total_tokens: 201,
            ..Default::default()
        }),
        0,
        1_128,
        Some(sample_performance()),
        None,
    );

    let record = &ledger.records_for_session("s1")[0];
    let performance = record.performance.expect("telemetry stored");
    assert_eq!(performance.ttft_us, Some(120_000));
    assert_eq!(performance.output_events, 101);

    // One rate: the attempt's completion count over the first→last token
    // span. 101 tokens over 1_000_000 µs → exactly 101 tok/s.
    let stream = performance
        .stream_tps(record.completion_tokens)
        .expect("stream rate");
    assert!((stream - 101.0).abs() < f64::EPSILON);
    // No provider-native telemetry yet → decode rate stays absent, never 0.
    assert_eq!(performance.provider_decode_tps(), None);

    // The hint-bar snapshot projection carries the same sample.
    let snapshot = record.performance_snapshot().expect("snapshot");
    assert_eq!(snapshot.round, 1);
    assert_eq!(snapshot.completion_tokens, 101);
    assert_eq!(snapshot.stream_tps(), Some(stream));
}

#[test]
fn the_streaming_rate_is_one_division_with_honest_refusals() {
    // 100 tokens over 1 s → 100 tok/s. The reader can reproduce it.
    let normal = RequestPerformance {
        stream_us: Some(1_000_000),
        output_events: 101,
        ..Default::default()
    };
    assert_eq!(normal.stream_tps(100), Some(100.0));

    let snapshot = TurnPerformanceSnapshot {
        round: 1,
        turn: 1,
        attempt: 1,
        completion_tokens: 100,
        usage_source: RequestUsageSource::Reported,
        performance: normal,
    };
    assert_eq!(snapshot.stream_tps(), Some(100.0));

    // A single event has no span: `–`, never a fabricated rate.
    let single_chunk = RequestPerformance {
        stream_us: Some(0),
        output_events: 1,
        ..Default::default()
    };
    assert_eq!(single_chunk.stream_tps(100), None);

    // A sub-20 ms span is a burst, not a decode pace.
    let burst = RequestPerformance {
        stream_us: Some(5_000),
        output_events: 2,
        ..Default::default()
    };
    assert_eq!(burst.stream_tps(505), None);

    // No tokens → no rate.
    assert_eq!(normal.stream_tps(0), None);
    // A span the attempt never recorded → no rate.
    let untimed = RequestPerformance::default();
    assert_eq!(untimed.stream_tps(100), None);
}

#[test]
fn a_silent_transport_never_claims_a_pooled_connection() {
    // Nothing reported: no reuse and no handshake may be asserted. This is
    // the shape every record in the wild had, which is why the timeline
    // used to read "reused warm pool connection" for every cold handshake.
    // A timing measured by the agent says nothing about the connection: an
    // absent `dns_us` here means nobody watched the socket, not that there
    // was nothing to watch. This is the record shape every attempt in the
    // wild had, and the reason the timeline used to call them all "reused
    // warm pool connection".
    let silent = RequestPerformance {
        stream_ready_us: Some(120_000),
        ttft_us: Some(300_000),
        stream_us: Some(1_000_000),
        output_events: 10,
        ..Default::default()
    };
    assert!(!silent.transport_observed());
    assert_eq!(silent.pooled_connection(), None);
    assert_eq!(
        TurnPerformanceSnapshot {
            performance: silent,
            ..Default::default()
        }
        .pooled_connection(),
        None
    );

    // Observed, and no setup paid: a real pooled socket.
    let pooled = RequestPerformance {
        connected_us: Some(390_000),
        request_sent_us: Some(400_000),
        stream_ready_us: Some(500_000),
        observation: TransportObservation::PooledConnection,
        ..Default::default()
    };
    assert!(pooled.transport_observed());
    assert_eq!(pooled.pooled_connection(), Some(true));

    // Observed, and setup paid: a real cold start.
    let cold = RequestPerformance {
        dns_us: Some(20_000),
        tcp_us: Some(40_000),
        tls_us: Some(90_000),
        connected_us: Some(150_000),
        request_sent_us: Some(400_000),
        observation: TransportObservation::ColdConnection,
        ..Default::default()
    };
    assert_eq!(cold.pooled_connection(), Some(false));

    // Observed, and the socket was sampled: `retransmits` is a measurement.
    let sampled = RequestPerformance {
        rtt_us: Some(42_000),
        observation: TransportObservation::PooledConnection,
        ..Default::default()
    };
    assert!(sampled.tcp_info_sampled());

    // Observed, but no `TCP_INFO`: a zero retransmit count on this record
    // would be an untouched field, not a clean socket. This is the pair the
    // renderer keys on.
    let unsampled = RequestPerformance {
        observation: TransportObservation::ColdConnection,
        retransmits: 0,
        ..Default::default()
    };
    assert!(!unsampled.tcp_info_sampled());
}

#[test]
fn transport_observation_survives_a_json_round_trip_and_legacy_records_stay_unreported() {
    let perf = RequestPerformance {
        observation: TransportObservation::ColdConnection,
        connected_us: Some(150_000),
        ..Default::default()
    };
    let json = serde_json::to_string(&perf).expect("serialize");
    let restored: RequestPerformance = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(restored, perf);

    // A record written before the field existed must not read as "observed":
    // that would restore exactly the fabrication the field removes.
    let legacy: RequestPerformance =
        serde_json::from_str(r#"{"ttft_us":300000,"output_events":4}"#).expect("legacy");
    assert_eq!(legacy.observation, TransportObservation::Unreported);
    assert_eq!(legacy.pooled_connection(), None);
    assert_eq!(legacy.connected_us, None);
}

#[test]
fn legacy_settlement_keeps_performance_none_and_json_round_trips() {
    // Legacy path (no telemetry argument): the field must stay None, both
    // in memory and across a persist/reload JSON round trip that predates
    // the field (serde default).
    let ledger = TokenSourceLedger::new();
    let key = ledger.begin_request("s1", "openai", "gpt-4o", 1, 1, 100);
    ledger.settle_request(&key, RequestUsageStatus::Completed, None, 42, 900);
    let legacy = &ledger.records_for_session("s1")[0];
    assert!(legacy.performance.is_none());

    let json = serde_json::to_string(legacy).expect("serialize");
    let reloaded: RequestUsageRecord = serde_json::from_str(&json).expect("deserialize");
    assert!(reloaded.performance.is_none());
    assert_eq!(reloaded.generation_ms, 900);

    // A fresh JSON payload carrying telemetry round trips losslessly.
    let timed = RequestUsageRecord {
        key: legacy.key.clone(),
        status: RequestUsageStatus::Completed,
        source: RequestUsageSource::Reported,
        completion_tokens: 10,
        generation_ms: 500,
        performance: Some(sample_performance()),
        ..Default::default()
    };
    let json = serde_json::to_string(&timed).expect("serialize");
    let reloaded: RequestUsageRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(reloaded.performance, Some(sample_performance()));
}

#[test]
fn reported_upgrade_preserves_fresh_but_not_stale_performance() {
    // Terminal replay rules: an estimated settlement followed by a
    // reported one upgrades counts — and the newer call's telemetry wins
    // because it describes the authoritative measurement pass. A
    // *reported* record, however, can never be rewritten by a later
    // estimate replay, so its telemetry survives untouched too.
    let ledger = TokenSourceLedger::new();
    let key = ledger.begin_request("s1", "openai", "gpt-4o", 1, 1, 100);

    ledger.settle_request_with_performance_and_error(
        &key,
        RequestUsageStatus::Completed,
        None,
        40,
        800,
        Some(sample_performance()),
        None,
    );
    // Reported upgrade: usage + fresher telemetry replace the estimate.
    let upgraded = RequestPerformance {
        ttft_us: Some(90_000),
        ..sample_performance()
    };
    ledger.settle_request_with_performance_and_error(
        &key,
        RequestUsageStatus::Completed,
        Some(nuo_tool::TokenUsage {
            completion_tokens: 60,
            total_tokens: 160,
            ..Default::default()
        }),
        0,
        700,
        Some(upgraded),
        None,
    );
    let record = &ledger.records_for_session("s1")[0];
    assert_eq!(record.source, RequestUsageSource::Reported);
    assert_eq!(record.performance.map(|p| p.ttft_us), Some(Some(90_000)));

    // A later estimate replay can neither downgrade the usage nor the
    // settled telemetry.
    ledger.settle_request_with_performance_and_error(
        &key,
        RequestUsageStatus::Completed,
        None,
        99,
        5_000,
        None,
        None,
    );
    let record = &ledger.records_for_session("s1")[0];
    assert_eq!(record.source, RequestUsageSource::Reported);
    assert_eq!(record.completion_tokens, 60);
    assert_eq!(record.performance.map(|p| p.ttft_us), Some(Some(90_000)));
}

#[test]
fn latest_turn_performance_picks_newest_completed_master_attempt() {
    let ledger = TokenSourceLedger::new();
    let older = ledger.begin_request("s1", "openai", "gpt-4o", 1, 1, 0);
    ledger.settle_request_with_performance_and_error(
        &older,
        RequestUsageStatus::Completed,
        None,
        10,
        100,
        Some(sample_performance()),
        None,
    );
    // A non-completed newer attempt must not become the hint sample.
    let failed = ledger.begin_request("s1", "openai", "gpt-4o", 2, 1, 0);
    ledger.settle_request_with_performance_and_error(
        &failed,
        RequestUsageStatus::Failed,
        None,
        0,
        50,
        Some(sample_performance()),
        Some("boom".to_string()),
    );
    // A subagent actor is excluded even when completed.
    let subagent = ledger.begin_request_for_actor(BeginRequestParams {
        session_id: "s1",
        actor_id: "subagent:call_1",
        provider: "openai",
        model: "gpt-4o",
        round: 2,
        turn: 2,
        projected_prompt_tokens: 0,
        started_at_ms: 1_700_000_000_000,
    });
    ledger.settle_request_with_performance_and_error(
        &subagent,
        RequestUsageStatus::Completed,
        None,
        500,
        400,
        Some(sample_performance()),
        None,
    );
    // Newest completed master attempt.
    let newest = ledger.begin_request("s1", "openai", "gpt-4o", 2, 3, 0);
    let newest_performance = RequestPerformance {
        ttft_us: Some(777_000),
        ..sample_performance()
    };
    ledger.settle_request_with_performance_and_error(
        &newest,
        RequestUsageStatus::Completed,
        None,
        20,
        300,
        Some(newest_performance),
        None,
    );

    let report = ledger.snapshot();
    let latest = report.latest_turn_performance().expect("a master sample");
    assert_eq!((latest.round, latest.turn), (2, 3));
    assert_eq!(latest.performance.ttft_us, Some(777_000));

    // Slice counterpart agrees for persisted-record paths.
    let records = ledger.records_for_session("s1");
    let slice_latest = latest_turn_performance(&records).expect("slice");
    assert_eq!(slice_latest, latest);

    // Nothing qualified → None rather than a fabricated default sample.
    let empty = TokenSourceLedger::new();
    assert!(empty.snapshot().latest_turn_performance().is_none());
}
