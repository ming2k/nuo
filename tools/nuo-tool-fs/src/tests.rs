use super::*;
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn test_fs_tools_lifecycle() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();

    let write_tool = WriteFileTool::new(ctx.clone());
    let read_tool = ReadTool::new(ctx.clone());
    let edit_tool = EditTextTool::new(ctx.clone());
    let find_tool = FindFilesTool::new(ctx.clone());
    let search_tool = SearchTextTool::new(ctx.clone());

    // 1. Write file
    let write_res = write_tool
        .execute(
            &t_ctx,
            json!({
                "path": "hello.txt",
                "content": "line 1: hello\nline 2: world\nline 3: end"
            }),
        )
        .await
        .unwrap();
    assert!(!write_res.is_error());
    assert!(matches!(
        &write_res,
        ToolOutput::Patch {
            op: PatchOp::Create,
            start_line: 1,
            ..
        }
    ));

    // 2. Read file
    let read_res = read_tool
        .execute(
            &t_ctx,
            json!({"path": "hello.txt", "offset": 2, "limit": 1}),
        )
        .await
        .unwrap();
    assert!(read_res.content().contains("line 2: world"));

    // 3. Edit file
    let edit_res = edit_tool
        .execute(
            &t_ctx,
            json!({
                "path": "hello.txt",
                "old_string": "line 2: world",
                "new_string": "line 2: nuo"
            }),
        )
        .await
        .unwrap();
    assert!(!edit_res.is_error());
    assert!(matches!(
        &edit_res,
        ToolOutput::Patch {
            op: PatchOp::Edit,
            start_line: 2,
            ..
        }
    ));

    // 4. Search text
    let search_res = search_tool
        .execute(&t_ctx, json!({"path": ".", "query": "nuo"}))
        .await
        .unwrap();
    assert!(search_res.content().contains("hello.txt:2: line 2: nuo"));

    // 5. Find files
    let find_res = find_tool
        .execute(&t_ctx, json!({"path": "."}))
        .await
        .unwrap();
    assert!(find_res.content().contains("hello.txt"));
}

#[tokio::test]
async fn read_emits_structured_code_with_pagination_framing() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();
    fs::write(dir.path().join("src.rs"), "a\nb\nc\nd\n").unwrap();

    let out = ReadTool::new(ctx)
        .execute(&t_ctx, json!({"path": "src.rs", "offset": 2, "limit": 2}))
        .await
        .unwrap();

    match &out {
        ToolOutput::Code {
            lang,
            text,
            start_line,
            prefix,
            suffix,
        } => {
            // Pure content: no baked gutter and no range header.
            assert_eq!(text, "b\nc");
            assert_eq!(*start_line, 2);
            assert_eq!(lang.as_deref(), Some("rs"));
            assert_eq!(prefix.as_deref(), Some("[Lines 2-3 of 4 from `src.rs`]"));
            assert_eq!(
                suffix.as_deref(),
                Some("[1 more line — read with offset=4]")
            );
        }
        other => panic!("expected ToolOutput::Code, got {other:?}"),
    }

    // The model still sees the range header and per-line file numbers,
    // composed at `to_text` time instead of baked into `text`.
    assert_eq!(
        out.to_text(),
        "[Lines 2-3 of 4 from `src.rs`]\n2: b\n3: c\n[1 more line — read with offset=4]"
    );
}

#[tokio::test]
async fn read_to_eof_has_no_continuation_hint() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();
    fs::write(dir.path().join("full.txt"), "one\ntwo\n").unwrap();

    let out = ReadTool::new(ctx)
        .execute(&t_ctx, json!({"path": "full.txt"}))
        .await
        .unwrap();

    match out {
        ToolOutput::Code {
            text,
            start_line,
            prefix,
            suffix,
            ..
        } => {
            assert_eq!(text, "one\ntwo");
            assert_eq!(start_line, 1);
            assert_eq!(prefix.as_deref(), Some("[Lines 1-2 of 2 from `full.txt`]"));
            assert_eq!(suffix, None);
        }
        other => panic!("expected ToolOutput::Code, got {other:?}"),
    }
}

