//! Root E2E entry (ADR-0178).
//!
//! Real process, real IPC, isolated `NUO_HOME`, hermetic mock provider. The
//! fresh binary path arrives from the nextest `e2e` profile setup script via
//! `NUO_TEST_BIN`; see `nuo_test_support::binary_path`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../common/mod.rs"]
mod common;

mod headless_roundtrip;
mod server_spawn;
mod smoke;
