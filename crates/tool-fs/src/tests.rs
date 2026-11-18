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
            ..
        } => {
            // Pure content: no baked gutter and no range header.
            assert_eq!(text, "b\nc");
            assert_eq!(*start_line, 2);
            assert_eq!(lang.as_deref(), Some("rs"));
            assert!(
                prefix.as_deref().is_some_and(|prefix| prefix
                    .starts_with("[Lines 2-3 of 4 from `src.rs`]")
                    && prefix.contains("version")),
                "{prefix:?}"
            );
            assert_eq!(
                suffix.as_deref(),
                Some("[1 more line — read with offset=4]")
            );
        }
        other => panic!("expected ToolOutput::Code, got {other:?}"),
    }

    // The model still sees the range header and per-line file numbers,
    // composed at `to_text` time instead of baked into `text`.
    let text = out.to_text();
    assert!(text.starts_with("[Lines 2-3 of 4 from `src.rs`]"), "{text}");
    assert!(text.contains("version"), "{text}");
    assert!(
        text.ends_with("[1 more line — read with offset=4]"),
        "{text}"
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
            assert!(
                prefix.as_deref().is_some_and(|prefix| prefix
                    .starts_with("[Lines 1-2 of 2 from `full.txt`]")
                    && prefix.contains("version")),
                "{prefix:?}"
            );
            assert_eq!(suffix, None);
        }
        other => panic!("expected ToolOutput::Code, got {other:?}"),
    }
}

#[tokio::test]
async fn read_resolves_virtual_path_via_vfs_without_disk() {
    struct MockVfs;
    #[async_trait]
    impl tool::VirtualFileResolver for MockVfs {
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
    let spool_dir = tool::spool_root_dir();
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
async fn find_files_supports_max_depth_1_as_directory_listing() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();

    fs::create_dir_all(dir.path().join("src").join("nested")).unwrap();
    fs::write(dir.path().join("src").join("nested").join("deep.rs"), "").unwrap();
    fs::write(dir.path().join("Cargo.toml"), "").unwrap();
    fs::write(dir.path().join(".env"), "SECRET=1").unwrap();

    let out = FindFilesTool::new(ctx)
        .execute(&t_ctx, json!({"path": ".", "max_depth": 1}))
        .await
        .unwrap();

    match out {
        ToolOutput::Listing {
            entries,
            total,
            truncated,
            ..
        } => {
            assert_eq!(total, Some(3));
            assert!(!truncated);
            // Directories first, then files case-insensitively; nested files excluded.
            assert_eq!(entries, vec!["src/", ".env", "Cargo.toml"]);
        }
        other => panic!("expected Listing, got {other:?}"),
    }
}

#[tokio::test]
async fn find_files_supports_braces_exclusions_kind_and_include_ignored() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();

    fs::write(dir.path().join(".gitignore"), "ignored.ts\n").unwrap();
    fs::create_dir_all(dir.path().join("src").join("fixtures")).unwrap();
    fs::create_dir_all(dir.path().join("target").join("debug")).unwrap();
    fs::create_dir_all(dir.path().join(".git").join("objects")).unwrap();
    fs::write(dir.path().join("src").join("app.ts"), "").unwrap();
    fs::write(dir.path().join("src").join("view.tsx"), "").unwrap();
    fs::write(dir.path().join("src").join("notes.md"), "").unwrap();
    fs::write(dir.path().join("src").join("fixtures").join("mock.ts"), "").unwrap();
    fs::write(dir.path().join("ignored.ts"), "").unwrap();
    fs::write(dir.path().join("target").join("debug").join("out.ts"), "").unwrap();
    fs::write(dir.path().join(".git").join("objects").join("obj.ts"), "").unwrap();

    let tool = FindFilesTool::new(ctx);

    // Brace expansion + exclude
    let res = tool
        .execute(
            &t_ctx,
            json!({
                "pattern": "**/*.{ts,tsx}",
                "exclude": ["**/fixtures/**"]
            }),
        )
        .await
        .unwrap();
    let text = res.content();
    assert!(text.contains("src/app.ts"), "{text}");
    assert!(text.contains("src/view.tsx"), "{text}");
    assert!(!text.contains("notes.md"), "{text}");
    assert!(!text.contains("fixtures/mock.ts"), "{text}");
    assert!(!text.contains("ignored.ts"), "{text}");

    // kind=dirs
    let dirs = tool
        .execute(&t_ctx, json!({"kind": "dirs", "max_depth": 2}))
        .await
        .unwrap();
    let dir_text = dirs.content();
    assert!(dir_text.contains("src/"), "{dir_text}");
    assert!(dir_text.contains("src/fixtures/"), "{dir_text}");
    assert!(!dir_text.contains("app.ts"), "{dir_text}");

    // include_ignored=true surfaces gitignored & target/ but skips .git internals
    let ignored = tool
        .execute(&t_ctx, json!({"pattern": "*.ts", "include_ignored": true}))
        .await
        .unwrap();
    let ignored_text = ignored.content();
    assert!(ignored_text.contains("ignored.ts"), "{ignored_text}");
    assert!(
        ignored_text.contains("target/debug/out.ts"),
        "{ignored_text}"
    );
    assert!(!ignored_text.contains(".git/objects"), "{ignored_text}");
}

