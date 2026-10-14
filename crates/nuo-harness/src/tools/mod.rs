//! Cognitive meta-tools owned by the Agent Harness ([INV-HARNESS-01]).
//!
//! Per ADR-0001 and ADR-0010, `nuo-harness` implements *strictly* cognitive
//! meta-tools ([`AskUserTool`], [`TodoTool`], [`InspectTool`]).
//!
//! Capability tools are decentralized in capability crates:
//! - Filesystem: [`nuo_tool_fs`]
//! - Process execution: [`nuo_tool_exec`]
//! - Web retrieval: [`nuo_tool_web`]
//! - Code & AST: [`nuo_tool_ast`]

mod ask_user;
pub mod inspect;
pub mod recall_memory;
mod todo;

pub use ask_user::AskUserTool;
pub use inspect::InspectTool;
pub use recall_memory::{RecallMemoryService, RecallMemoryTool};
pub use todo::{TodoTool, TodoToolContext};

pub use nuo_tool_fs::*;
pub use nuo_tool_exec::*;
pub use nuo_tool_web::*;
pub use nuo_tool_ast::*;
