//! Wall-clock time primitive.
//!
//! A leaf module (`[INV-HARNESS-04]`, ADR-0101): it depends on nothing in
//! `cognition`/`orchestration`/`dispatch`, so both the cognitive loop and the
//! round policy can share it without creating a layering cycle.

/// Wall-clock now in Unix-epoch milliseconds. The timestamp source for
/// round-interrupt records (C11): it must be a payload field because event-log
/// compaction drops envelope timestamps.
pub(crate) fn unix_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
