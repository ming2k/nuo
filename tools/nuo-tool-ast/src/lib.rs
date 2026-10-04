#![allow(dead_code)]
//! AST structural query tools for Nuo cognitive agents.

pub mod code_query;
pub mod file_search;
pub mod helpers;

pub use code_query::CodeQueryTool;

/// Creates code AST query tools for an agent.
pub fn create_ast_tools(root: Option<std::path::PathBuf>) -> Vec<std::sync::Arc<dyn nuo_tool::Tool>> {
    vec![std::sync::Arc::new(CodeQueryTool::new(root))]
}
