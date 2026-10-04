//! Comprehensive unit tests for Capability Seams and Middlewares.

use super::*;
use crate::tools::{EditTextTool, ListDirTool, ReadTextTool, WriteFileTool};
use nuo_wire::execution::{ExecutionEnvironment, ToolMiddleware};
use nuo_wire::{Tool, ToolOutput};
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::test]
async fn in_memory_fs_roundtrip() {
    let env = InMemoryExecutionEnvironment::new("/virtual/workspace");
    let fs = env.fs();

    let test_file = PathBuf::from("/virtual/workspace/src/main.rs");
    assert!(!fs.exists(&test_file).await);

    fs.write(&test_file, b"fn main() { println!(\"hello\"); }")
        .await
        .unwrap();
    assert!(fs.exists(&test_file).await);
    assert!(fs.is_file(&test_file).await);
    assert!(!fs.is_dir(&test_file).await);

    let content = fs.read_to_string(&test_file).await.unwrap();
    assert_eq!(content, "fn main() { println!(\"hello\"); }");

    let meta = fs.metadata(&test_file).await.unwrap();
    assert_eq!(meta.len, content.len() as u64);
    assert!(meta.is_file);

    let entries = fs
        .list_dir(&PathBuf::from("/virtual/workspace/src"))
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "main.rs");
    assert_eq!(entries[0].size_bytes, content.len() as u64);

    fs.remove_file(&test_file).await.unwrap();
    assert!(!fs.exists(&test_file).await);
}

#[tokio::test]
async fn mock_process_subagent_scripted_response() {
    let env = InMemoryExecutionEnvironment::new("/virtual/workspace");
    let subagent = env.process_runner();

    subagent
        .register(
            "cargo build",
            nuo_wire::execution::ProcessOutput {
                exit_code: Some(0),
                stdout: b"Compiling muta v0.1.0\nFinished dev target(s)".to_vec(),
                stderr: Vec::new(),
                timed_out: false,
            },
        )
        .await;

    let out = env
        .process()
        .exec(
            "cargo build",
            &PathBuf::from("/virtual/workspace"),
            None,
            std::time::Duration::from_secs(5),
        )
        .await
        .unwrap();

    assert!(out.is_success());
    assert_eq!(out.exit_code, Some(0));
    assert!(out.stdout_lossy().contains("Compiling muta"));
}

#[tokio::test]
async fn tools_running_on_system_tool_context() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Arc::new(crate::tools::SystemToolContext::new(dir.path()));

    // 1. WriteFileTool creates file
    let write_tool = WriteFileTool::new(ctx.clone());
    let _write_res = write_tool
        .call_structured(
            r#"{"path":"lib.rs","content":"pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n"}"#,
        )
        .await
        .unwrap();
    assert!(dir.path().join("lib.rs").exists());

    // 2. ReadTextTool reads file
    let read_tool = ReadTextTool::new(ctx.clone());
    let read_res = read_tool
        .call_structured(r#"{"path":"lib.rs"}"#)
        .await
        .unwrap();
    assert!(read_res.to_text().contains("pub fn add"));

    // 3. EditTextTool edits file
    let edit_tool = EditTextTool::new(ctx.clone());
    let _edit_res = edit_tool
        .call_structured(r#"{"path":"lib.rs","old_string":"a + b","new_string":"a + b + 1"}"#)
        .await
        .unwrap();
    let updated = std::fs::read_to_string(dir.path().join("lib.rs")).unwrap();
    assert!(updated.contains("a + b + 1"));

    // 4. ListDirTool lists directory
    let list_tool = ListDirTool::new(ctx.clone());
    let list_res = list_tool.call_structured(r#"{"path":"."}"#).await.unwrap();
    assert!(list_res.to_text().contains("lib.rs"));
}

#[tokio::test]
async fn secret_scrub_middleware_redacts_credentials() {
    let middleware = SecretScrubMiddleware;
    let env = InMemoryExecutionEnvironment::new("/virtual/workspace");

    let mut output = ToolOutput::Shell {
        command: "export".to_string(),
        stdout: "OPENAI_API_KEY=sk-proj-abc1234567890abcdef1234567890\nGITHUB_TOKEN=ghp_1234567890abcdef1234567890abcdef1234\n".to_string(),
        stderr: String::new(),
        lines: Vec::new(),
        exit: Some(0),
        truncated: false,
        termination: nuo_wire::ShellTermination::Exited,
        detached_job_id: None,
    };

    middleware
        .post_execute("execute_command", &mut output, &env)
        .await
        .unwrap();

    let text = output.to_text();
    assert!(!text.contains("sk-proj-abc"));
    assert!(text.contains("[REDACTED_OPENAI_KEY]"));
    assert!(!text.contains("ghp_123456"));
    assert!(text.contains("[REDACTED_GITHUB_TOKEN]"));
}

#[tokio::test]
async fn workspace_jail_middleware_blocks_sensitive_roots() {
    let mut env = InMemoryExecutionEnvironment::new("/virtual/workspace");
    let jail = WorkspaceJailMiddleware;

    let ok_args = serde_json::json!({ "path": "src/main.rs" });
    assert!(jail.pre_execute("read_text", &ok_args, &env).await.is_ok());

    // A traversal is platform-independent and exercises the actual jail
    // invariant; hard-coding `/etc` silently becomes a relative path on
    // Windows and leaves the dangerous `..` case untested everywhere.
    let jail_args = serde_json::json!({ "path": "../secret" });
    let res = jail.pre_execute("read_text", &jail_args, &env).await;
    assert!(res.is_err());
    assert!(res.unwrap_err().contains("Security Denial"));

    // Tilde path outside workspace is also blocked when confined
    let tilde_args = serde_json::json!({ "path": "~/.local/state/muta/auth.toml" });
    let res_tilde = jail.pre_execute("search_text", &tilde_args, &env).await;
    assert!(res_tilde.is_err());
    assert!(res_tilde.unwrap_err().contains("Security Denial"));

    // When confinement is disabled, both are allowed
    env.set_confined(false);
    assert!(
        jail.pre_execute("read_text", &jail_args, &env)
            .await
            .is_ok()
    );
    assert!(
        jail.pre_execute("search_text", &tilde_args, &env)
            .await
            .is_ok()
    );
}
