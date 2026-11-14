//! Cognitive meta-tools owned by the Agent Harness ([INV-HARNESS-01]).
//!
//! Per ADR-0001 and ADR-0010 (superseded by ADR-0100), `nuo-harness` implements *strictly* cognitive
//! meta-tools ([`AskUserTool`], [`TodoTool`]).
//!
//! Capability tools are decentralized in capability crates:
//! - Filesystem: `nuo-tool-fs`
//! - Process execution: `nuo-tool-exec`
//! - Web retrieval: `nuo-tool-web`
//! - Code & AST: `nuo-tool-ast`

mod ask_user;
pub mod recall_memory;
mod todo;

pub use ask_user::AskUserTool;
pub use recall_memory::{RecallMemoryService, RecallMemoryTool};
pub use todo::{TodoTool, TodoToolContext};

#[cfg(test)]
pub use nuo_tool_ast::*;
#[cfg(test)]
pub use nuo_tool_exec::*;
#[cfg(test)]
pub use nuo_tool_fs::*;
#[cfg(test)]
pub use nuo_tool_web::*;
