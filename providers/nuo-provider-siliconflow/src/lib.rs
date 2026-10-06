//! SiliconFlow (硅基流动) balance / usage fetcher (ADR-0027 §3).
//!
//! Relocated verbatim from `nuo-provider-adapters/src/usage/siliconflow.rs`,
//! which had no counterpart anywhere in the `providers/` tree. Exposed as an
//! inherent `impl` so the composition root's typed quota dispatch can call it.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod usage;

pub use usage::*;
