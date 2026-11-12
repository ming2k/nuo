//! Snapshot baselines for tool-step and shell rendering.
//!
//! These lock the text/layout of the tool step so the rendering
//! refactor (RenderCtx extraction, and later redesign steps) can be verified
//! behavior-preserving. Snapshots capture the painted grid (cell symbols per
//! row, trailing whitespace trimmed) at a fixed terminal size.
//!
//! Regenerate baselines after an intentional visual change:
//!
//! ```sh
//! INSTA_UPDATE=always cargo test -p nuo --lib shell::snapshot_tests
//! ```
#![cfg(test)]

mod command_log;
mod context_bar;
pub(crate) mod helpers;
mod pre_attach;
mod tools;
mod transcript;
