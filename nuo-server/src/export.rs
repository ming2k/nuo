//! Conversation exporter: renders the durable [`Message`] stream as a single
//! Markdown document suitable for clipboard copying and handoff. Triggered by
//! the `/export` slash command, which copies the result to the system clipboard.
//!
//! Format matches the clean conversational Markdown specification:
//! - Top-level `# Nuo conversation` title.
//! - Clean `## User`, `## Reasoning`, `## Assistant`, and `## Activity` sections.
//! - Activities render tool invocations and outputs as indented code blocks (4 spaces).
//! - Hidden and system messages are skipped. Subagent transcripts are summarised inline.

use nuo_wire::{Message, Role, SubagentMeta, ToolCall};

/// Metadata carried from the harness into the exporter so the header reflects
/// the live session state at the moment of export.
#[derive(Debug, Clone)]
pub struct ExportContext<'a> {
    pub session_id: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
}

/// Render the current conversation as a Markdown handoff document.
///
/// `commands` is the ADR-0091 command ledger: slash commands (and `!cmd`
/// passthroughs) are operations, not conversation, so they render as a
/// distinct blockquote block after the dialogue instead of a `## User`
/// heading, keeping the dialogue pure.
pub fn format_export_markdown(
    _ctx: ExportContext<'_>,
    messages: &[Message],
    commands: &[nuo_wire::CommandRecord],
) -> String {
    let mut out = String::from("# Nuo conversation\n\n");
    let mut emitted_any = false;
    let mut tool_call_cursor: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();

    for message in messages {
        if message.hidden || message.role == Role::System {
            continue;
        }
        match message.role {
            Role::User => {
                let content = pick_content(message).trim();
                if content.is_empty() {
                    continue;
                }
                emitted_any = true;
                out.push_str("## User\n\n");
                out.push_str(content);
                if let Some(images) = message.images.as_ref() {
                    for (i, _) in images.iter().enumerate() {
                        let label = format!("[Image #{}]", i + 1);
                        if !content.contains(&label) {
                            out.push_str("\n\n");
                            out.push_str(&label);
                        }
                    }
                }
                out.push_str("\n\n");
            }
            Role::Assistant => {
                if let Some(reasoning) = message.reasoning_content.as_deref()
                    && !reasoning.trim().is_empty()
                {
                    emitted_any = true;
                    out.push_str("## Reasoning\n\n");
                    out.push_str(reasoning.trim());
                    out.push_str("\n\n");
                }
                let content = pick_content(message).trim();
                if !content.is_empty() {
                    emitted_any = true;
                    out.push_str("## Assistant\n\n");
                    out.push_str(content);
                    out.push_str("\n\n");
                }
                if let Some(calls) = message.tool_calls.as_ref() {
                    for call in calls {
                        emitted_any = true;
                        render_activity(call, messages, &mut tool_call_cursor, &mut out);
                    }
                }
            }
            Role::Tool => {
                // Tool results are inlined next to their originating call under
                // ## Activity by `render_activity`, so a standalone Tool message
                // here is a result whose matching call lived in a turn we skipped
                // (e.g. a hidden injection). Drop it to keep the transcript clean.
            }
            Role::System => {}
        }
    }

    // Command ledger (ADR-0091): operations are not dialogue, so they render
    // as a distinct blockquote block rather than a `## User` heading.
    for record in commands {
        emitted_any = true;
        let invocation = if record.args.is_empty() {
            format!("/{}", record.name)
        } else {
            format!("/{} {}", record.name, record.args)
        };
        match &record.result {
            Some(result) => {
                out.push_str(&format!("> **`{}`**\n>\n", invocation));
                for line in result.to_text().lines() {
                    if line.is_empty() {
                        out.push_str(">\n");
                    } else {
                        out.push_str(&format!("> {}\n", line));
                    }
                }
                out.push('\n');
            }
            None => {
                out.push_str(&format!("> `{}`\n\n", invocation));
            }
        }
    }

    if !emitted_any {
        out.push_str("_(No user-visible rounds in this session yet.)_\n");
    }

    out
}

/// Choose the text we render for a message: `display_content` (the harness's
/// curated view) when present, otherwise the raw `content`.
fn pick_content(message: &Message) -> &str {
    if let Some(display) = message.display_content.as_deref() {
        display
    } else {
        &message.content
    }
}

