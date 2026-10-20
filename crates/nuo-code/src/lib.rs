//! Code intelligence and syntax verification domain powered by Tree-sitter for Nuo.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod syntax;
pub mod syntax_guard;

pub use syntax::{
    DECLARATION_KINDS, Declaration, SupportedLanguage, SyntaxPattern, extract_declarations,
    matches_name, parse_syntax_pattern, verify_ast_syntax,
};
pub use syntax_guard::{SyntaxCheckResult, mutation_output, verify_syntax};
