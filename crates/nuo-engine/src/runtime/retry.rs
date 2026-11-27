//! Exponential-backoff primitives.
//!
//! Leaf module (`[INV-HARNESS-04]`, ADR-0101): pure functions over numbers, so
//! the round policy (`orchestration`) and the subagent retry loop (`cognition`)
//! share one backoff definition without a layering cycle.

/// Maximum total attempts for consecutive local/routing network unreachability
/// (`ProviderErrorKind::Unreachable`) before fast-failing and parking a durable
/// `/retry` point (ADR-0224 `[INV-RESILIENCE-01]`).
///
/// Offline runs must not burn the 30-attempt transient upstream budget
/// (3.5–9 minutes of dead waits): 3 attempts absorb a momentary Wi-Fi/VPN
/// handoff (~3 s on immediate `ENETUNREACH`, ~23 s on a blackhole) and then
/// return control to the operator with a parked `RetryPoint`.
pub const UNREACHABLE_MAX_ATTEMPTS: usize = 3;

/// Whether a retryable failure warrants flushing the shared provider connection
/// pools before the next attempt dials (ADR-0224 `[INV-RESILIENCE-02]`).
///
/// Transport, reachability, and timeout failures may leave half-open sockets
/// behind after a Wi-Fi or VPN switch; HTTP 429/5xx arrived cleanly over a
/// healthy socket, so flushing the pool there would only waste TLS handshakes.
pub const fn should_flush_pools(kind: nuo_protocol::ProviderErrorKind) -> bool {
    matches!(
        kind,
        nuo_protocol::ProviderErrorKind::Transport
            | nuo_protocol::ProviderErrorKind::Unreachable
            | nuo_protocol::ProviderErrorKind::Timeout
    )
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_protocol::ProviderErrorKind;

    #[test]
    fn unreachable_budget_and_pool_flush_policy() {
        assert_eq!(UNREACHABLE_MAX_ATTEMPTS, 3);
        assert!(should_flush_pools(ProviderErrorKind::Transport));
        assert!(should_flush_pools(ProviderErrorKind::Unreachable));
        assert!(should_flush_pools(ProviderErrorKind::Timeout));
        // HTTP 429 / 5xx arrived cleanly over a healthy socket; keep the pool.
        assert!(!should_flush_pools(ProviderErrorKind::RateLimited));
        assert!(!should_flush_pools(ProviderErrorKind::Upstream));
        assert!(!should_flush_pools(ProviderErrorKind::Authentication));
    }

    #[test]
    fn stepped_backoff_advances_on_1_based_attempts() {
        assert_eq!(retry_delay_ms(1, None, 1_000, 10_000), 1_000);
        assert_eq!(retry_delay_ms(2, None, 1_000, 10_000), 2_000);
        assert_eq!(retry_delay_ms(3, None, 1_000, 10_000), 5_000);
        assert_eq!(retry_delay_ms(4, None, 1_000, 10_000), 10_000);
        assert_eq!(retry_delay_ms(5, None, 1_000, 10_000), 10_000);
    }
}
