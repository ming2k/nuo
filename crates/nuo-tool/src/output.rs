//! Structured tool output — the single [`ToolOutput`] type (ADR-0008 §7).
//!
//! The former flat `{content, is_error, metadata}` struct and the rich variant
//! enum were unified into one type living in [`crate::tool_output`]. This module
//! remains as the stable `nuo_tool::output::ToolOutput` path for call sites;
//! new code should prefer [`crate::tool_output`] or the crate-root re-export.

pub use crate::tool_output::ToolOutput;
