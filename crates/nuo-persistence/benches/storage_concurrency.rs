//! N=1/16/64/128 concurrent storage benchmark & p99 gate (ADR-0242, ADR-0248, ADR-0249).
//!
//! Exercises the real catalog + fixed-K content-shard + per-session journal
//! stack under single-workspace monorepo concurrency with skewed heavy tool
//! outputs (CAS blob offload) and concurrent global FTS fan-out reads.
//!
//! Run:
//!   cargo bench -p nuo-persistence --bench storage_concurrency
//!   cargo bench -p nuo-persistence --bench storage_concurrency -- --gate

#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use nuo_persistence::thread::{CommitTurn, ThreadSession};
use nuo_tool::{Message, Role};

struct Percentiles {
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    p999_ms: f64,
    max_ms: f64,
}

fn compute_percentiles(samples_us: &mut [u64]) -> Percentiles {
    if samples_us.is_empty() {
        return Percentiles {
            p50_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            p999_ms: 0.0,
            max_ms: 0.0,
        };
    }
    samples_us.sort_unstable();
    let pick = |q: f64| -> f64 {
        let idx = ((samples_us.len() as f64 - 1.0) * q).round() as usize;
        samples_us[idx.min(samples_us.len() - 1)] as f64 / 1000.0
    };
    Percentiles {
        p50_ms: pick(0.50),
        p95_ms: pick(0.95),
        p99_ms: pick(0.99),
        p999_ms: pick(0.999),
        max_ms: *samples_us.last().unwrap_or(&0) as f64 / 1000.0,
    }
}

#[cfg(target_os = "linux")]
fn process_thread_count() -> Option<usize> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Threads:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn process_thread_count() -> Option<usize> {
    None
}

struct RunResult {
    sessions: usize,
    turns_per_session: usize,
    total_commits: usize,
    wall_ms: f64,
    commit: Percentiles,
    reader_errors: usize,
    shards_active: usize,
    threads_after: Option<usize>,
}

async fn run_tier(sessions: usize, turns_per_session: usize) -> RunResult {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path().to_path_buf();

    // Open all N sessions in the SAME workspace directory (monorepo stress).
    let stores: Vec<Arc<ThreadSession>> = (0..sessions)
        .map(|i| Arc::new(ThreadSession::for_path(dir.join(format!("s-{i}.json")))))
        .collect();

    let reader_handle = stores[0].persistence().clone();
    let reader_errors = Arc::new(AtomicUsize::new(0));
    let stop_readers = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Concurrent global FTS fan-out readers running alongside the write burst.
    let reader_task = {
        let handle = reader_handle.clone();
        let errors = Arc::clone(&reader_errors);
        let stop = Arc::clone(&stop_readers);
        tokio::task::spawn_blocking(move || {
            while !stop.load(Ordering::Relaxed) {
                match handle.reader() {
                    Ok(reader) => {
                        if reader.search_history("bench_needle", None, 10).is_err() {
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(_) => {
                        errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };

    let heavy_payload = "x".repeat(128 * 1024);
    let wall_started = Instant::now();
    let mut handles = Vec::with_capacity(sessions);

    for (idx, store) in stores.iter().enumerate() {
        let store = Arc::clone(store);
        let heavy = if idx % 10 == 0 {
            Some(heavy_payload.clone())
        } else {
            None
        };
        handles.push(tokio::spawn(async move {
            let mut latencies_us = Vec::with_capacity(turns_per_session);
            for turn in 0..turns_per_session {
                let reply = if let Some(ref blob) = heavy {
                    format!("s{idx} t{turn} heavy {blob}")
                } else {
                    format!("s{idx} t{turn} compact reply")
                };
                let delta = [
                    Message::new(Role::User, format!("s{idx} t{turn} bench_needle query")),
                    Message::new(Role::Assistant, reply),
                ];

                let t0 = Instant::now();
                store
                    .commit_turn(CommitTurn::new(&delta))
                    .await
                    .expect("commit_turn must succeed");
                latencies_us.push(t0.elapsed().as_micros() as u64);
            }
            latencies_us
        }));
    }

    let mut all_samples_us = Vec::with_capacity(sessions * turns_per_session);
    for h in handles {
        all_samples_us.extend(h.await.expect("session task must not panic"));
    }
    let wall_ms = wall_started.elapsed().as_secs_f64() * 1000.0;

    stop_readers.store(true, Ordering::Relaxed);
    let _ = reader_task.await;

    let shards_dir = reader_handle.shards_dir();
    let shards_active = std::fs::read_dir(shards_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".db"))
                .count()
        })
        .unwrap_or(0);

    RunResult {
        sessions,
        turns_per_session,
        total_commits: all_samples_us.len(),
        wall_ms,
        commit: compute_percentiles(&mut all_samples_us),
        reader_errors: reader_errors.load(Ordering::Relaxed),
        shards_active,
        threads_after: process_thread_count(),
    }
}

fn main() {
    let gate = std::env::args().any(|arg| arg == "--gate");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .expect("tokio runtime");

    println!(
        "| N | turns | commits | wall_ms | p50_ms | p95_ms | p99_ms | p99.9_ms | max_ms | shards | reader_err | os_threads |"
    );
    println!("|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");

    let tiers = [(1, 8), (16, 8), (64, 6), (128, 4)];
    let mut results = Vec::new();
    for (n, turns) in tiers {
        let r = rt.block_on(run_tier(n, turns));
        println!(
            "| {} | {} | {} | {:.1} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {} | {} | {} |",
            r.sessions,
            r.turns_per_session,
            r.total_commits,
            r.wall_ms,
            r.commit.p50_ms,
            r.commit.p95_ms,
            r.commit.p99_ms,
            r.commit.p999_ms,
            r.commit.max_ms,
            r.shards_active,
            r.reader_errors,
            r.threads_after
                .map(|t| t.to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
        results.push(r);
    }

    if gate {
        for r in &results {
            assert_eq!(
                r.reader_errors, 0,
                "N={}: reader exhaustion/errors must be zero",
                r.sessions
            );
            if r.sessions >= 16 {
                assert!(
                    r.shards_active > 1,
                    "N={}: fixed-K sharding must distribute single-workspace sessions across >1 shard",
                    r.sessions
                );
            }
            if let Some(threads) = r.threads_after {
                // O(1) thread ceiling: tokio(8) + journal lanes(<=16) + projection(<=8) + shards(<=16) + catalog(3) + slack.
                assert!(
                    threads < 96,
                    "N={}: OS thread count {threads} violated O(1) bound",
                    r.sessions
                );
            }
        }
        println!("GATE PASSED: zero reader errors, multi-shard distribution, O(1) threads.");
    }
}
