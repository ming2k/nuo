#![allow(dead_code)]
//! AST structural query tools for Nuo cognitive agents.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod code_query;
pub mod file_search;
pub mod helpers;

pub use code_query::CodeQueryTool;

/// Creates code AST query tools for an agent.
pub fn create_ast_tools(
    root: Option<std::path::PathBuf>,
) -> Vec<std::sync::Arc<dyn nuo_tool::Tool>> {
    vec![std::sync::Arc::new(CodeQueryTool::new(root))]
}
