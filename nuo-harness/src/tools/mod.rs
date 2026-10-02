//! Built-in tools (filesystem, shell, web, ask-user, todo).
//!
//! Most tools self-register from their own module via
//! [`nuo_contracts::register_tool!`] (collected by `inventory` at link time).
//! The stateful todo tools are constructed by `nuo-harness` with their shared
//! task-list context. Shared helpers live in `helpers`, and pluggable
//! web-search backends in `search`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod reader;
pub mod search;
mod ssrf;

mod ask_user;
mod code_query;
mod edit_text;
pub(crate) mod execute_command;
mod file_search;
mod find_files;
mod helpers;
pub mod inspect;
mod list_dir;
mod read_image;
mod read_text;
pub mod recall_memory;
pub use recall_memory::RecallMemoryService;
mod search_text;
pub mod syntax_guard;
mod todo;
mod web;
mod write_file;

pub use syntax_guard::{SyntaxCheckResult, verify_syntax};

// Re-export every tool struct at the module root so existing consumers
// (`crate::tools::ReadTextTool`, etc.) keep resolving unchanged.
pub use ask_user::AskUserTool;
pub use code_query::CodeQueryTool;
pub use edit_text::EditTextTool;
pub use execute_command::ExecuteCommandTool;
pub use find_files::FindFilesTool;
pub use inspect::InspectTool;
pub use list_dir::ListDirTool;
pub use read_image::ReadImageTool;
pub use read_text::{ReadTextTerseTool, ReadTextTool};
pub use recall_memory::RecallMemoryTool;
pub use search_text::SearchTextTool;
pub use todo::{TodoTool, TodoToolContext};
pub(crate) use web::html_to_text;
pub use web::{WebPageSnapshot, WebReaderTool, WebSearchTool, WebSnapshotResult};
pub use write_file::WriteFileTool;

#[cfg(test)]
mod tests;
