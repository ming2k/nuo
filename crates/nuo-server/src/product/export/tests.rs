use super::*;
use nuo_tool::ToolCall;

fn user(content: &str) -> Message {
    Message::new(Role::User, content)
}

fn assistant_with_call(content: &str, call: ToolCall) -> Message {
    let mut m = Message::new(Role::Assistant, content);
    m.tool_calls = Some(vec![call]);
    m
}

fn tool_result(name: &str, output: &str) -> Message {
    let call = ToolCall {
        id: format!("{}_id", name),
        name: name.to_string(),
        arguments: "{}".to_string(),
    };
    Message::tool_result(&call, format!("[{} result]:\n{}", name, output))
}

#[test]
fn renders_conversation_title_and_user_message() {
    let out = format_export_markdown(
        ExportContext {
            session_id: "abcd1234ef",
            provider: "kimi-code",
            model: "kimi-k2.7-code",
        },
        &[user("hello")],
        &[],
    );
    assert!(out.starts_with("# Nuo thread\n\n"));
    assert!(out.contains("## User\n\nhello"));
}

#[test]
fn skips_hidden_and_system_messages() {
    let messages = vec![
        Message::hidden(Role::System, "internal"),
        user("visible"),
        Message::hidden(Role::User, "hidden user prompt"),
    ];
    let out = format_export_markdown(
        ExportContext {
            session_id: "id",
            provider: "p",
            model: "m",
        },
        &messages,
        &[],
    );
    assert!(out.contains("visible"));
    assert!(!out.contains("hidden user prompt"));
    assert!(!out.contains("internal"));
}

#[test]
fn inlines_tool_call_and_result_as_activity() {
    let call = ToolCall {
        id: "bash_1".to_string(),
        name: "execute_command".to_string(),
        arguments: r#"{"command":"ls"}"#.to_string(),
    };
    let messages = vec![
        user("list files"),
        assistant_with_call("", call),
        tool_result("execute_command", "file1\nfile2"),
    ];
    let out = format_export_markdown(
        ExportContext {
            session_id: "id",
            provider: "p",
            model: "m",
        },
        &messages,
        &[],
    );
    assert!(out.contains(
        "## Activity\n\n    $ ls\n    status: Completed | exit 0\n      file1\n      file2\n"
    ));
    // Assistant header should not be emitted when its content was empty
    assert!(!out.contains("## Assistant"));
}

#[test]
fn renders_reasoning_and_assistant_content() {
    let mut msg = Message::new(Role::Assistant, "Here is the plan.");
    msg.reasoning_content = Some("Thinking through options...".to_string());
    let out = format_export_markdown(
        ExportContext {
            session_id: "id",
            provider: "p",
            model: "m",
        },
        &[user("plan"), msg],
        &[],
    );
    assert!(out.contains("## Reasoning\n\nThinking through options...\n\n"));
    assert!(out.contains("## Assistant\n\nHere is the plan.\n\n"));
}

#[test]
fn renders_shell_error_exit_status() {
    let call = ToolCall {
        id: "bash_err".to_string(),
        name: "execute_command".to_string(),
        arguments: r#"{"command":"cargo test"}"#.to_string(),
    };
    let messages = vec![
        assistant_with_call("", call),
        tool_result("execute_command", "test failed\nassertion failed\nexit 101"),
    ];
    let out = format_export_markdown(
        ExportContext {
            session_id: "id",
            provider: "p",
            model: "m",
        },
        &messages,
        &[],
    );
    assert!(out.contains("## Activity\n\n    $ cargo test\n    status: Failed | exit 101\n      test failed\n      assertion failed\n"));
}

#[test]
fn renders_shell_harness_interrupted_status() {
    let call = ToolCall {
        id: "bash_term".to_string(),
        name: "execute_command".to_string(),
        arguments: r#"{"command":"cargo run"}"#.to_string(),
    };
    let messages = vec![
        assistant_with_call("", call),
        tool_result(
            "execute_command",
            "running stream...\n[terminated by harness: output limit reached]",
        ),
    ];
    let out = format_export_markdown(
        ExportContext {
            session_id: "id",
            provider: "p",
            model: "m",
        },
        &messages,
        &[],
    );
    assert!(out.contains("## Activity\n\n    $ cargo run\n    status: Failed | terminated by harness\n      running stream...\n"));
}

#[test]
fn pairs_repeated_same_named_calls_in_order() {
    let call_a = ToolCall {
        id: "bash_a".to_string(),
        name: "execute_command".to_string(),
        arguments: r#"{"command":"echo a"}"#.to_string(),
    };
    let call_b = ToolCall {
        id: "bash_b".to_string(),
        name: "execute_command".to_string(),
        arguments: r#"{"command":"echo b"}"#.to_string(),
    };
    let messages = vec![
        assistant_with_call("", call_a),
        tool_result("execute_command", "first"),
        assistant_with_call("", call_b),
        tool_result("execute_command", "second"),
    ];
    let out = format_export_markdown(
        ExportContext {
            session_id: "id",
            provider: "p",
            model: "m",
        },
        &messages,
        &[],
    );
    let first_call = out.find("echo a").unwrap();
    let first_result = out.find("first").unwrap();
    let second_call = out.find("echo b").unwrap();
    let second_result = out.find("second").unwrap();
    assert!(first_call < first_result);
    assert!(first_result < second_call);
    assert!(second_call < second_result);
}

