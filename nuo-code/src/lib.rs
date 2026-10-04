//! Code intelligence and syntax verification domain powered by Tree-sitter for Nuo.

pub mod syntax;
pub mod syntax_guard;

pub use syntax::{
    DECLARATION_KINDS, Declaration, SupportedLanguage, SyntaxPattern, extract_declarations,
    matches_name, parse_syntax_pattern, verify_ast_syntax,
};
pub use syntax_guard::{SyntaxCheckResult, mutation_output, verify_syntax};
