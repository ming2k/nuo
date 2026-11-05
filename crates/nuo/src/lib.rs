//! Unified application entrypoint and coordinator for Nuo.
//!
//! Re-exports server runtime modules from [`nuo_server`] per ADR-0011. The
//! interactive terminal shell is an internal module of this binary
//! ([`shell`], ADR-0103 §3, ADR-0106, ADR-0107) behind the default-on `interactive`
//! feature; the pure presentation substrates it renders through are the
//! external `nuo-ui-primitives` and `nuo-view` crates, which the shell names absolutely so the
//! dependency direction is visible at every use site.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub use nuo_server::*;
pub mod dev;
pub mod transport;

#[cfg(feature = "interactive")]
pub mod shell;
#[cfg(feature = "interactive")]
pub use shell::*;
