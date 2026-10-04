//! AST and Code Intelligence utilities powered by Tree-sitter (ADR-0211).
//!
//! Provides:
//! - Pre-mutation syntax verification (`verify_ast_syntax`)
//! - Structural symbol extraction for on-demand outlines (`extract_symbols`)
//! - Named-declaration extraction for structural queries
//!   ([`extract_declarations`], for the `code_query` tool)
//! - The closed structural-query grammar ([`parse_syntax_pattern`])
//!
//! Per ADR-0214 there is no ambient repository-wide Repo Map generator here:
//! structure enters the model context only through the scoped, bounded
//! `code_query` tool. This module owns parsing; it does not own freshness or
//! context delivery, and an on-demand parse of the current bytes is the
//! correctness baseline.

pub mod declarations;
pub mod query;

pub use declarations::{DECLARATION_KINDS, Declaration, extract_declarations};
pub use query::{SyntaxPattern, matches_name, parse_syntax_pattern};

use tree_sitter::{Node, Parser};

/// Supported language for Tree-sitter parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportedLanguage {
    Rust,
    TypeScript,
    Tsx,
    Python,
    C,
    Cpp,
    Go,
}

impl SupportedLanguage {
    /// Detect supported language from a file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "rs" => Some(Self::Rust),
            "ts" | "js" | "mjs" | "cjs" => Some(Self::TypeScript),
            "tsx" | "jsx" => Some(Self::Tsx),
            "py" => Some(Self::Python),
            "c" | "h" => Some(Self::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some(Self::Cpp),
            "go" => Some(Self::Go),
            _ => None,
        }
    }

    /// Resolve the underlying Tree-sitter `Language`.
    pub fn tree_sitter_language(self) -> tree_sitter::Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }
}

/// Verify syntactic correctness of `content` using Tree-sitter.
///
/// Returns `Ok(())` if the syntax is valid or if the language is unsupported.
/// Returns `Err(message)` with line and column diagnostics if a syntax error is detected.
pub fn verify_ast_syntax(ext: &str, content: &str) -> Result<(), String> {
    let Some(lang) = SupportedLanguage::from_extension(ext) else {
        return Ok(());
    };

    let mut parser = Parser::new();
    if let Err(err) = parser.set_language(&lang.tree_sitter_language()) {
        return Err(format!("Tree-sitter failed to set language: {err}"));
    }

    let Some(tree) = parser.parse(content, None) else {
        return Err("Tree-sitter parser returned empty tree".to_string());
    };

    let root = tree.root_node();
    if root.has_error() {
        if let Some(err_node) = find_first_error(root) {
            let start = err_node.start_position();
            let row = start.row + 1;
            let col = start.column + 1;
            let snippet = node_snippet(err_node, content);
            return Err(format!(
                "Syntax error at line {row}, column {col} near '{snippet}': broken or unclosed syntax"
            ));
        }
        return Err("Syntax error detected: tree contains invalid syntax nodes".to_string());
    }

    Ok(())
}

/// Recursively find the first node with an error flag.
fn find_first_error(node: Node) -> Option<Node> {
    if node.is_error() || node.is_missing() {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.has_error() {
            return find_first_error(child);
        }
    }
    None
}

/// Short preview of the error node's text.
fn node_snippet<'a>(node: Node, source: &'a str) -> &'a str {
    let range = node.byte_range();
    if range.start < source.len() {
        let end = range.end.min(source.len());
        let slice = &source[range.start..end];
        let first_line = slice.lines().next().unwrap_or("").trim();
        if first_line.len() > 30 {
            &first_line[..30]
        } else {
            first_line
        }
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_rust_syntax_passes() {
        let code = r#"
            pub fn add(a: i32, b: i32) -> i32 {
                a + b
            }
        "#;
        assert_eq!(verify_ast_syntax("rs", code), Ok(()));
    }

    #[test]
    fn broken_rust_syntax_is_rejected() {
        let code = "fn broken( { let x = 1; ";
        assert!(verify_ast_syntax("rs", code).is_err());
    }

    #[test]
    fn valid_python_syntax_passes() {
        let code = "def hello():\n    return 42\n";
        assert_eq!(verify_ast_syntax("py", code), Ok(()));
    }

    #[test]
    fn broken_python_syntax_is_rejected() {
        let code = "def hello(\nreturn 42\n";
        assert!(verify_ast_syntax("py", code).is_err());
    }

    #[test]
    fn declaration_extraction_covers_the_rust_item_kinds() {
        let code = r#"pub struct User { id: u64 }
pub trait Greeter {
    fn greet(&self);
}
pub fn run() {}
"#;
        let declarations = extract_declarations("rs", code);
        let rendered: Vec<&str> = declarations
            .iter()
            .map(|decl| decl.signature.as_str())
            .collect();
        assert!(
            rendered.contains(&"pub struct User { id: u64 }"),
            "{rendered:?}"
        );
        assert!(rendered.contains(&"pub trait Greeter"), "{rendered:?}");
        assert!(rendered.contains(&"pub fn run()"), "{rendered:?}");
        // Members are part of the same index, not a second code path.
        assert!(
            declarations
                .iter()
                .any(|decl| decl.kind == "method" && decl.qualified_name() == "Greeter::greet"),
            "{declarations:?}"
        );
    }

    #[test]
    fn valid_c_cpp_syntax_passes() {
        let c_code = "int main(void) { return 0; }";
        assert_eq!(verify_ast_syntax("c", c_code), Ok(()));

        let cpp_code = "class Engine { public: void start(); };";
        assert_eq!(verify_ast_syntax("cpp", cpp_code), Ok(()));
    }

    #[test]
    fn broken_c_cpp_syntax_is_rejected() {
        let broken_c = "int main(void { return 0;";
        assert!(verify_ast_syntax("c", broken_c).is_err());
    }

    #[test]
    fn valid_go_syntax_passes() {
        let go_code = "package main\nfunc main() {}\n";
        assert_eq!(verify_ast_syntax("go", go_code), Ok(()));
    }

    #[test]
    fn broken_go_syntax_is_rejected() {
        let broken_go = "package main\nfunc main( {}\n";
        assert!(verify_ast_syntax("go", broken_go).is_err());
    }
}