/// Render a single tool invocation and its matching result under an indented
/// `## Activity` code block.
fn render_activity<'a>(
    call: &'a ToolCall,
    messages: &[Message],
    cursor: &mut std::collections::HashMap<&'a str, usize>,
    out: &mut String,
) {
    let matched_result = messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .find(|m| m.tool_call_id.as_deref() == Some(&call.id))
        .or_else(|| {
            let slot = {
                let entry = cursor.entry(call.name.as_str()).or_insert(0);
                let current = *entry;
                *entry += 1;
                current
            };
            let matches: Vec<&Message> = messages
                .iter()
                .filter(|m| m.role == Role::Tool)
                .filter(|m| {
                    parse_tool_result(&m.content).is_some_and(|(name, _)| name == call.name)
                })
                .collect();
            matches.get(slot).copied()
        });

    let parsed_output = matched_result.and_then(|m| {
        parse_tool_result(&m.content)
            .map(|(_, output)| output)
            .or(Some(&m.content))
    });
    let children = matched_result.and_then(|m| m.children.as_deref());
    let subagent_meta = matched_result.and_then(|m| m.subagent_meta.as_ref());

    let (header, status, body) = format_tool_activity(call, parsed_output, children, subagent_meta);
    push_activity_block(out, &header, &status, &body);
}