#[tokio::test]
async fn read_resolves_virtual_path_via_vfs_without_disk() {
    struct MockVfs;
    #[async_trait]
    impl nuo_tool::VirtualFileResolver for MockVfs {
        async fn resolve_virtual(&self, path: &str) -> Option<String> {
            if path == "/tmp/nuo/vfs/call_123.txt"
                || path == ".nuo/virtual/observations/call_123.txt"
            {
                Some("alpha\nbeta\ngamma\ndelta\n".to_string())
            } else {
                None
            }
        }
    }

    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()).with_vfs_resolver(Arc::new(MockVfs)));
    let t_ctx = ToolContext::default();

    let out = ReadTool::new(ctx)
        .execute(
            &t_ctx,
            json!({
                "path": "/tmp/nuo/vfs/call_123.txt",
                "offset": 2,
                "limit": 2
            }),
        )
        .await
        .unwrap();

    match out {
        ToolOutput::Code {
            text,
            start_line,
            prefix,
            suffix,
            ..
        } => {
            assert_eq!(text, "beta\ngamma");
            assert_eq!(start_line, 2);
            assert_eq!(
                prefix.as_deref(),
                Some("[Lines 2-3 of 4 from `/tmp/nuo/vfs/call_123.txt`]")
            );
            assert_eq!(
                suffix.as_deref(),
                Some("[1 more line — read with offset=4]")
            );
        }
        other => panic!("expected ToolOutput::Code, got {other:?}"),
    }
}

#[tokio::test]
async fn test_read_resolves_spool_disk_fallback() {
    let spool_dir = nuo_tool::spool_root_dir();
    let _ = std::fs::create_dir_all(&spool_dir);
    let spool_file = spool_dir.join("test_spool_fallback.log");
    std::fs::write(&spool_file, b"line1\nline2\nline3\nline4\n").unwrap();

    let dir = tempdir().unwrap();
    // Context WITHOUT vfs_resolver
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();

    let spool_path_str = spool_file.to_str().unwrap().to_string();
    let out = ReadTool::new(ctx)
        .execute(
            &t_ctx,
            json!({
                "path": spool_path_str,
                "offset": 2,
                "limit": 2
            }),
        )
        .await
        .unwrap();

    let _ = std::fs::remove_file(&spool_file);

    match out {
        ToolOutput::Code {
            text, start_line, ..
        } => {
            assert_eq!(text, "line2\nline3");
            assert_eq!(start_line, 2);
        }
        other => panic!("expected ToolOutput::Code, got {other:?}"),
    }
}

#[test]
fn display_match_path_falls_back_when_root_is_the_file() {
    let ws = Path::new("/ws");
    let file = Path::new("/ws/src/actions.rs");
    // Directory root: relative to the search root.
    assert_eq!(
        display_match_path(file, Path::new("/ws/src"), ws),
        "actions.rs"
    );
    // File root (single-file search): stripping would be empty, so fall
    // back to the workspace-relative path rather than the empty string.
    assert_eq!(display_match_path(file, file, ws), "src/actions.rs");
    // No usable base at all -> bare filename, never empty.
    assert_eq!(
        display_match_path(file, Path::new("/x"), Path::new("/y")),
        "actions.rs"
    );
}

#[tokio::test]
async fn search_text_single_file_search_emits_a_real_path() {
    // Regression: searching a *file* (not a directory) used to strip the
    // file's own path as a prefix, yielding an empty path and emitting a
    // pathless `:LINE: content` line — unreadable for both the model and
    // the TUI (which then tallied 0 files and dropped the file title row).
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    let file = dir.path().join("src").join("actions.rs");
    fs::write(&file, "fn a() {}\npub(super) fn enter_scene(\nfn b() {}\n").unwrap();

    let out = SearchTextTool::new(ctx)
        .execute(
            &t_ctx,
            json!({"path": "src/actions.rs", "query": "enter_scene"}),
        )
        .await
        .unwrap();

    let text = out.content();
    assert!(
        text.contains("src/actions.rs:2: pub(super) fn enter_scene("),
        "the match line must carry a real, workspace-relative path; got:\n{text}"
    );
    assert!(
        !text.contains("\n:2:"),
        "no pathless match line may be emitted; got:\n{text}"
    );
}

#[tokio::test]
async fn test_edit_normalizes_trailing_whitespace() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();

    let file_path = dir.path().join("code.rs");
    fs::write(&file_path, "fn hello() {\n    let a = 1;\n}\n").unwrap();

    let edit_tool = EditTextTool::new(ctx.clone());
    let out = edit_tool
        .execute(
            &t_ctx,
            json!({
                "path": "code.rs",
                "old_string": "    let a = 1;  ",
                "new_string": "    let a = 2;"
            }),
        )
        .await
        .unwrap();
    match out {
        ToolOutput::Patch { warnings, .. } => {
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains("normalizing")),
                "a normalized match must be disclosed: {warnings:?}"
            );
        }
        other => panic!("expected Patch, got {other:?}"),
    }
    assert_eq!(
        fs::read_to_string(&file_path).unwrap(),
        "fn hello() {\n    let a = 2;\n}\n"
    );
}