#[test]
fn notes_interrupted_call_when_no_result() {
    let call = ToolCall {
        id: "bash_1".to_string(),
        name: "execute_command".to_string(),
        arguments: r#"{"command":"sleep 10"}"#.to_string(),
    };
    let messages = vec![user("kick off"), assistant_with_call("", call)];
    let out = format_export_markdown(
        ExportContext {
            session_id: "id",
            provider: "p",
            model: "m",
        },
        &messages,
        &[],
    );
    assert!(out.contains("status: Interrupted"));
    assert!(out.contains("no result recorded"));
}

#[test]
fn empty_session_emits_placeholder() {
    let out = format_export_markdown(
        ExportContext {
            session_id: "id",
            provider: "p",
            model: "m",
        },
        &[],
        &[],
    );
    assert!(out.contains("No user-visible rounds"));
}

#[test]
fn renders_file_and_search_tools_cleanly() {
    let read_call = ToolCall {
        id: "read_1".to_string(),
        name: "read".to_string(),
        arguments: r#"{"path":"src/lib.rs","offset":10,"limit":20}"#.to_string(),
    };
    let search_call = ToolCall {
        id: "search_1".to_string(),
        name: "search_text".to_string(),
        arguments: r#"{"query":"hello","path":"src"}"#.to_string(),
    };
    let messages = vec![
        assistant_with_call("", read_call),
        tool_result("read", "10: fn hello() {}\n11: fn world() {}"),
        assistant_with_call("", search_call),
        tool_result("search_text", "src/lib.rs:10: fn hello() {}"),
    ];
    let out = format_export_markdown(
        ExportContext {
            session_id: "id",
            provider: "p",
            model: "m",
        },
        &messages,
        &[],
    );
    assert!(out.contains("## Activity\n\n    read: src/lib.rs (offset: 10, limit: 20)\n    status: Completed\n      10: fn hello() {}\n      11: fn world() {}\n"));
    assert!(out.contains("## Activity\n\n    search: \"hello\" in src\n    status: Completed\n      src/lib.rs:10: fn hello() {}\n"));
}

#[test]
fn renders_user_images_and_subagent_activity() {
    let mut user_msg = user("check this diagram");
    user_msg.images = Some(vec![nuo_tool::ImagePart {
        mime: "image/png".to_string(),
        data: "abcd".to_string(),
    }]);

    let spawn_call = ToolCall {
        id: "sub_1".to_string(),
        name: "spawn_agent".to_string(),
        arguments: r#"{"description":"analyze performance","role":"explore"}"#.to_string(),
    };
    let mut res = tool_result("spawn_agent", "found no bottleneck");
    res.children = Some(vec![
        user("sub task"),
        assistant_with_call("", ToolCall::new("1", "execute_command", "{}")),
        tool_result("execute_command", "ok"),
    ]);
    res.subagent_meta = Some(SubagentMeta {
        description: Some("analyze performance".to_string()),
        duration_ms: Some(1500),
        ..Default::default()
    });

    let messages = vec![user_msg, assistant_with_call("", spawn_call), res];
    let out = format_export_markdown(
        ExportContext {
            session_id: "id",
            provider: "p",
            model: "m",
        },
        &messages,
        &[],
    );
    assert!(out.contains("## User\n\ncheck this diagram\n\n[Image #1]\n\n"));
    assert!(out.contains("spawn agent: analyze performance (explore)"));
    assert!(out.contains("Subagent transcript: 1 user / 1 assistant / 1 tool messages."));
    assert!(out.contains("Subagent task: analyze performance"));
    assert!(out.contains("Subagent duration: 1500ms"));
}

#[test]
fn renders_command_ledger_as_distinct_blockquotes() {
    let commands = vec![
        nuo_protocol::CommandRecord::new("search", "foo").with_result(
            nuo_protocol::CommandResult::Search {
                query: "foo".to_string(),
                hits: vec![nuo_protocol::SearchHit {
                    text: "match".to_string(),
                    score: 0.5,
                }],
            },
        ),
        nuo_protocol::CommandRecord::new("compact", ""),
    ];
    let out = format_export_markdown(
        ExportContext {
            session_id: "id",
            provider: "p",
            model: "m",
        },
        &[],
        &commands,
    );
    assert!(
        out.contains("> **`/search foo`**"),
        "command invocation exports as a blockquote: {out}"
    );
    assert!(
        out.contains("Relevant history (most similar first):"),
        "typed result body exports: {out}"
    );
    assert!(
        out.contains("> `/compact`"),
        "result-less command invocation exports: {out}"
    );
    assert!(
        !out.contains("## User"),
        "commands never render as user headings: {out}"
    );
    assert!(
        !out.contains("No user-visible rounds"),
        "a command-only session still exports content: {out}"
    );
}
