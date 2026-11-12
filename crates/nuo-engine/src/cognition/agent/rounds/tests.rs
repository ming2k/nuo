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
    assert!(!content.contains("Use `read_text"));
    assert!(!content.contains("Claim-Check Stub"));
    assert!(!content.contains("call:call_test_spill_123"));

    // Verify the spool file was actually created on disk
    let spool_file = nuo_tool::spool_root_dir().join(format!(
        "{}.log",
        nuo_tool::vfs_short_hash(
            nuo_tool::tool_output::shell_inner_text(
                "line 1\nline 2\n",
                "warning in build\n",
                Some(0)
            )
            .as_bytes()
        )
    ));
    assert!(spool_file.exists());
    let disk_content = std::fs::read_to_string(&spool_file).unwrap();
    assert!(disk_content.contains("line 1\nline 2"));
    assert!(disk_content.contains("warning in build"));
    let _ = std::fs::remove_file(spool_file);
}

#[test]
fn test_vfs_read_text_universal_immunity() {
    let call = ToolCall {
        id: "call_vfs_read_456".to_string(),
        name: "read_text".to_string(),
        arguments: r#"{"path":"/tmp/nuo/vfs/fold:ckpt_1.txt"}"#.to_string(),
    };

    // Create large text that exceeds SHELL_MAX_OUTPUT_CHARS (8000 bytes)
    let large_text = "a".repeat(12000);
    let result = ToolOutput::Text(large_text.clone());

    let msg = format_tool_result_message(&call, &result, &large_text);
    let content = &msg.content;
    // VFS Immunity invariant [INV-VFS-02]: must NOT be truncated, spilled, or stubbed
    assert!(content.starts_with("[read_text result]:\n"));
    assert!(!content.contains("[Output truncated:"));
    assert!(!content.contains("[Full raw output saved to:"));
    assert!(!content.contains("Claim-Check Stub"));
    assert_eq!(content.len(), "[read_text result]:\n".len() + 12000);
}
