//! Unified application entrypoint and coordinator for Nuo.
//!
//! Re-exports server runtime modules from [`nuo_server`] per ADR-0011.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub use nuo_server::*;
