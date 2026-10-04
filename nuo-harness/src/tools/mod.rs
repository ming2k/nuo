//! Built-in tools (filesystem, shell, web, ask-user, todo).
//!
//! Web tools have been decentralized into [`nuo_tools_web`],
//! and AST / Code tools into [`nuo_tools_code`] per ADR-0010.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod search {
    pub use nuo_tools_web::search::*;
}
pub mod reader {
    pub use nuo_tools_web::reader::*;
}
pub mod ssrf {
    pub use nuo_tools_web::ssrf::*;
}
pub mod web {
    pub use nuo_tools_web::*;
}
pub mod syntax_guard {
    pub use nuo_tools_code::syntax_guard::*;
}

mod ask_user;
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
mod todo;
mod write_file;

pub use nuo_tools_code::{CodeQueryTool, SyntaxCheckResult, verify_syntax};

// Re-export every tool struct at the module root so existing consumers
// (`crate::tools::ReadTextTool`, etc.) keep resolving unchanged.
pub use ask_user::AskUserTool;
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
pub use nuo_tools_web::{
    WebPageSnapshot, WebReaderTool, WebSearchTool, WebSnapshotResult, html_to_text,
};
pub use write_file::WriteFileTool;

#[cfg(test)]
mod tests;
