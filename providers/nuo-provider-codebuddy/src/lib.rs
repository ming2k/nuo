//! Tencent AI CodeBuddy provider channel for Nuo.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod credential;
pub mod spec;

pub use credential::*;
pub use spec::*;
