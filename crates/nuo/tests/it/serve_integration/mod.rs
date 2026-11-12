#![allow(clippy::unwrap_used, clippy::expect_used)]

//! `serve` integration coverage, split by surface.
//!
//! Shared fixtures live in [`helpers`]; every other module owns one surface.

mod attach;
mod control_ipc;
mod helpers;
mod monitor;
mod reaper_suspend;
mod workspace_trust;