/// Format the activity lines: command / tool summary line, status line, and body lines.
fn format_tool_activity(
    call: &ToolCall,
    result_output: Option<&str>,
    children: Option<&[Message]>,
    subagent_meta: Option<&SubagentMeta>,
) -> (String, String, Vec<String>) {
    let json_args: Option<serde_json::Value> = serde_json::from_str(&call.arguments).ok();

    let (header, status, mut body) = match call.name.as_str() {
        "execute_command" => {
            let cmd = json_args
                .as_ref()
                .and_then(|v| v.get("command").and_then(|c| c.as_str()))
                .unwrap_or(&call.arguments);
            let header = format!("$ {cmd}");
            match result_output {
                None => (
                    header,
                    "status: Interrupted".to_string(),
                    vec!["(no result recorded — the call may have been interrupted)".to_string()],
                ),
                Some(output) => {
                    let (status, lines) = parse_shell_output(output);
                    (header, status, lines)
                }
            }
        }
        "read_text" | "read_file" => {
            let path = json_args
                .as_ref()
                .and_then(|v| v.get("path").and_then(|p| p.as_str()))
                .unwrap_or("");
            let offset = json_args
                .as_ref()
                .and_then(|v| v.get("offset").and_then(|o| o.as_u64()));
            let limit = json_args
                .as_ref()
                .and_then(|v| v.get("limit").and_then(|l| l.as_u64()));
            let header = match (offset, limit) {
                (Some(o), Some(l)) => format!("read: {path} (offset: {o}, limit: {l})"),
                (Some(o), None) => format!("read: {path} (offset: {o})"),
                (None, Some(l)) => format!("read: {path} (limit: {l})"),
                (None, None) => format!("read: {path}"),
            };
            let (status, lines) = format_generic_result(result_output);
            (header, status, lines)
        }
        "edit_text" => {
            let path = json_args
                .as_ref()
                .and_then(|v| v.get("path").and_then(|p| p.as_str()))
                .unwrap_or("");
            let header = format!("edit: {path}");
            let (status, lines) = format_generic_result(result_output);
            (header, status, lines)
        }
        "write_file" => {
            let path = json_args
                .as_ref()
                .and_then(|v| v.get("path").and_then(|p| p.as_str()))
                .unwrap_or("");
            let header = format!("write: {path}");
            let (status, lines) = format_generic_result(result_output);
            (header, status, lines)
        }
        "search_text" => {
            let query = json_args
                .as_ref()
                .and_then(|v| v.get("query").and_then(|q| q.as_str()))
                .unwrap_or("");
            let path = json_args
                .as_ref()
                .and_then(|v| v.get("path").and_then(|p| p.as_str()));
            let header = match path {
                Some(p) if !p.is_empty() && p != "." => format!("search: \"{query}\" in {p}"),
                _ => format!("search: \"{query}\""),
            };
            let (status, lines) = format_generic_result(result_output);
            (header, status, lines)
        }
        "find_files" => {
            let patterns = json_args
                .as_ref()
                .and_then(|v| v.get("patterns"))
                .and_then(|p| serde_json::to_string(p).ok())
                .unwrap_or_else(|| "*".to_string());
            let path = json_args
                .as_ref()
                .and_then(|v| v.get("path").and_then(|p| p.as_str()));
            let header = match path {
                Some(p) if !p.is_empty() && p != "." => format!("find files: {patterns} in {p}"),
                _ => format!("find files: {patterns}"),
            };
            let (status, lines) = format_generic_result(result_output);
            (header, status, lines)
        }
        "list_dir" => {
            let path = json_args
                .as_ref()
                .and_then(|v| v.get("path").and_then(|p| p.as_str()))
                .unwrap_or(".");
            let header = format!("list dir: {path}");
            let (status, lines) = format_generic_result(result_output);
            (header, status, lines)
        }
        "code_query" => {
            let mode = json_args
                .as_ref()
                .and_then(|v| v.get("mode").and_then(|m| m.as_str()))
                .unwrap_or("query");
            let path = json_args
                .as_ref()
                .and_then(|v| v.get("path").and_then(|p| p.as_str()))
                .unwrap_or("");
            let sym = json_args
                .as_ref()
                .and_then(|v| v.get("symbol").and_then(|s| s.as_str()));
            let pat = json_args
                .as_ref()
                .and_then(|v| v.get("pattern").and_then(|p| p.as_str()));
            let target = sym.or(pat).unwrap_or(path);
            let header = format!("code query: {mode} {target}");
            let (status, lines) = format_generic_result(result_output);
            (header, status, lines)
        }
        "spawn_agent" => {
            let desc = json_args
                .as_ref()
                .and_then(|v| v.get("description").and_then(|d| d.as_str()))
                .unwrap_or("");
            let role = json_args
                .as_ref()
                .and_then(|v| v.get("role").and_then(|r| r.as_str()));
            let header = match role {
                Some(r) => format!("spawn agent: {desc} ({r})"),
                None => format!("spawn agent: {desc}"),
            };
            let (status, lines) = format_generic_result(result_output);
            (header, status, lines)
        }
        "todo" => {
            let header = "todo".to_string();
            let (status, lines) = format_generic_result(result_output);
            (header, status, lines)
        }
        "ask_user" => {
            let header = "ask user".to_string();
            let (status, lines) = format_generic_result(result_output);
            (header, status, lines)
        }
        name => {
            let compact_args = json_args
                .as_ref()
                .and_then(|v| serde_json::to_string(v).ok())
                .unwrap_or_else(|| call.arguments.clone());
            let header = if compact_args == "{}" || compact_args.is_empty() {
                format!("tool: {name}")
            } else {
                format!("tool: {name}({compact_args})")
            };
            let (status, lines) = format_generic_result(result_output);
            (header, status, lines)
        }
    };

    if let Some(children) = children
        && !children.is_empty()
    {
        let user_count = children.iter().filter(|m| m.role == Role::User).count();
        let assistant_count = children
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .count();
        let tool_count = children.iter().filter(|m| m.role == Role::Tool).count();
        body.push(format!(
            "Subagent transcript: {user_count} user / {assistant_count} assistant / {tool_count} tool messages."
        ));
    }
    if let Some(meta) = subagent_meta {
        if let Some(desc) = meta.description.as_deref()
            && !desc.is_empty()
        {
            body.push(format!("Subagent task: {desc}"));
        }
        if let Some(duration_ms) = meta.duration_ms {
            body.push(format!("Subagent duration: {duration_ms}ms"));
        }
    }

    (header, status, body)
}

