//! AST and Code Intelligence utilities powered by Tree-sitter.
//!
//! Provides:
//! - Named-declaration extraction for structural queries
//!   ([`extract_declarations`], for the `code_query` tool)
//! - The closed structural-query grammar ([`parse_syntax_pattern`])

pub mod declarations;
pub mod query;

pub use declarations::{DECLARATION_KINDS, Declaration, extract_declarations};
pub use query::{SyntaxPattern, matches_name, parse_syntax_pattern};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
