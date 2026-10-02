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
pub(crate) fn mutation_output(
    path: &Path,
    content: &str,
    mut patch: nuo_contracts::ToolOutput,
) -> nuo_contracts::ToolOutput {
    if let SyntaxCheckResult::Invalid(diagnostic) = verify_syntax(path, content) {
        let nuo_contracts::ToolOutput::Patch { warnings, .. } = &mut patch else {
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
        use crate::tools::{edit_text::EditTextTool, write_file::WriteFileTool};
        use nuo_contracts::{Tool, ToolOutput};
        let dir = tempfile::tempdir().unwrap();
        let writer = WriteFileTool::new(Some(dir.path().to_path_buf()));
        let editor = EditTextTool::new(Some(dir.path().to_path_buf()));
        // Invalid new files and overwrites are both successful, with diagnostics.
        for (path, content) in [
            ("broken.rs", "fn broken("),
            ("config.toml", "[bad"),
            ("config.json", "{\"a\":,\"b\":}"),
        ] {
            let args = serde_json::json!({"path": path, "content": content}).to_string();
            for _ in 0..2 {
                let output = writer.call_structured(&args).await.unwrap();
                assert!(
                    matches!(&output, ToolOutput::Patch { warnings, new, .. } if !warnings.is_empty() && new == content)
                );
                let text = output.to_text();
                assert!(text.contains("Successfully wrote"));
                assert!(text.contains("Warning: non-blocking syntax diagnostic:"));
                assert_eq!(
                    std::fs::read_to_string(dir.path().join(path)).unwrap(),
                    content
                );
            }
        }
        // Already-broken -> still-broken -> valid -> invalid -> valid.
        for (old, new, warning) in [
            ("\"a\":,", "\"a\":1,", true),
            ("\"b\":}", "\"b\":2}", false),
            ("\"a\":1", "\"a\":", true),
            ("\"a\":,", "\"a\":3,", false),
        ] {
            let before = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
            let output = editor
                .call_structured(
                    &serde_json::json!({"path":"config.json", "old_string":old, "new_string":new})
                        .to_string(),
                )
                .await
                .unwrap();
            assert_eq!(
                output
                    .to_text()
                    .contains("Warning: non-blocking syntax diagnostic"),
                warning
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("config.json")).unwrap(),
                before.replacen(old, new, 1)
            );
            assert!(
                matches!(output, ToolOutput::Patch { warnings, .. } if warnings.is_empty() != warning)
            );
        }
        let before = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
        for old in ["missing", "\"", ""] {
            assert!(editor.call(&serde_json::json!({"path":"config.json", "old_string":old, "new_string":"invalid"}).to_string()).await.is_err());
            assert_eq!(
                std::fs::read_to_string(dir.path().join("config.json")).unwrap(),
                before
            );
        }
        std::fs::create_dir(dir.path().join("directory.json")).unwrap();
        let err = writer
            .call(&serde_json::json!({"path":"directory.json", "content":"{"}).to_string())
            .await
            .unwrap_err();
        assert!(err.contains("Failed to write"));
        assert!(!err.contains("succeeded"));
        // Legacy calls also carry warnings; unsupported formats claim no validation.
        assert!(
            writer
                .call(r#"{"path":"legacy.json","content":"{"}"#)
                .await
                .unwrap()
                .contains("Warning: non-blocking syntax diagnostic")
        );
        assert!(matches!(
            writer
                .call_structured(r#"{"path":"unknown.xyz","content":"{"}"#)
                .await
                .unwrap(),
            ToolOutput::Patch { .. }
        ));
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
