//! Unified application entrypoint and coordinator for Nuo.
//!
//! Re-exports server runtime modules from [`nuo_server`] per ADR-0011.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub use nuo_server::*;
