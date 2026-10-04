#![allow(dead_code)]
//! Code intelligence, AST structural query, and syntax validation tools powered by Tree-sitter for Nuo.

pub mod code_query;
pub mod file_search;
pub mod helpers;
pub mod syntax;
pub mod syntax_guard;

pub use code_query::CodeQueryTool;
pub use syntax::{
    DECLARATION_KINDS, Declaration, SupportedLanguage, SyntaxPattern, extract_declarations,
    matches_name, parse_syntax_pattern, verify_ast_syntax,
};
pub use syntax_guard::{SyntaxCheckResult, mutation_output, verify_syntax};
