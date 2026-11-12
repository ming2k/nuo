//! Composer/input-box rendering tests: wrapping, caret positioning, grapheme/emoji
//! handling, chips, scroll-block, held/queued rows.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub(super) use super::*;

mod caret;
mod chips;
mod geometry;
mod helpers;
mod hints;
mod panel;
mod selection;
mod wrap;
