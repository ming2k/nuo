//! Exponential-backoff primitives.
//!
//! Leaf module (`[INV-HARNESS-04]`, ADR-0101): pure functions over numbers, so
//! the round policy (`orchestration`) and the subagent retry loop (`cognition`)
//! share one backoff definition without a layering cycle.

/// Stepped backoff delay for a retry attempt.
pub fn retry_delay_ms(
    attempt: usize,
    retry_after_ms: Option<u64>,
    base_ms: u64,
    max_ms: u64,
) -> u64 {
    // Stepped backoff schedule: 1s, 2s, 5s, 10s, 10s, then continuously 10s (scaled by base_ms).
    const STEP_MULTIPLIERS: [u64; 4] = [1, 2, 5, 10];
    let step_idx = attempt.saturating_sub(1);
    let multiplier = STEP_MULTIPLIERS[step_idx.min(STEP_MULTIPLIERS.len() - 1)];
    let stepped_backoff = base_ms.saturating_mul(multiplier);
    match retry_after_ms {
        Some(ms) => ms.max(base_ms).min(max_ms.max(1)),
        None => stepped_backoff.min(max_ms.max(1)),
    }
}

/// Apply "equal jitter" to a backoff delay, the variant recommended for
/// client-side retries: half the delay is fixed, the other half is randomised.
/// Unlike "full jitter" (`[0, base]`) it never collapses to a near-zero delay,
/// so a retry never fires immediately; unlike "no jitter" it still de-synchronises
/// clients that fail in unison (e.g. behind the same load balancer). The result
/// is always in `[base/2, base]`, so the configured upper bound is respected and
/// a server-supplied `Retry-After` (which bypasses `retry_delay_ms`) is honoured
/// while still being jittered to avoid thundering-herd on its expiry.
///
/// `roll` is an injected `[0, base] -> u64` closure so this stays a pure,
/// deterministic, unit-testable function; call sites supply `fastrand`. A `base`
/// of 0 is degenerate and passed through unchanged.
pub fn apply_jitter_ms(base: u64, roll: impl Fn(u64) -> u64) -> u64 {
    if base == 0 {
        return 0;
    }
    let half = base / 2;
    half + roll(base - half).min(base - half)
}

/// The one-line, user-facing form of a provider error for a retry notice.
pub fn public_retry_reason(message: &str) -> String {
    let first = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("transient provider error");
    const MAX_CHARS: usize = 96;
    if first.chars().count() <= MAX_CHARS {
        first.to_string()
    } else {
        let mut compact: String = first.chars().take(MAX_CHARS.saturating_sub(1)).collect();
        compact.push('…');
        compact
    }
}
