//! Wiring for the dynamic-catalog pattern — the background refresh loop that
//! drives every [`DynamicCatalog`] implementation.
//!
//! Each catalog (remote skills, MCP tools, …) implements
//! [`DynamicCatalog`]; this module provides the single [`spawn_refresh`] that
//! runs one on a schedule. The wiring layer (the CLI binary) calls it once per
//! catalog at startup, after the initial eager refresh.

use std::time::Duration;

use nuo_contracts::DynamicCatalog;
/// Spawn a background task that refreshes a [`DynamicCatalog`] on its declared
/// cadence. The first tick fires **immediately** (so a catalog that was not
/// refreshed eagerly at startup gets its first refresh within seconds);
/// subsequent ticks drive periodic refresh. Errors are logged and swallowed —
/// a failed refresh never kills the loop.
///
/// The task lives for the program's lifetime.
pub fn spawn_refresh(catalog: impl DynamicCatalog + 'static) {
    let id = catalog.id();
    let period = catalog.refresh_period();
    if period == Duration::ZERO {
        tracing::debug!(catalog = id, "periodic refresh disabled (period is zero)");
        return;
    }
    tokio::spawn(async move {
        // Fire an immediate first refresh so the catalog is populated without
        // blocking the startup path, then settle into the periodic cadence.
        let mut consecutive_failures = 0u32;
        if let Err(error) = catalog.refresh().await {
            consecutive_failures = 1;
            tracing::warn!(catalog = id, %error, "initial background refresh failed");
        }
        loop {
            // Apply exponential backoff when consecutive failures occur, capping at 4x period or 60s.
            let next_delay = if consecutive_failures > 0 {
                let multiplier = 1u32.checked_shl(consecutive_failures.min(3)).unwrap_or(8);
                period
                    .saturating_mul(multiplier)
                    .min(Duration::from_secs(60))
            } else {
                period
            };
            tokio::time::sleep(next_delay).await;

            match catalog.refresh().await {
                Ok(()) => {
                    if consecutive_failures > 0 {
                        tracing::info!(catalog = id, "periodic catalog refresh recovered");
                        consecutive_failures = 0;
                    }
                }
                Err(error) => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    tracing::warn!(
                        catalog = id,
                        %error,
                        consecutive_failures,
                        "periodic refresh failed, backing off"
                    );
                }
            }
        }
    });
}
