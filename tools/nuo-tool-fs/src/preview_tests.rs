//! Preview contracts for the mutating fs tools (ADR-0125).
//!
//! Two properties matter, and only these two:
//!
//! 1. **A preview is what execution does.** The preview is computed by the same
//!    function the execution path uses, so an approved diff cannot differ from the
//!    bytes written. These tests assert that by running both and comparing.
//! 2. **A preview is pure.** A preview must not mutate the workspace, because the
//!    approval gate calls it *before* the user decides — a preview with side effects
//!    would perform the very change it was describing.
//!
//! Everything else (diff rendering, line counting) is tested where it lives.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use serde_json::json;
use std::sync::Arc;
use tempfile::tempdir;

fn context(dir: &tempfile::TempDir) -> Arc<SystemToolContext> {
    Arc::new(SystemToolContext::new(dir.path()))
}

/// The preview of an edit is exactly the content execution then produces.
#[tokio::test]
async fn edit_preview_matches_the_applied_edit() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    let ctx = context(&dir);
    let tool = EditTextTool::new(ctx);

    let args = json!({
        "path": "a.rs",
        "old_string": "fn b() {}",
        "new_string": "fn b() { todo!() }",
    });
    let preview = tool.preview(&args).expect("an edit has a preview");
    assert_eq!(preview.op, PatchOp::Edit);
    assert!(!preview.is_creation());
    assert_eq!(preview.path, "a.rs");
    // The preview carries the whole file, not the model's needle.
    assert_eq!(preview.old, "fn a() {}\nfn b() {}\n");
    assert_eq!(preview.new, "fn a() {}\nfn b() { todo!() }\n");
    assert_eq!(preview.line_counts(), (1, 1));

    // The file is untouched by the preview itself.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
        "fn a() {}\nfn b() {}\n",
        "a preview must not mutate the workspace"
    );

    // Executing writes precisely the previewed content.
    tool.execute(&ToolContext::default(), args).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
        preview.new,
        "the applied bytes must equal the previewed bytes"
    );
}

/// A creating write previews as a creation with an empty pre-image, and execution
/// produces that content.
#[tokio::test]
async fn write_preview_of_a_new_file_is_a_creation() {
    let dir = tempdir().unwrap();
    let tool = WriteFileTool::new(context(&dir));

    let args = json!({ "path": "new.rs", "content": "hello\n" });
    let preview = tool.preview(&args).expect("a write has a preview");
    assert!(preview.is_creation());
    assert_eq!(preview.old, "");
    assert_eq!(preview.new, "hello\n");
    assert_eq!(preview.line_counts(), (1, 0));
    assert!(!dir.path().join("new.rs").exists(), "pure preview");

    tool.execute(&ToolContext::default(), args).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("new.rs")).unwrap(),
        preview.new
    );
}

/// Overwriting an existing file previews the old bytes against the new ones, so the
/// operator sees exactly what would be lost.
#[tokio::test]
async fn write_preview_of_an_overwrite_shows_the_displaced_content() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "valuable work\n").unwrap();
    let tool = WriteFileTool::new(context(&dir));

    let preview = tool
        .preview(&json!({ "path": "a.rs", "content": "replacement\n" }))
        .unwrap();
    assert_eq!(preview.op, PatchOp::Edit);
    assert_eq!(preview.old, "valuable work\n");
    assert_eq!(preview.new, "replacement\n");
    assert_eq!(preview.line_counts(), (1, 1));
}

/// A preview that cannot be computed honestly returns `None` rather than guessing.
///
/// The three cases are: an edit whose anchor does not match (execution would reject
/// it), an edit whose anchor matches more than once (also rejected), and a path that
/// does not exist for an edit. In each case a preview would describe a call that will
/// not happen — making the preview the lie.
#[test]
fn edit_preview_is_absent_when_the_edit_would_be_rejected() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "alpha\nbeta\n").unwrap();
    let tool = EditTextTool::new(context(&dir));

    assert!(
        tool.preview(&json!({
            "path": "a.rs",
            "old_string": "not present",
            "new_string": "x",
        }))
        .is_none(),
        "a preview of an edit that cannot match must be absent"
    );

    assert!(
        tool.preview(&json!({
            "path": "a.rs",
            "old_string": "a",
            "new_string": "x",
        }))
        .is_none(),
        "a preview of an ambiguous edit must be absent"
    );

    assert!(
        tool.preview(&json!({
            "path": "missing.rs",
            "old_string": "a",
            "new_string": "x",
        }))
        .is_none(),
        "a preview of an edit to a missing file must be absent"
    );
}

/// A malformed argument blob gets no preview: the schema gate owns that rejection,
/// and a preview here would be a plausible-looking answer to a call that never runs.
#[test]
fn malformed_arguments_get_no_preview() {
    let dir = tempdir().unwrap();
    let write = WriteFileTool::new(context(&dir));
    assert!(write.preview(&json!({ "path": "a.rs" })).is_none());
    assert!(write.preview(&json!({})).is_none());

    let edit = EditTextTool::new(context(&dir));
    assert!(edit.preview(&json!({ "path": "a.rs" })).is_none());
}

/// An idempotent write previews as "no change", which the approval sheet reports
/// instead of drawing an empty diff.
#[test]
fn rewriting_identical_content_previews_as_unchanged() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "same\n").unwrap();
    let tool = WriteFileTool::new(context(&dir));

    let preview = tool
        .preview(&json!({ "path": "a.rs", "content": "same\n" }))
        .unwrap();
    assert!(preview.is_unchanged());
    assert_eq!(preview.line_counts(), (0, 0));
}

/// Tools whose effect cannot be computed without causing it must not pretend
/// otherwise. The default is `None`, and these tools do not override it.
#[test]
fn unpredictable_tools_have_no_preview() {
    let dir = tempdir().unwrap();
    let ctx = context(&dir);
    // A reader, a searcher, and a lister are not mutating tools at all.
    assert!(
        ReadTextTool::new(ctx.clone())
            .preview(&json!({ "path": "a.rs" }))
            .is_none()
    );
    assert!(
        SearchTextTool::new(ctx.clone())
            .preview(&json!({ "path": ".", "query": "x" }))
            .is_none()
    );
    assert!(
        FindFilesTool::new(ctx.clone())
            .preview(&json!({ "pattern": "*.rs" }))
            .is_none()
    );
    assert!(
        ListDirTool::new(ctx)
            .preview(&json!({ "path": "." }))
            .is_none()
    );
}
