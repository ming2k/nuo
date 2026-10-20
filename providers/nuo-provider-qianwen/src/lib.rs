//! QianwenAI Token Plan provider channel for Nuo (ADR-0015).

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

#[cfg(test)]
extern crate nuo_provider_deepseek;
#[cfg(test)]
extern crate nuo_provider_zai;

pub mod spec;
pub use spec::*;
