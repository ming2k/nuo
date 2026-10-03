//! Relocated to the tool leaf (`nuo_tool::tool_output`) per ADR-0008 §7 — the
//! former flat `ToolOutput` and rich variant enum are unified into one type.
//! Re-exported here for existing call sites.

pub use nuo_tool::tool_output::*;