#[tokio::test]
async fn find_files_discloses_truncation_and_sorts_recursive_by_mtime() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();

    fs::write(dir.path().join("a_older.rs"), "old").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(dir.path().join("z_newer.rs"), "new").unwrap();
    fs::write(dir.path().join("m_mid.rs"), "mid").unwrap();

    let out = FindFilesTool::new(ctx)
        .execute(&t_ctx, json!({"pattern": "*.rs", "limit": 2}))
        .await
        .unwrap();

    match &out {
        ToolOutput::Listing {
            entries,
            total,
            truncated,
            hint,
        } => {
            assert_eq!(*total, Some(3));
            assert!(*truncated);
            assert_eq!(entries.len(), 2);
            assert!(
                hint.as_deref()
                    .is_some_and(|h| h.contains("Truncated at limit=2"))
            );
            // Newest file must not be starved by alphabetical order.
            assert!(entries.contains(&"z_newer.rs".to_string()), "{entries:?}");
        }
        other => panic!("expected Listing, got {other:?}"),
    }
    let rendered = out.to_text();
    assert!(rendered.contains("[Showing 2 of 3 entries]"), "{rendered}");
    assert!(rendered.contains("Truncated at limit=2"), "{rendered}");
}

#[tokio::test]
async fn find_files_rejects_zero_depth_and_invalid_enum_values() {
    let dir = tempdir().unwrap();
    let tool = FindFilesTool::new(Arc::new(SystemToolContext::new(dir.path())));
    let t_ctx = ToolContext::default();

    assert!(
        tool.execute(&t_ctx, json!({"max_depth": 0}))
            .await
            .unwrap_err()
            .to_string()
            .contains("at least 1")
    );
    assert!(
        tool.execute(&t_ctx, json!({"kind": "sockets"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid `kind`")
    );
    assert!(
        tool.execute(&t_ctx, json!({"sort": "random"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid `sort`")
    );
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

#[tokio::test]
async fn search_text_filters_by_include_and_case() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();
    fs::write(dir.path().join("a.rs"), "Needle here\n").unwrap();
    fs::write(dir.path().join("b.md"), "Needle here\n").unwrap();

    let out = SearchTextTool::new(ctx)
        .execute(
            &t_ctx,
            json!({
                "query": "needle",
                "case_insensitive": true,
                "include": ["*.rs"]
            }),
        )
        .await
        .unwrap();
    let content = out.content();
    assert!(content.contains("a.rs:1:"), "rs file must match: {content}");
    assert!(
        !content.contains("b.md"),
        "md file must be filtered: {content}"
    );
}

#[tokio::test]
async fn search_text_emits_context_lines() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();
    fs::write(
        dir.path().join("a.rs"),
        "before\nneedle\nafter\nunrelated\n",
    )
    .unwrap();

    let out = SearchTextTool::new(ctx)
        .execute(&t_ctx, json!({"query": "needle", "context": 1}))
        .await
        .unwrap();
    let content = out.content();
    assert!(
        content.contains("a.rs-1- before"),
        "leading context: {content}"
    );
    assert!(content.contains("a.rs:2: needle"), "match line: {content}");
    assert!(
        content.contains("a.rs-3- after"),
        "trailing context: {content}"
    );
    assert!(
        !content.contains("unrelated"),
        "context must be bounded: {content}"
    );
}

#[tokio::test]
async fn search_text_bounds_oversized_single_line_and_total_output() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();
    let huge_line = format!("needle {}", "x".repeat(5_000));
    fs::write(dir.path().join("minified.js"), huge_line).unwrap();

    let out = SearchTextTool::new(ctx)
        .execute(&t_ctx, json!({"query": "needle"}))
        .await
        .unwrap();
    let content = out.content();
    assert!(
        content.contains("minified.js:1: needle "),
        "should still report the match line prefix: {content}"
    );
    assert!(
        content.contains("line truncated to 500 chars"),
        "long line must carry explicit truncation notice: {content}"
    );
    assert!(
        content.len() < 1_000,
        "clamped match should stay tiny, got {} bytes",
        content.len()
    );
}

#[tokio::test]
async fn stale_version_refuses_edit_and_write() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(SystemToolContext::new(dir.path()));
    let t_ctx = ToolContext::default();
    std::fs::write(dir.path().join("v.txt"), "one\ntwo\n").unwrap();

    let read = ReadTool::new(ctx.clone())
        .execute(&t_ctx, json!({"path": "v.txt"}))
        .await
        .unwrap();
    let version = match read {
        ToolOutput::Code { version, .. } => version.expect("read must carry a version"),
        other => panic!("expected Code, got {other:?}"),
    };

    // A stale version (the file changed after the read) is refused.
    std::fs::write(dir.path().join("v.txt"), "one\nTWO\n").unwrap();
    let stale = EditTextTool::new(ctx.clone())
        .execute(
            &t_ctx,
            json!({
                "path": "v.txt",
                "old_string": "TWO",
                "new_string": "three",
                "expected_version": version
            }),
        )
        .await
        .unwrap_err();
    assert!(
        stale.to_string().contains("changed since it was read"),
        "{stale}"
    );

    // Re-reading yields the current version, which is accepted.
    let read = ReadTool::new(ctx.clone())
        .execute(&t_ctx, json!({"path": "v.txt"}))
        .await
        .unwrap();
    let version = match read {
        ToolOutput::Code { version, .. } => version.expect("version"),
        other => panic!("expected Code, got {other:?}"),
    };
    let edited = EditTextTool::new(ctx.clone())
        .execute(
            &t_ctx,
            json!({
                "path": "v.txt",
                "old_string": "TWO",
                "new_string": "three",
                "expected_version": version
            }),
        )
        .await
        .unwrap();
    assert!(!edited.is_error());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("v.txt")).unwrap(),
        "one\nthree\n"
    );
}
