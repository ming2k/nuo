//! Semantic document model for the TUI.
//!
//! Unlike storing raw strings, this model preserves the structure of messages
//! so that selection and copy operate on semantic units (blocks) rather than
//! terminal grid characters.

pub mod blocks;
pub mod helpers;
pub mod inlines;
pub mod message;
pub mod types;

pub use blocks::Block;
pub use helpers::*;
pub use inlines::{CodeRange, Inline, InlineScan, LinkRange, MathRange};
pub use message::{CompactedCardData, TranscriptMessage};
pub use nuo_tool::{Role, SubagentEvent};

pub use types::*;

pub(crate) use super::markdown::{clamp_link_ranges, clamp_ranges, scan_inline};
pub use super::markdown::{parse_blocks, parse_blocks_plain, parse_blocks_verbatim};

#[cfg(test)]
#[path = "../document_tests.rs"]
mod tests;
