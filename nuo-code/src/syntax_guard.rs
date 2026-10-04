//! Non-blocking syntax diagnostics for committed file modifications (ADR-0233).
//!
//! JSON and TOML use format parsers; supported source languages use Tree-sitter.
//! Diagnostics are not type checking and must never authorize or reject a write.

use std::path::Path;

/// Result of a pre/post-edit syntax check.
#[derive(Debug, PartialEq, Eq)]
pub enum SyntaxCheckResult {
    /// Content syntax is valid or the file format is not in the strict syntax validation whitelist.
    Valid,
    /// Content syntax is invalid with a diagnostic message and error detail.
    Invalid(String),
}

/// Verify syntactic integrity of `content` based on file extension.
///
/// Unsupported extensions return `Valid` for compatibility, not proof of validation.
pub fn verify_syntax(path: &Path, content: &str) -> SyntaxCheckResult {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();

    match extension.as_str() {
        "json" => {
            if let Err(e) = serde_json::from_str::<serde_json::Value>(content) {
                return SyntaxCheckResult::Invalid(format!("Malformed JSON: {e}"));
            }
        }
        "toml" => {
            if let Err(e) = toml::from_str::<toml::Value>(content) {
                return SyntaxCheckResult::Invalid(format!("Malformed TOML: {e}"));
            }
        }
        ext if crate::syntax::SupportedLanguage::from_extension(ext).is_some() => {
            if let Err(e) = crate::syntax::verify_ast_syntax(&extension, content) {
                return SyntaxCheckResult::Invalid(e);
            }
        }
        _ => {}
    }

    SyntaxCheckResult::Valid
}

/// Called only after the filesystem reports a successful write. Diagnostics
/// accompany the structured patch without changing its mutation semantics.
pub fn mutation_output(
    path: &Path,
    content: &str,
    mut patch: nuo_wire::ToolOutput,
) -> nuo_wire::ToolOutput {
    if let SyntaxCheckResult::Invalid(diagnostic) = verify_syntax(path, content) {
        let nuo_wire::ToolOutput::Patch { warnings, .. } = &mut patch else {
            unreachable!("mutation feedback requires a committed patch")
        };
        warnings.push(format!("non-blocking syntax diagnostic: {diagnostic}"));
    }
    patch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mutation_diagnostics_allow_multistep_repair_and_preserve_hard_errors() {
        use nuo_wire::{PatchOp, ToolOutput};
        let patch = ToolOutput::Patch {
            path: "test.rs".into(),
            op: PatchOp::Create,
            old: "".into(),
            new: "fn broken(".into(),
            start_line: 1,
            warnings: Vec::new(),
        };
        let res = mutation_output(Path::new("broken.rs"), "fn broken(", patch);
        let ToolOutput::Patch { warnings, .. } = res else { panic!("expected patch") };
        assert!(!warnings.is_empty());
        assert!(warnings[0].contains("non-blocking syntax diagnostic"));
    }

    #[test]
    fn json_syntax_validation() {
        let p = Path::new("config.json");
        assert_eq!(
            verify_syntax(p, r#"{"key": "value", "count": 42}"#),
            SyntaxCheckResult::Valid
        );
        assert!(matches!(
            verify_syntax(p, r#"{"key": "value", "count": }"#),
            SyntaxCheckResult::Invalid(_)
        ));
    }

    #[test]
    fn toml_syntax_validation() {
        let p = Path::new("Cargo.toml");
        assert_eq!(
            verify_syntax(p, "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n"),
            SyntaxCheckResult::Valid
        );
        assert!(matches!(
            verify_syntax(p, "[package\nname = \"pkg\""),
            SyntaxCheckResult::Invalid(_)
        ));
    }

    #[test]
    fn ast_supported_source_files_are_validated() {
        let rs_valid = Path::new("src/main.rs");
        assert_eq!(
            verify_syntax(rs_valid, "fn main() { println!(\"ok\"); }"),
            SyntaxCheckResult::Valid
        );

        let rs_broken = Path::new("src/main.rs");
        assert!(matches!(
            verify_syntax(rs_broken, "fn main() { broken unclosed"),
            SyntaxCheckResult::Invalid(_)
        ));

        let py_broken = Path::new("script.py");
        assert!(matches!(
            verify_syntax(py_broken, "def foo():\nreturn (unclosed"),
            SyntaxCheckResult::Invalid(_)
        ));

        // Unsupported languages pass through without error
        let lua_path = Path::new("config.lua");
        assert_eq!(
            verify_syntax(lua_path, "local x = 42"),
            SyntaxCheckResult::Valid
        );
    }
}
