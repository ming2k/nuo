//! Agent streaming-round tests.

use super::*;
use nuo_tool::ToolOutput;

#[test]
fn test_shell_output_spilling_and_guidance() {
    let call = ToolCall {
        id: "call_test_spill_123".to_string(),
        name: "execute_command".to_string(),
        arguments: r#"{"command":"cargo test"}"#.to_string(),
    };

    let result = ToolOutput::Shell {
        command: "cargo test".to_string(),
        stdout: "line 1\nline 2\n".to_string(),
        stderr: "warning in build\n".to_string(),
        exit: Some(0),
        lines: vec![],
        truncated: true,
        truncated_note: None,
        termination: nuo_tool::ShellTermination::Exited,
        detached_job_id: None,
    };

    let msg = format_tool_result_message(&call, &result, &result.to_text());
    let content = &msg.content;
    // ADR-0093 [INV-SPOOL-01]: Ephemeral session spool path
    assert!(content.contains("- Spool File: \""));
    assert!(content.contains(".log\""));
    // ADR-0093 [INV-TRUNC-01]: Zero imperative pseudo-guidance to eliminate prompt-injection false positives
    assert!(!content.contains("[Guidance:"));
    assert!(!content.contains("Use `read"));
    assert!(!content.contains("Claim-Check Stub"));
    assert!(!content.contains("call:call_test_spill_123"));

    // Verify the spool file was actually created on disk
    let hash = nuo_tool::vfs_short_hash(
        nuo_tool::tool_output::shell_inner_text("line 1\nline 2\n", "warning in build\n", Some(0))
            .as_bytes(),
    );
    let spool_file = std::fs::read_dir(nuo_tool::spool_root_dir())
        .expect("spool dir")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&hash) && name.ends_with(".log"))
        })
        .expect("spool file with the content hash");
    assert!(spool_file.exists());
    let disk_content = std::fs::read_to_string(&spool_file).unwrap();
    assert!(disk_content.contains("line 1\nline 2"));
    assert!(disk_content.contains("warning in build"));
    let _ = std::fs::remove_file(spool_file);
}

#[test]
fn test_vfs_read_universal_immunity() {
    let call = ToolCall {
        id: "call_vfs_read_456".to_string(),
        name: "read".to_string(),
        arguments: r#"{"path":"/tmp/nuo/vfs/fold:ckpt_1.txt"}"#.to_string(),
    };

    // Create large text that exceeds SHELL_MAX_OUTPUT_CHARS (8000 bytes)
    let large_text = "a".repeat(12000);
    let result = ToolOutput::Text(large_text.clone());

    let msg = format_tool_result_message(&call, &result, &large_text);
    let content = &msg.content;
    // VFS Immunity invariant [INV-VFS-02]: must NOT be truncated, spilled, or stubbed
    assert!(content.starts_with("[read result]:\n"));
    assert!(!content.contains("[Output truncated:"));
    assert!(!content.contains("[Full raw output saved to:"));
    assert!(!content.contains("Claim-Check Stub"));
    assert_eq!(content.len(), "[read result]:\n".len() + 12000);
}

#[test]
fn structured_and_bounded_tool_outputs_bypass_8kb_spooling() {
    // 1. Paginated source read (25 KB > 8000 bytes) must stay verbatim.
    let read_call = ToolCall {
        id: "call_read_code".to_string(),
        name: "read".to_string(),
        arguments: r#"{"path":"crates/nuo-engine/src/lib.rs"}"#.to_string(),
    };
    let code_body = "let x = 1;\n".repeat(2300);
    let code_output = ToolOutput::Code {
        lang: Some("rust".to_string()),
        text: code_body,
        start_line: 1,
        prefix: None,
        suffix: None,

        version: None,
    };
    let code_text = code_output.to_text();
    assert!(code_text.len() > 20_000);
    let msg = format_tool_result_message(&read_call, &code_output, &code_text);
    assert!(!msg.content.contains("- Spool File:"));
    assert!(!msg.content.contains("omitted to protect context budget"));
    assert_eq!(msg.content, format!("[read result]:\n{code_text}"));

    // 2. Self-budgeted AST query (16 KB > 8000 bytes) must stay verbatim.
    let ast_call = ToolCall {
        id: "call_code_query".to_string(),
        name: "code_query".to_string(),
        arguments: r#"{"mode":"outline","path":"src/lib.rs"}"#.to_string(),
    };
    let ast_text = "fn sym() {}\n".repeat(1400);
    let ast_output = ToolOutput::Text(ast_text.clone());
    assert!(ast_text.len() > 16_000);
    let ast_msg = format_tool_result_message(&ast_call, &ast_output, &ast_text);
    assert!(!ast_msg.content.contains("- Spool File:"));
    assert_eq!(ast_msg.content, format!("[code_query result]:\n{ast_text}"));

    // 3. Search matches under 64 KB (> 8000 bytes) must stay verbatim.
    let grep_call = ToolCall {
        id: "call_search_text".to_string(),
        name: "search_text".to_string(),
        arguments: r#"{"pattern":"foo"}"#.to_string(),
    };
    let match_lines: Vec<String> = (1..=350)
        .map(|i| format!("src/mod_{i}.rs:{i}:pub fn foo_{i}() {{}}"))
        .collect();
    let grep_output = ToolOutput::Matches {
        pattern: "foo".to_string(),
        lines: match_lines,
    };
    let grep_text = grep_output.to_text();
    assert!(grep_text.len() > 8000 && grep_text.len() < 64 * 1024);
    let grep_msg = format_tool_result_message(&grep_call, &grep_output, &grep_text);
    assert!(!grep_msg.content.contains("- Spool File:"));
    assert_eq!(
        grep_msg.content,
        format!("[search_text result]:\n{grep_text}")
    );
}

#[test]
fn unbounded_generic_tool_output_over_64kb_spools_safely() {
    let mcp_call = ToolCall {
        id: "call_mcp_dump".to_string(),
        name: "mcp_custom_dump".to_string(),
        arguments: "{}".to_string(),
    };
    let huge_text = "x".repeat(80 * 1024);
    let result = ToolOutput::Text(huge_text.clone());
    let msg = format_tool_result_message(&mcp_call, &result, &huge_text);
    assert!(msg.content.contains("- Spool File: \""));
    assert!(msg.content.contains("omitted to protect context budget"));

    let hash = nuo_tool::vfs_short_hash(huge_text.as_bytes());
    let spool_file = std::fs::read_dir(nuo_tool::spool_root_dir())
        .expect("spool dir")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&hash) && name.ends_with(".log"))
        })
        .expect("spool file with the content hash");
    assert!(spool_file.exists());
    let _ = std::fs::remove_file(spool_file);
}

#[test]
fn oversized_error_output_is_bounded_and_spooled() {
    let call = ToolCall {
        id: "call_error_spill".to_string(),
        name: "mcp__noisy__fail".to_string(),
        arguments: "{}".to_string(),
    };
    let text = "e".repeat(20 * 1024);
    let result = ToolOutput::Error {
        message: text.clone(),
        detail: None,
    };
    let message = format_tool_result_message(&call, &result, &text);
    assert!(message.content.contains("Spool File"));
    assert!(message.content.len() < 64 * 1024);
}