/// Parse shell stdout/stderr and exit status from Nuo's shell tool output.
fn parse_shell_output(output: &str) -> (String, Vec<String>) {
    let trimmed = output.trim_matches('\n');
    if trimmed.is_empty() {
        return ("status: Completed · exit 0".to_string(), Vec::new());
    }

    if let Some(rest) = trimmed.strip_prefix("Exit ")
        && let Some((exit_code_str, streams)) = rest.split_once('\n')
    {
        let code = exit_code_str.trim();
        let status = format!("status: Failed · exit {code}");
        let mut lines = Vec::new();
        if let Some((stdout_part, stderr_part)) = streams.split_once("\nSTDERR:\n") {
            let stdout_clean = stdout_part.strip_prefix("STDOUT:\n").unwrap_or(stdout_part);
            for line in stdout_clean.lines() {
                if !line.is_empty() {
                    lines.push(line.to_string());
                }
            }
            for line in stderr_part.lines() {
                if !line.is_empty() {
                    lines.push(line.to_string());
                }
            }
        } else {
            for line in streams.lines() {
                lines.push(line.to_string());
            }
        }
        return (status, lines);
    }

    if let Some(stderr) = trimmed.strip_prefix("(success, stderr):\n") {
        let lines = stderr.lines().map(|l| l.to_string()).collect();
        return ("status: Completed · exit 0".to_string(), lines);
    }

    if trimmed.starts_with("Error:") || trimmed.starts_with("Failed:") {
        let lines = trimmed.lines().map(|l| l.to_string()).collect();
        return ("status: Failed".to_string(), lines);
    }

    let lines = trimmed.lines().map(|l| l.to_string()).collect();
    ("status: Completed · exit 0".to_string(), lines)
}

/// Fallback output parser for non-shell tools.
fn format_generic_result(output: Option<&str>) -> (String, Vec<String>) {
    match output {
        None => (
            "status: Interrupted".to_string(),
            vec!["(no result recorded — the call may have been interrupted)".to_string()],
        ),
        Some(out) => {
            let trimmed = out.trim_matches('\n');
            if trimmed.starts_with("Error:") || trimmed.starts_with("Failed:") {
                (
                    "status: Failed".to_string(),
                    trimmed.lines().map(|l| l.to_string()).collect(),
                )
            } else {
                (
                    "status: Completed".to_string(),
                    trimmed.lines().map(|l| l.to_string()).collect(),
                )
            }
        }
    }
}

/// Write a single indented code block under `## Activity`.
fn push_activity_block(
    out: &mut String,
    header_line: &str,
    status_line: &str,
    body_lines: &[String],
) {
    out.push_str("## Activity\n\n");
    for line in header_line.lines() {
        out.push_str("    ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.push_str("    ");
    out.push_str(status_line);
    out.push('\n');
    for line in body_lines {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            out.push_str("      \n");
        } else {
            out.push_str("      ");
            out.push_str(trimmed);
            out.push('\n');
        }
    }
    out.push('\n');
}

/// Parse the `[<name> result]:<output>` envelope that wraps Tool-role
/// messages.
fn parse_tool_result(content: &str) -> Option<(&str, &str)> {
    let content = content.strip_prefix('[')?;
    let (name, output) = content.split_once(" result]:")?;
    Some((name, output.trim_start_matches('\n')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_wire::ToolCall;

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
        assert!(out.starts_with("# Nuo conversation\n\n"));
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
            "## Activity\n\n    $ ls\n    status: Completed · exit 0\n      file1\n      file2\n"
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
            tool_result(
                "execute_command",
                "Exit 101\nSTDOUT:\ntest failed\nSTDERR:\nassertion failed",
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
        assert!(out.contains("## Activity\n\n    $ cargo test\n    status: Failed · exit 101\n      test failed\n      assertion failed\n"));
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
            name: "read_text".to_string(),
            arguments: r#"{"path":"src/lib.rs","offset":10,"limit":20}"#.to_string(),
        };
        let search_call = ToolCall {
            id: "search_1".to_string(),
            name: "search_text".to_string(),
            arguments: r#"{"query":"hello","path":"src"}"#.to_string(),
        };
        let messages = vec![
            assistant_with_call("", read_call),
            tool_result("read_text", "10: fn hello() {}\n11: fn world() {}"),
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
        user_msg.images = Some(vec![nuo_wire::ImagePart {
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
            nuo_wire::CommandRecord::new("search", "foo").with_result(
                nuo_wire::CommandResult::Search {
                    query: "foo".to_string(),
                    hits: vec![nuo_wire::SearchHit {
                        text: "match".to_string(),
                        score: 0.5,
                    }],
                },
            ),
            nuo_wire::CommandRecord::new("compact", ""),
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
}
