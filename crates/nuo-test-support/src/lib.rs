//! Hermetic test harness for Nuo's integration and E2E tiers.
//!
//! This crate is deliberately **not** part of the shipped graph: it is a
//! dev-dependency only, and `nuo/tests/test_architecture.rs` enforces that no
//! crate declares it in `[dependencies]` or `[build-dependencies]`.
//!
//! The harness has three responsibilities:
//!
//! - [`Sandbox`] gives every test a private `NUO_HOME` (config, state, data,
//!   cache, instance) and workspace, and can write the minimal provider files
//!   an E2E run needs.
//! - [`MockProvider`] is a deterministic OpenAI-compatible HTTP/SSE surface.
//!   E2E tests never reach a live provider.
//! - [`Cli`] runs the freshly built `nuo` binary with a bounded timeout and
//!   captures stdout/stderr without pipe deadlocks.
//!
//! Tests that mutate process-global environment variables must not use this
//! crate; E2E tests pass the sandbox environment to child processes instead,
//! so they are safe to run concurrently under nextest.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod cli;
pub mod mock;
pub mod sandbox;

pub use cli::{ChildGuard, Cli};
pub use mock::{MockProvider, RecordedRequest};
pub use sandbox::{DiscoveryRecord, Sandbox, ServerCleanup};

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The freshly built `nuo` binary selected by the nextest `e2e` profile.
///
/// Root-level E2E tests live in the `nuo-tests` package, so Cargo does not set
/// `CARGO_BIN_EXE_nuo` for them. The nextest setup script builds the binary and
/// exports its exact path as `NUO_TEST_BIN`; package-local tests may still set
/// `CARGO_BIN_EXE_nuo` directly.
pub fn binary_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("NUO_TEST_BIN") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = option_env!("CARGO_BIN_EXE_nuo") {
        return Ok(PathBuf::from(path));
    }
    Err("the `nuo` binary path is unavailable; run E2E through \
         `cargo nextest run -p nuo-tests --profile e2e` (its setup script exports \
         NUO_TEST_BIN) or set NUO_TEST_BIN explicitly"
        .into())
}

/// Boxed error used by test helpers. Tests can return `nuo_test_support::Result<()>`
/// from a `#[test]` function, so setup failures report the underlying cause.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Poll `probe` until it yields `Some`, or fail after `timeout`.
///
/// Used for process-level facts (discovery file written, server stopped) that
/// have no in-process notification edge.
pub fn eventually<T>(timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> Result<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = probe() {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(format!("condition was not met within {timeout:?}").into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