#[tokio::test]
async fn find_files_ignores_git_dir_and_gitignore_and_ignored_dirs() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();

    // 1. .git internal files must be pruned
    let git_hooks = dir.path().join(".git").join("hooks");
    fs::create_dir_all(&git_hooks).unwrap();
    fs::write(git_hooks.join("applypatch-msg.samp"), "sample hook").unwrap();

    // 2. .gitignore entries must be pruned
    fs::write(
        dir.path().join(".gitignore"),
        "secret.txt\nbuild_out/\n*.log\n",
    )
    .unwrap();
    fs::write(dir.path().join("secret.txt"), "api_key").unwrap();
    let build_out = dir.path().join("build_out");
    fs::create_dir_all(&build_out).unwrap();
    fs::write(build_out.join("artifact.bin"), "bin").unwrap();
    fs::write(dir.path().join("debug.log"), "debug").unwrap();

    // 3. Known ignored directories like node_modules, target, etc. must be pruned
    let node_modules = dir.path().join("node_modules").join("pkg");
    fs::create_dir_all(&node_modules).unwrap();
    fs::write(node_modules.join("index.js"), "module.exports = {}").unwrap();

    let target_dir = dir.path().join("target").join("debug");
    fs::create_dir_all(&target_dir).unwrap();
    fs::write(target_dir.join("build.rs"), "fn main() {}").unwrap();

    // 4. Legitimate project files
    let src_dir = dir.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("main.rs"), "fn main() {}").unwrap();

    let tool = FindFilesTool::new(ctx);
    let res = tool.execute(&t_ctx, json!({"path": "."})).await.unwrap();

    let content = res.content();
    assert!(
        content.contains("src/main.rs"),
        "legitimate project file must be found; got:\n{content}"
    );
    assert!(
        !content
            .lines()
            .any(|l| l == ".git" || l.starts_with(".git/")),
        ".git directories/files must never be found; got:\n{content}"
    );
    assert!(
        !content.contains("applypatch-msg.samp"),
        ".git/hooks must not be found; got:\n{content}"
    );
    assert!(
        !content.contains("secret.txt"),
        ".gitignore files must be pruned; got:\n{content}"
    );
    assert!(
        !content.contains("artifact.bin"),
        ".gitignore directories must be pruned; got:\n{content}"
    );
    assert!(
        !content.contains("debug.log"),
        ".gitignore glob matches must be pruned; got:\n{content}"
    );
    assert!(
        !content.contains("node_modules"),
        "node_modules must be pruned; got:\n{content}"
    );
    assert!(
        !content.contains("target"),
        "target directory must be pruned; got:\n{content}"
    );
}

#[tokio::test]
async fn find_files_accepts_pattern_variations_and_slashless_globs() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();

    let nested = dir.path().join("crates").join("core").join("src");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("lib.rs"), "pub fn run() {}").unwrap();
    fs::write(nested.join("readme.md"), "# Core").unwrap();

    let tool = FindFilesTool::new(ctx);

    // A. Singular "pattern" alias with slashless glob matching nested files
    let res1 = tool
        .execute(&t_ctx, json!({"pattern": "*.rs"}))
        .await
        .unwrap();
    assert!(
        res1.content().contains("crates/core/src/lib.rs"),
        "slashless pattern must match nested file; got:\n{}",
        res1.content()
    );
    assert!(!res1.content().contains("readme.md"));

    // B. "patterns" as array
    let res2 = tool
        .execute(&t_ctx, json!({"patterns": ["*.md"]}))
        .await
        .unwrap();
    assert!(res2.content().contains("crates/core/src/readme.md"));
    assert!(!res2.content().contains("lib.rs"));

    // C. "patterns" as single string (JSON normalization)
    let res3 = tool
        .execute(&t_ctx, json!({"patterns": "*.rs"}))
        .await
        .unwrap();
    assert!(res3.content().contains("crates/core/src/lib.rs"));

    // D. Omitted patterns -> all files
    let res4 = tool
        .execute(&t_ctx, json!({"path": "crates"}))
        .await
        .unwrap();
    assert!(res4.content().contains("core/src/lib.rs"));
    assert!(res4.content().contains("core/src/readme.md"));
}

#[tokio::test]
async fn search_text_ignores_git_dir_and_gitignore() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();

    let git_dir = dir.path().join(".git");
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(git_dir.join("config"), "target_marker_needle = 1").unwrap();

    fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();
    fs::write(dir.path().join("ignored.rs"), "target_marker_needle = 2").unwrap();

    let src_dir = dir.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("main.rs"), "target_marker_needle = 3").unwrap();

    let tool = SearchTextTool::new(ctx);
    let res = tool
        .execute(
            &t_ctx,
            json!({"path": ".", "query": "target_marker_needle"}),
        )
        .await
        .unwrap();

    let content = res.content();
    assert!(
        content.contains("src/main.rs:1: target_marker_needle = 3"),
        "legitimate file must be matched; got:\n{content}"
    );
    assert!(
        !content.contains(".git"),
        ".git must not be searched; got:\n{content}"
    );
    assert!(
        !content.contains("ignored.rs"),
        ".gitignore file must not be searched; got:\n{content}"
    );
}
