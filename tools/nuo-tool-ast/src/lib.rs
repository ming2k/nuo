//! AST structural query tools for Nuo cognitive agents.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod code_query;
pub mod file_search;
pub mod helpers;
pub mod syntax;

pub use code_query::CodeQueryTool;
pub use syntax::{
    DECLARATION_KINDS, Declaration, SupportedLanguage, SyntaxPattern, extract_declarations,
    matches_name, parse_syntax_pattern,
};
