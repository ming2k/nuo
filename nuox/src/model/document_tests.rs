//! Tests for the transcript document model and markdown parser.

use super::*;

#[test]
fn test_parse_simple_text() {
    let blocks = parse_blocks("Hello world");
    assert_eq!(blocks.len(), 1);
    assert!(matches!(&blocks[0], Block::Text(inline) if inline.content == "Hello world"));
}

#[test]
fn test_parse_code_block() {
    let text = "Some text\n\n```rust\nfn main() {}\n```\n\nMore text";
    let blocks = parse_blocks(text);
    assert_eq!(blocks.len(), 5);
    assert!(matches!(&blocks[0], Block::Text(inline) if inline.content == "Some text"));
    assert!(
        matches!(&blocks[2], Block::Code { language, content } if language.as_deref() == Some("rust") && content == "fn main() {}")
    );
    assert!(matches!(&blocks[4], Block::Text(inline) if inline.content == "More text"));
}

#[test]
fn inline_code_keeps_its_backtick_quotes_in_prose() {
    // Inline code keeps its backtick delimiters in the flattened content
    // so the rendered/copied paragraph still shows the quotes, and the
    // renderer can paint the span on the code surface. This holds across
    // paragraph / heading / list item / quote contexts.
    let blocks = parse_blocks("Call the `read_text` tool.");
    assert!(matches!(
        &blocks[0],
        Block::Text(inline) if inline.content == "Call the `read_text` tool."
    ));

    // Heading.
    let blocks = parse_blocks("# Use `list_dir` for directories");
    assert!(matches!(
        &blocks[0],
        Block::Heading { level: 1, inline } if inline.content == "Use `list_dir` for directories"
    ));

    // List item.
    let blocks = parse_blocks("- item with `code` inside");
    assert!(matches!(
        &blocks[0],
        Block::ListItem { inline, .. } if inline.content == "item with `code` inside"
    ));

    // Blockquote.
    let blocks = parse_blocks("> quoted `code` span");
    assert!(matches!(
        &blocks[0],
        Block::Quote(inline) if inline.content == "quoted `code` span"
    ));

    // Multiple inline spans in one paragraph, mixed with emphasis.
    let blocks = parse_blocks("Mix `a` and `b` and plain.");
    assert!(matches!(
        &blocks[0],
        Block::Text(inline) if inline.content == "Mix `a` and `b` and plain."
    ));
}

/// Helper: find the byte range of the first `` `…` `` run in `s`, matching
/// what the parser records, so the `code_ranges` assertions below can be
/// written against the literal content rather than hand-counted offsets.
fn code_ranges_of(s: &str) -> Vec<CodeRange> {
    let mut ranges = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            // find the closing backtick
            if let Some(rel) = s[i + 1..].find('`') {
                ranges.push((i, i + 1 + rel + 1));
                i = i + 1 + rel + 1;
                continue;
            }
        }
        i += 1;
    }
    ranges
}

#[test]
fn parses_inline_math_and_http_links_outside_code() {
    let text = "Use $x^2$ and [Rust](https://www.rust-lang.org), not `https://ignored.test`.";
    let blocks = parse_blocks(text);
    let Block::Text(inline) = &blocks[0] else {
        panic!("expected text block");
    };
    assert_eq!(inline.math_ranges, vec![(4, 9)]);
    assert_eq!(inline.code_ranges.len(), 1);
    assert_eq!(inline.link_ranges.len(), 1);
    assert_eq!(inline.link_ranges[0].label_range, (15, 19));
    assert_eq!(inline.link_ranges[0].url, "https://www.rust-lang.org");
}

#[test]
fn parses_display_math_blocks() {
    let blocks = parse_blocks("Before\n\n$$\n\\int_0^\\infty e^{-x} dx = 1\n$$\n\nAfter");
    assert!(matches!(&blocks[0], Block::Text(inline) if inline.content == "Before"));
    assert!(matches!(&blocks[2], Block::Math { content } if content.contains("\\int_0^\\infty")));
    assert!(matches!(&blocks[4], Block::Text(inline) if inline.content == "After"));
}

#[test]
fn inline_code_records_byte_ranges_for_every_prose_context() {
    // Paragraph: the run is `read_text` including both backticks.
    let text = "Call the `read_text` tool.";
    let expected = code_ranges_of(text);
    let blocks = parse_blocks(text);
    let Block::Text(inline) = &blocks[0] else {
        panic!("expected Text block, got {:?}", blocks[0]);
    };
    assert_eq!(inline.content, text);
    assert_eq!(inline.code_ranges, expected);

    // Heading.
    let text = "Use `list_dir` for directories";
    let expected = code_ranges_of(text);
    let blocks = parse_blocks(&format!("# {text}"));
    let Block::Heading { inline, .. } = &blocks[0] else {
        panic!("expected Heading block, got {:?}", blocks[0]);
    };
    assert_eq!(inline.content, text);
    assert_eq!(inline.code_ranges, expected);

    // List item.
    let text = "item with `code` inside";
    let expected = code_ranges_of(text);
    let blocks = parse_blocks(&format!("- {text}"));
    let Block::ListItem { inline, .. } = &blocks[0] else {
        panic!("expected ListItem block, got {:?}", blocks[0]);
    };
    assert_eq!(inline.content, text);
    assert_eq!(inline.code_ranges, expected);

    // Blockquote.
    let text = "quoted `code` span";
    let expected = code_ranges_of(text);
    let blocks = parse_blocks(&format!("> {text}"));
    let Block::Quote(inline) = &blocks[0] else {
        panic!("expected Quote block, got {:?}", blocks[0]);
    };
    assert_eq!(inline.content, text);
    assert_eq!(inline.code_ranges, expected);

    // Multiple spans → multiple, non-overlapping, ordered ranges.
    let text = "Mix `a` and `b` and plain.";
    let expected = code_ranges_of(text);
    let blocks = parse_blocks(text);
    let Block::Text(inline) = &blocks[0] else {
        panic!("expected Text block");
    };
    assert_eq!(inline.code_ranges, expected);
}

#[test]
fn test_push_stream() {
    let mut streamed = TranscriptMessage::new(Role::Assistant, "");
    for chunk in [
        "# Result\n\n",
        "First paragraph.\n\n",
        "- one\n",
        "- two\n\n",
        "```rust\nfn main() {}\n```",
    ] {
        streamed.push_stream(chunk);
    }

    let completed = TranscriptMessage::new(Role::Assistant, streamed.raw.clone());
    assert_eq!(streamed.blocks, completed.blocks);
}

#[test]
fn parses_block_boundaries_without_collapsing_the_document() {
    let blocks = parse_blocks(
        "# Result\n\nFirst paragraph.\n\nSecond paragraph.\n\n1. one\n2. two\n\n> quoted",
    );

    assert!(matches!(
        &blocks[0],
        Block::Heading { level: 1, inline } if inline.content == "Result"
    ));
    assert!(blocks.iter().any(|block| matches!(block, Block::Break)));
    assert!(
        blocks.iter().any(
            |block| matches!(block, Block::Text(inline) if inline.content == "First paragraph.")
        )
    );
    assert!(blocks.iter().any(
        |block| matches!(block, Block::Text(inline) if inline.content == "Second paragraph.")
    ));
    assert!(blocks.iter().any(|block| matches!(
        block,
        Block::ListItem {
            inline,
            ordered: Some(1),
            ..
        } if inline.content == "one"
    )));
    assert!(
        blocks
            .iter()
            .any(|block| matches!(block, Block::Quote(inline) if inline.content == "quoted"))
    );
}

#[test]
fn headings_are_visually_separated_from_following_body_text() {
    let blocks = parse_blocks("# Result\nFirst paragraph.");

    assert!(matches!(&blocks[0], Block::Heading { inline, .. } if inline.content == "Result"));
    assert!(
        matches!(&blocks[1], Block::Break),
        "heading-to-text boundaries should render with a blank row"
    );
    assert!(matches!(&blocks[2], Block::Text(inline) if inline.content == "First paragraph."));
}

#[test]
fn markdown_soft_breaks_flow_but_hard_breaks_are_preserved() {
    let soft = parse_blocks("alpha bravo\ncharlie delta");
    assert!(matches!(
        &soft[0],
        Block::Text(inline) if inline.content == "alpha bravo charlie delta"
    ));

    let hard = parse_blocks("alpha bravo  \ncharlie delta");
    assert!(matches!(
        &hard[0],
        Block::Text(inline) if inline.content == "alpha bravo\ncharlie delta"
    ));
}

#[test]
fn parses_task_lists_and_tables() {
    let blocks =
        parse_blocks("- [x] done\n- [ ] next\n\n| Name | State |\n| --- | --- |\n| muta | ready |");

    assert!(blocks.iter().any(|block| matches!(
        block,
        Block::ListItem {
            checked: Some(true),
            inline,
            ..
        } if inline.content == "done"
    )));
    assert!(blocks.iter().any(|block| matches!(
        block,
        Block::ListItem {
            checked: Some(false),
            inline,
            ..
        } if inline.content == "next"
    )));
    let table = blocks.iter().find_map(|block| match block {
        Block::Table { headers, rows, .. } => Some((headers, rows)),
        _ => None,
    });
    let (headers, rows) = table.expect("table block present");
    assert_eq!(headers, &["Name".to_string(), "State".to_string()]);
    assert_eq!(rows, &[vec!["muta".to_string(), "ready".to_string()]]);

    // The rendered grid must align columns and separate the header from
    // the body, the regression that motivated reintroducing Block::Table.
    let rendered = blocks
        .iter()
        .find_map(|block| match block {
            Block::Table { rendered, .. } => Some(rendered.as_str()),
            _ => None,
        })
        .expect("rendered table text");
    assert!(rendered.contains("┌"), "missing top border: {rendered}");
    assert!(
        rendered.contains("├"),
        "missing header/body separator: {rendered}"
    );
    // Pipes must line up: the header and data rows share the same `│`
    // positions, so splitting on `│` yields the same number of pieces.
    let pipes = |line: &str| line.matches('│').count();
    let header_line = rendered.lines().nth(1).unwrap();
    let data_line = rendered.lines().nth(3).unwrap();
    assert_eq!(
        pipes(header_line),
        pipes(data_line),
        "header and body rows must align: {rendered}"
    );
}

#[test]
fn table_alignment_and_uneven_cells_line_up() {
    let blocks =
        parse_blocks("| Tool | Count |\n| :--- | ---: |\n| read | 1 |\n| webfetch | 250 |");
    let rendered = blocks
        .iter()
        .find_map(|block| match block {
            Block::Table {
                rendered, aligns, ..
            } => Some((rendered.as_str(), aligns.clone())),
            _ => None,
        })
        .expect("table block");
    let (rendered, aligns) = rendered;
    assert_eq!(
        aligns,
        vec![TableAlignment::Left, TableAlignment::Right],
        "alignment must be captured: {rendered}"
    );
    // Right-aligned numeric column: digits hug the right border, so the
    // single-digit "1" gets more left padding than "250" does.
    let data_lines: Vec<&str> = rendered.lines().skip(3).take(2).collect();
    assert!(
        data_lines[0].ends_with("│     1 │"),
        "got: {}",
        data_lines[0]
    );
    assert!(
        data_lines[1].ends_with("│   250 │"),
        "got: {}",
        data_lines[1]
    );
}

/// GFM fixes the table column count from the header, so every body row in
/// a `Block::Table` must be normalized to exactly `headers.len()` cells:
/// short rows padded with empty strings, over-wide rows truncated. This is
/// the invariant the live renderer indexes against; a ragged row used to
/// panic `build_table_render` with an out-of-bounds index.
#[test]
fn table_normalizes_ragged_body_rows_to_header_width() {
    // 2-column header; body rows have 2, 1, and 3 cells respectively.
    let blocks = parse_blocks("| A | B |\n|---|---|\n| 1 | 2 |\n| 3 |\n| 4 | 5 | 6 |");
    let (headers, rows) = blocks
        .iter()
        .find_map(|block| match block {
            Block::Table { headers, rows, .. } => Some((headers.clone(), rows.clone())),
            _ => None,
        })
        .expect("table block present");
    let ncols = headers.len();
    assert_eq!(ncols, 2, "header defines 2 columns");
    assert!(
        rows.iter().all(|row| row.len() == ncols),
        "every body row must be normalized to {ncols} cells, got {rows:?}"
    );
    // Short rows are padded with empty cells, the over-wide row truncated.
    assert_eq!(rows[0], vec!["1".to_string(), "2".to_string()]);
    assert_eq!(rows[1], vec!["3".to_string(), String::new()]);
    assert_eq!(rows[2], vec!["4".to_string(), "5".to_string()]);
}

#[test]
fn tool_step_collapses_and_restores_full_semantic_detail() {
    let mut message =
        TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"README.md"}"#);
    // Collapsed running: human-readable summary only — no tool name.
    assert!(message.raw.contains("Read README.md"));
    assert!(!message.raw.contains("read_text"));

    assert!(message.finish_tool_step(
        "call_1",
        "contents",
        nuo_wire::ToolOutput::text("contents"),
        1234
    ));
    // Collapsed completed: summary + duration suffix.
    assert!(message.raw.contains("Read README.md"));
    assert!(message.raw.contains("1.2s"));
    message.set_tool_step_expanded(true);

    // Expanded: arguments as compact key-value text + output verbatim.
    assert!(message.raw.contains("path: README.md"));
    assert!(message.raw.contains("contents"));
}

#[test]
fn subagent_task_is_detected_and_addressable() {
    let task = TranscriptMessage::tool_step(
        "call_42",
        "spawn_agent",
        r#"{"description":"explore src","prompt":"..."}"#,
    );
    assert!(task.is_subagent_task());
    assert_eq!(task.tool_step_call_id(), Some("call_42"));
    assert_eq!(task.subagent_children().map(|c| c.len()), Some(0));
    assert_eq!(task.subagent_description(), "explore src");
    assert_eq!(task.subagent_role(), None);

    // A regular tool step is not a subagent task.
    let read = TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"a"}"#);
    assert!(!read.is_subagent_task());
    assert!(read.subagent_status_line().is_none());
}

#[test]
fn subagent_started_event_labels_step_by_role() {
    // A `Started` event stamps the bound profile name on the step so the
    // page header can read the role out as its `[ROLE]` tag.
    let mut task = TranscriptMessage::tool_step(
        "call_7",
        "spawn_agent",
        r#"{"description":"write the plan","prompt":"..."}"#,
    );
    assert_eq!(task.subagent_description(), "write the plan");
    assert_eq!(task.subagent_role(), None);
    assert!(
        task.push_subagent_event(&nuo_wire::SubagentEvent::Started {
            profile: "explore".to_string()
        })
    );
    assert_eq!(task.subagent_role().as_deref(), Some("explore"));
    assert_eq!(task.subagent_description(), "write the plan");
    // The collapsed header carries only the description — the role is
    // shown by the renderer's `[PROFILE]` badge in front of it.
    let header = task.tool_step_summary().expect("summary");
    assert_eq!(header, "write the plan");
}

#[test]
fn subagent_status_reflects_children_and_completion() {
    let mut task = TranscriptMessage::tool_step(
        "call_9",
        "spawn_agent",
        r#"{"description":"d","prompt":"p"}"#,
    );

    // No children yet, still running — the peek row opens with the
    // generic `running` state until the subagent reports more.
    let running = task.subagent_status_line().expect("running status");
    assert!(running.starts_with("running"), "got: {running}");

    // A reported activity line (e.g. during the first model call) is
    // surfaced so the row reads as alive, not stuck on a bare state.
    task.push_subagent_event(&SubagentEvent::Activity("waiting for model".into()));
    let waiting = task.subagent_status_line().expect("waiting status");
    assert!(
        waiting.starts_with("running waiting for model"),
        "got: {waiting}"
    );

    // Streaming assistant text => the peek row reports `thinking`.
    task.push_subagent_event(&SubagentEvent::StreamStart { round: 1, turn: 0 });
    task.push_subagent_event(&SubagentEvent::StreamDelta("partial".into()));
    let thinking = task.subagent_status_line().expect("thinking status");
    assert!(thinking.starts_with("running thinking"), "got: {thinking}");

    // An in-flight child tool call surfaces the tool's header.
    task.push_subagent_event(&SubagentEvent::ToolCall {
        id: "inner".into(),
        name: "search_text".into(),
        arguments: r#"{"query":"foo"}"#.into(),
        round: 1,
        turn: 0,
    });
    let running = task.subagent_status_line().expect("running status");
    assert!(running.contains("Search"), "got: {running}");

    // Completing the parent hides the peek row; the outcome row takes over
    // with the subagent's one-line conclusion.
    assert!(task.finish_tool_step(
        "call_9",
        "final answer",
        nuo_wire::ToolOutput::text("final answer"),
        1500
    ));
    assert!(
        task.subagent_status_line().is_none(),
        "the peek row must disappear once the subagent terminates"
    );
    assert_eq!(
        task.subagent_outcome_line().as_deref(),
        Some("final answer"),
        "the outcome row carries the subagent's conclusion"
    );

    // Children are accessible for the dedicated subagent view.
    assert_eq!(task.subagent_children().map(|c| c.len()), Some(2));
}

#[test]
fn subagent_failed_status_reports_failure() {
    let mut task =
        TranscriptMessage::tool_step("c", "spawn_agent", r#"{"description":"d","prompt":"p"}"#);
    task.push_subagent_event(&SubagentEvent::ToolCall {
        id: "i".into(),
        name: "execute_command".into(),
        arguments: "{}".into(),
        round: 1,
        turn: 0,
    });
    // The subagent failure is now signalled by the structured `failed`
    // flag on `ToolOutput::Subagent`, not by an "Error:" text prefix.
    let structured = nuo_wire::ToolOutput::Subagent {
        summary: "Error: boom".into(),
        messages: Vec::new(),
        usage: nuo_wire::TokenUsage::default(),
        generation_ms: 0,
        failed: true,
        interrupted: false,
    };
    assert!(task.finish_tool_step("c", structured.to_text(), structured, 100));
    assert!(
        task.subagent_status_line().is_none(),
        "a terminal subagent hides the peek row"
    );
    // The outcome row surfaces the error summary's first line.
    assert_eq!(task.subagent_outcome_line().as_deref(), Some("Error: boom"));
}

#[test]
fn subagent_peek_reports_awaiting_approval_while_parked() {
    let mut task =
        TranscriptMessage::tool_step("c", "spawn_agent", r#"{"description":"d","prompt":"p"}"#);
    task.push_subagent_event(&SubagentEvent::ToolCall {
        id: "i".into(),
        name: "execute_command".into(),
        arguments: r#"{"command":"rm -rf x"}"#.into(),
        round: 1,
        turn: 0,
    });
    // The in-flight tool normally drives the peek row…
    let peek = task.subagent_status_line().unwrap();
    assert!(peek.starts_with("running Run rm"), "got: {peek}");

    // …but a parked permission request takes over the row: the subagent is
    // blocked on a human, not making progress.
    task.push_subagent_event(&SubagentEvent::PermissionRequest(
        nuo_wire::PermissionRequest {
            id: "p1".into(),
            tool: "execute_command".into(),
            label: "Run rm".into(),
            description: String::new(),
            arguments: "{}".into(),
            scope: "workspace".into(),
            elevation: false,
            one_off: false,
            origin: None,
            ..Default::default()
        },
    ));
    let peek = task.subagent_status_line().unwrap();
    assert!(peek.starts_with("awaiting approval"), "got: {peek}");

    // The next progress event from the subagent clears the parked wait.
    task.push_subagent_event(&SubagentEvent::ToolResult {
        id: "i".into(),
        name: "execute_command".into(),
        output: "done".into(),
        duration_ms: 3,
    });
    task.push_subagent_event(&SubagentEvent::StreamStart { round: 1, turn: 0 });
    task.push_subagent_event(&SubagentEvent::StreamDelta("…".into()));
    let peek = task.subagent_status_line().unwrap();
    assert!(peek.starts_with("running thinking"), "got: {peek}");
}

#[test]
fn interrupted_subagent_status_reports_interrupted_not_failed() {
    let mut task =
        TranscriptMessage::tool_step("c", "spawn_agent", r#"{"description":"d","prompt":"p"}"#);
    task.push_subagent_event(&SubagentEvent::ToolCall {
        id: "i".into(),
        name: "read_text".into(),
        arguments: "{}".into(),
        round: 1,
        turn: 0,
    });
    task.push_subagent_event(&SubagentEvent::ToolResult {
        id: "i".into(),
        name: "read_text".into(),
        output: "found 1 of 3 handlers".into(),
        duration_ms: 5,
    });
    // An interrupted subagent carries `interrupted: true, failed: false`:
    // the partial work was preserved, so it must classify as Interrupted
    // — never as Failed (it did not error) and never as Ok (it did not
    // finish).
    let structured = nuo_wire::ToolOutput::Subagent {
        summary: "Interrupted: stopped by the user".into(),
        messages: Vec::new(),
        usage: nuo_wire::TokenUsage::default(),
        generation_ms: 0,
        failed: false,
        interrupted: true,
    };
    assert!(task.finish_tool_step("c", structured.to_text(), structured, 100));
    assert_eq!(
        task.tool_step_status(),
        Some(ToolStepStatus::Interrupted),
        "an interrupted subagent classifies as Interrupted"
    );
    assert!(
        task.subagent_status_line().is_none(),
        "a terminal subagent hides the peek row"
    );
    assert_eq!(
        task.subagent_outcome_line().as_deref(),
        Some("Interrupted: stopped by the user"),
        "the outcome row carries the interruption summary"
    );
}

#[test]
fn bash_failure_is_classified_failed_from_structured_exit_code() {
    // Regression: a bash failure emits `Exit N …` which does NOT start with
    // "Error", so the legacy text sniff misclassified it as `Ok`. With
    // structured `ToolOutput::Shell { exit: Some(1) }`, `is_error()` now
    // drives the classification and the step correctly reads `Failed`.
    let mut step = TranscriptMessage::tool_step("c", "execute_command", r#"{"command":"false"}"#);
    let structured = nuo_wire::ToolOutput::Shell {
        command: "false".into(),
        stdout: String::new(),
        stderr: "boom".into(),
        lines: Vec::new(),
        exit: Some(1),
        truncated: false,
        termination: nuo_wire::tool_output::ShellTermination::Exited,
        detached_job_id: None,
    };
    let text = structured.to_text();
    assert!(
        !text.starts_with("Error"),
        "precondition: text is not Error-prefixed"
    );
    assert!(step.finish_tool_step("c", text, structured, 50));
    assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Failed));
}

#[test]
fn bash_success_is_classified_ok() {
    let mut step = TranscriptMessage::tool_step("c", "execute_command", r#"{"command":"true"}"#);
    let structured = nuo_wire::ToolOutput::Shell {
        command: "true".into(),
        stdout: "ok\n".into(),
        stderr: String::new(),
        lines: Vec::new(),
        exit: Some(0),
        truncated: false,
        termination: nuo_wire::tool_output::ShellTermination::Exited,
        detached_job_id: None,
    };
    let text = structured.to_text();
    assert!(step.finish_tool_step("c", text, structured, 5));
    assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Ok));
}

#[test]
fn push_tool_stream_builds_interleaved_lines_for_live_view() {
    // L5: the streaming seed must populate `lines` (with the right stream
    // tag each) so the live view renders arrival-ordered, stderr-tinted,
    // interleaved output — not the all-stdout-then-all-stderr degraded
    // band the empty-`lines` fallback forced.
    use nuo_wire::{ToolStream, tool_output::ShellStream};
    let mut step = TranscriptMessage::tool_step("c", "execute_command", r#"{"command":"x"}"#);
    assert!(step.push_tool_stream("c", &ToolStream::Stdout("Compiling a\n".into())));
    assert!(step.push_tool_stream("c", &ToolStream::Stderr("warning: b\n".into())));
    assert!(step.push_tool_stream("c", &ToolStream::Stdout("Compiling c\n".into())));

    let lines = match &step.kind {
        MessageKind::ToolStep {
            structured: Some(b),
            ..
        } => match b.as_ref() {
            nuo_wire::ToolOutput::Shell { lines, .. } => lines,
            _ => panic!("expected Shell"),
        },
        _ => panic!("expected ToolStep"),
    };
    assert_eq!(
        lines
            .iter()
            .map(|l| (l.stream, l.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (ShellStream::Out, "Compiling a"),
            (ShellStream::Err, "warning: b"),
            (ShellStream::Out, "Compiling c"),
        ],
        "streaming seed must preserve arrival order + stream tags"
    );
    // The flat strings stay populated too (model-facing path).
    match step.kind {
        MessageKind::ToolStep {
            structured: Some(b),
            ..
        } => match b.as_ref() {
            nuo_wire::ToolOutput::Shell { stdout, stderr, .. } => {
                assert!(stdout.contains("Compiling a"));
                assert!(stdout.contains("Compiling c"));
                assert!(stderr.contains("warning: b"));
            }
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

#[test]
fn cancel_tool_step_transitions_to_a_terminal_state() {
    let mut step = TranscriptMessage::tool_step("call_1", "websearch", r#"{"query":"rust"}"#);
    // Running -> Cancelled is a real terminal transition.
    assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Running));
    assert!(step.cancel_tool_step("call_1"));
    assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Cancelled));

    // The summary advertises the cancelled state instead of staying blank.
    let summary = step.tool_step_summary().expect("summary");
    assert!(summary.contains("cancelled"), "got: {summary}");
    // The raw (collapsed) transcript line mirrors the summary.
    assert!(step.raw.contains("cancelled"), "got: {}", step.raw);

    // Cancelled is terminal: a late result or another cancel is ignored.
    assert!(!step.finish_tool_step(
        "call_1",
        "late result",
        nuo_wire::ToolOutput::text("late result"),
        10
    ));
    assert!(!step.cancel_tool_step("call_1"));
    assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Cancelled));
}

#[test]
fn cancel_only_acts_on_the_matching_call_id() {
    let mut step = TranscriptMessage::tool_step("call_1", "websearch", "{}");
    // A different id does nothing and leaves the step running.
    assert!(!step.cancel_tool_step("call_9"));
    assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Running));
}

#[test]
fn cancelling_a_subagent_also_cancels_its_running_children() {
    let mut task = TranscriptMessage::tool_step(
        "task_1",
        "spawn_agent",
        r#"{"description":"d","prompt":"p"}"#,
    );
    // A nested tool call still in flight.
    task.push_subagent_event(&SubagentEvent::ToolCall {
        id: "inner".into(),
        name: "search_text".into(),
        arguments: r#"{"query":"foo"}"#.into(),
        round: 1,
        turn: 0,
    });
    let children = task.subagent_children().expect("has children");
    assert_eq!(
        children[0].tool_step_status(),
        Some(ToolStepStatus::Running)
    );

    // Interrupting the parent task cancels it AND the nested running child,
    // so the subagent view never shows a stuck "running" step.
    assert!(task.cancel_tool_step("task_1"));
    assert_eq!(task.tool_step_status(), Some(ToolStepStatus::Cancelled));
    let children = task.subagent_children().expect("has children");
    assert_eq!(
        children[0].tool_step_status(),
        Some(ToolStepStatus::Cancelled),
        "nested child must converge with the parent"
    );

    // A cancelled subagent is terminal: the peek row disappears and the
    // outcome row falls back to the legacy output text (none was recorded
    // here, so the row hides entirely).
    assert!(task.subagent_status_line().is_none());
    assert!(task.subagent_outcome_line().is_none());
}

#[test]
fn cancel_all_running_is_a_defensive_sweep_that_skips_terminal_steps() {
    let mut a = TranscriptMessage::tool_step("a", "read_text", "{}");
    let mut b = TranscriptMessage::tool_step("b", "read_text", "{}");
    // `b` already finished successfully; the sweep must not clobber it.
    assert!(b.finish_tool_step(
        "b",
        "contents",
        nuo_wire::ToolOutput::text("contents"),
        5
    ));
    assert_eq!(b.tool_step_status(), Some(ToolStepStatus::Ok));

    // The sweep cancels a running step and is then a no-op on it.
    assert!(a.cancel_all_running());
    assert!(!a.cancel_all_running());
    assert_eq!(a.tool_step_status(), Some(ToolStepStatus::Cancelled));
    // A finished step is untouched by the sweep.
    assert!(!b.cancel_all_running());
    assert_eq!(b.tool_step_status(), Some(ToolStepStatus::Ok));
}

#[test]
fn notice_carries_severity_and_is_classified_as_notice() {
    let n = TranscriptMessage::notice(NoticeSeverity::Error, "boom");
    assert!(n.is_notice());
    assert!(matches!(
        n.kind,
        MessageKind::Notice {
            severity: NoticeSeverity::Error,
            ..
        }
    ));
    // The raw text is preserved verbatim for the renderer (no "Error: "
    // prefix injection — the glyph is the renderer's job).
    assert_eq!(n.raw, "boom");
    assert_eq!(n.notice_expanded(), Some(false));

    let mut mut_n = n.clone();
    mut_n.pin_notice_expanded(true);
    assert_eq!(mut_n.notice_expanded(), Some(true));

    // A text message is not a notice.
    let plain = TranscriptMessage::new(Role::Assistant, "hi");
    assert!(!plain.is_notice());
}

#[test]
fn notice_from_core_preserves_the_topic_and_detail_split() {
    // The two-part split agreed at the contract layer (topic vocabulary +
    // title/body detail) must survive the boundary into the transcript
    // model instead of being flattened to `raw` and re-parsed at render
    // time. `raw` still carries the flattened form for copy fidelity.
    let core = nuo_wire::AgentNotice::new(
        nuo_wire::NoticeKind::ProviderRetry,
        nuo_wire::NoticeSeverity::Error,
        "Retrying provider request (2/3)",
        nuo_wire::NoticeSource::Harness,
    )
    .with_body("Google HTTP 429 Too Many Requests: {\"error\":{\"code\":429}}");

    let msg = TranscriptMessage::notice_from_core(&core);

    let parts = msg.notice_parts().expect("core notices carry parts");
    assert_eq!(parts.topic.as_deref(), Some("provider"));
    assert_eq!(parts.title, "Retrying provider request (2/3)");
    assert_eq!(
        parts.detail.as_deref(),
        Some("Google HTTP 429 Too Many Requests: {\"error\":{\"code\":429}}")
    );
    // Flattened fallback stays byte-identical to `render_text()`.
    assert_eq!(msg.raw, core.render_text());

    // Local notices keep the parts-free legacy shape (renderer falls back to
    // its heuristic parse).
    let local = TranscriptMessage::notice(NoticeSeverity::Info, "compacted 12 messages");
    assert!(local.notice_parts().is_none());
}

#[test]
fn notice_topic_labels_cover_the_contract_vocabulary() {
    // Every wire `NoticeKind` maps to a predictable user-facing topic label;
    // the match must not grow stale as the contract evolves.
    for (kind, label) in [
        (nuo_wire::NoticeKind::ProviderRetry, "provider"),
        (nuo_wire::NoticeKind::NudgeInjected, "turn guard"),
        (nuo_wire::NoticeKind::ReviewAlert, "review"),
        (nuo_wire::NoticeKind::TrustChanged, "trust"),
        (nuo_wire::NoticeKind::CommandAck, "command"),
        (nuo_wire::NoticeKind::ImageInputWithheld, "images"),
    ] {
        assert_eq!(notice_topic_label(kind), label);
    }
}

#[test]
fn user_message_origin_defaults_to_chat_and_can_be_overridden() {
    // A plain user message is a genuine chat prompt by default.
    let chat = TranscriptMessage::new(Role::User, "fix the bug");
    assert_eq!(chat.origin, UserMessageOrigin::Chat);

    // Slash commands tag themselves so the Activity modal does not mistake
    // them for the driving prompt.
    let slash = TranscriptMessage::new(Role::User, "/review working-tree")
        .with_origin(UserMessageOrigin::Slash);
    assert_eq!(slash.origin, UserMessageOrigin::Slash);

    // with_origin is idempotent and does not depend on the text: a
    // genuine chat prompt that happens to start with '/' stays Slash only
    // when explicitly tagged, never inferred from text here.
    let explicit_chat =
        TranscriptMessage::new(Role::User, "/etc is a path").with_origin(UserMessageOrigin::Chat);
    assert_eq!(explicit_chat.origin, UserMessageOrigin::Chat);
}

#[test]
fn provider_retry_settles_on_interruption() {
    let retry_at = std::time::Instant::now() + std::time::Duration::from_secs(4);
    let mut msg =
        TranscriptMessage::provider_retry(2, 5, retry_at, "Anthropic HTTP 529: Overloaded");
    assert!(msg.is_provider_retry());
    let initial_rev = msg.rev;

    msg.settle_interrupted_provider_retry();

    assert!(!msg.is_provider_retry());
    assert!(msg.is_notice());
    assert!(msg.rev > initial_rev);

    let MessageKind::Notice {
        severity,
        ref parts,
        ..
    } = msg.kind
    else {
        panic!("expected Notice");
    };
    assert_eq!(severity, NoticeSeverity::Warning);
    let parts = parts.as_ref().expect("expected parts");
    assert_eq!(parts.topic.as_deref(), Some("retry"));
    assert_eq!(parts.title, "Provider request failed (attempt 2/5)");
    assert_eq!(
        parts.detail.as_deref(),
        Some("Anthropic HTTP 529: Overloaded")
    );
    assert_eq!(
        parts.origin,
        Some(crate::model::document::NoticeOrigin::Provider {
            provider_name: None,
            attempt: Some((2, 5)),
        })
    );
    assert_eq!(
        msg.raw,
        "Provider request failed (attempt 2/5): Anthropic HTTP 529: Overloaded"
    );
}

#[test]
fn round_interrupt_creates_structured_notice() {
    use nuo_wire::{RoundInterrupt, RoundInterruptReason};
    let marker = TranscriptMessage::round_interrupted(RoundInterrupt {
        reason: RoundInterruptReason::User,
        round: Some(3),
        at_ms: 1_000,
        detail: Some("draft text that must not be repeated".to_string()),
    });
    assert!(marker.is_notice());
    assert!(marker.is_round_interrupt());
    assert_eq!(marker.raw, "Round 3 — cancelled via [Esc Esc]");
    let MessageKind::Notice { ref parts, .. } = marker.kind else {
        panic!("must be notice");
    };
    let parts = parts.as_ref().expect("must have parts");
    assert_eq!(parts.topic.as_deref(), Some("interrupted"));
    assert_eq!(parts.detail, None, "interrupted draft must not duplicate into notice detail");
    assert_eq!(
        parts.origin,
        Some(crate::model::document::NoticeOrigin::System {
            topic: crate::model::document::SystemNoticeTopic::Interrupted
        })
    );

    // Terminal round error with 429 detail
    let err_marker = TranscriptMessage::round_interrupted(RoundInterrupt {
        reason: RoundInterruptReason::Error,
        round: Some(4),
        at_ms: 2_000,
        detail: Some("Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests: {\n  \"error\": {\n    \"code\": 429\n  }\n}".to_string()),
    });
    assert!(err_marker.is_notice());
    assert!(err_marker.is_round_interrupt());
    let MessageKind::Notice {
        severity,
        ref parts,
        ..
    } = err_marker.kind
    else {
        panic!("must be notice");
    };
    assert_eq!(severity, NoticeSeverity::Error);
    let parts = parts.as_ref().expect("must have parts");
    assert_eq!(parts.topic.as_deref(), Some("error"));
    assert_eq!(
        parts.title,
        "Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests"
    );
    assert!(parts.detail.is_some());
}

#[test]
fn command_result_populates_command_invocation() {
    let harness_cmd = TranscriptMessage::pending_command("review", "HEAD~1");
    assert_eq!(harness_cmd.command_name(), Some("review"));
    assert_eq!(harness_cmd.command_args(), Some("HEAD~1"));
    assert_eq!(harness_cmd.raw, "/review HEAD~1");

    let no_args_cmd = TranscriptMessage::pending_command("compact", "");
    assert_eq!(no_args_cmd.command_name(), Some("compact"));
    assert_eq!(no_args_cmd.command_args(), Some(""));
    assert_eq!(no_args_cmd.raw, "/compact");
}

#[test]
fn notice_strips_terminal_controls_from_crlf_http_errors() {
    let n = TranscriptMessage::notice(
        NoticeSeverity::Error,
        "OpenAI HTTP 504: <html>\r\n<head>timeout</head>\x1b[2J\r\n</html>",
    );

    assert_eq!(
        n.raw,
        "OpenAI HTTP 504: <html>\n<head>timeout</head>[2J\n</html>"
    );
    assert!(!n.raw.chars().any(|c| c.is_control() && c != '\n'));
}

/// Streaming reasoning rows show activity, never a token count (ADR-0191);
/// finished traces settle to the milestone/duration line.
#[test]
fn reasoning_summary_shows_activity_then_settles() {
    // Streaming without milestones: an activity word, not a metric.
    let streaming = TranscriptMessage::reasoning("one two three four five");
    let summary = streaming.reasoning_summary().unwrap();
    assert_eq!(
        summary, "Thinking…",
        "no token count while streaming: {summary}"
    );

    // Even a long trace shows no count.
    let filler = "lorem ipsum ".repeat(600);
    let deep = TranscriptMessage::reasoning(&filler);
    let summary = deep.reasoning_summary().unwrap();
    assert!(!summary.contains("tokens"), "no token display: {summary}");

    // Finished trace: milestone/duration line, still no count.
    let mut done = TranscriptMessage::reasoning(filler);
    done.set_reasoning_duration(2_400);
    let settled = done.reasoning_summary().unwrap();
    assert_eq!(settled, "Thought (2.4s)", "settled line: {settled}");
}

#[test]
fn reasoning_summary_handles_structured_milestones() {
    use super::{count_milestones, extract_active_milestone};

    // Live streaming with a single milestone heading: normalizes with "Thinking through"
    let streaming_single = TranscriptMessage::reasoning("**Planning architectural changes**\n\n");
    assert_eq!(
        streaming_single.reasoning_summary().as_deref(),
        Some("Thinking through the architectural changes")
    );

    // Live streaming updating to subsequent milestone heading
    let streaming_multi = TranscriptMessage::reasoning(
        "**Planning architectural changes**\n\nAnalyzed codebase.\n\n**Executing database migration**\n\n",
    );
    assert_eq!(
        streaming_multi.reasoning_summary().as_deref(),
        Some("Thinking through the database migration")
    );

    // Flagship case: "Deconstructing Security Architecture Components"
    let streaming_flagship = TranscriptMessage::reasoning(
        "**Deconstructing Security Architecture Components**\n\nAnalyzing system boundaries...",
    );
    assert_eq!(
        streaming_flagship.reasoning_summary().as_deref(),
        Some("Thinking through the security architecture components")
    );

    // Helper functions verification
    assert_eq!(
        extract_active_milestone("### Validating test suite"),
        Some("Validating test suite".to_string())
    );
    assert_eq!(
        count_milestones(
            "**Step 1: Planning**\n\nDetails\n\n**Step 2: Execution**\n\nDetails\n\n**Step 3: Verification**\n\n"
        ),
        3
    );

    // Finished trace with multiple milestones
    let mut done_multi = TranscriptMessage::reasoning(
        "**Step 1: Planning**\n\nDetails\n\n**Step 2: Execution**\n\nDetails\n\n**Step 3: Verification**\n\n",
    );
    done_multi.set_reasoning_duration(4_500);
    assert_eq!(
        done_multi.reasoning_summary().as_deref(),
        Some("Thought through 3 steps (4.5s)")
    );

    // Finished trace with single milestone
    let mut done_single =
        TranscriptMessage::reasoning("**Planning architectural changes**\n\nDetails\n\n");
    done_single.set_reasoning_duration(1_200);
    assert_eq!(
        done_single.reasoning_summary().as_deref(),
        Some("Thought through the architectural changes (1.2s)")
    );

    // Flagship case finished (even with 0ms duration)
    let mut done_flagship = TranscriptMessage::reasoning(
        "**Deconstructing Security Architecture Components**\n\nAnalyzed boundaries.",
    );
    done_flagship.set_reasoning_duration(0);
    assert_eq!(
        done_flagship.reasoning_summary().as_deref(),
        Some("Thought through the security architecture components (0ms)")
    );
}

#[test]
fn normalize_reasoning_topic_edge_cases() {
    use super::normalize_thinking_topic;

    assert_eq!(
        normalize_thinking_topic("Deconstructing Security Architecture Components"),
        "the security architecture components"
    );
    assert_eq!(
        normalize_thinking_topic("Planning architectural changes"),
        "the architectural changes"
    );
    assert_eq!(
        normalize_thinking_topic("Executing database migration"),
        "the database migration"
    );
    assert_eq!(
        normalize_thinking_topic("Validating test suite"),
        "the test suite"
    );
    assert_eq!(
        normalize_thinking_topic("Analyzing OAuth 2.0 PKCE flow"),
        "the OAuth 2.0 PKCE flow"
    );
    assert_eq!(
        normalize_thinking_topic("Reviewing SQL query performance"),
        "the SQL query performance"
    );
    assert_eq!(
        normalize_thinking_topic("How to handle concurrency"),
        "how to handle concurrency"
    );
    assert_eq!(
        normalize_thinking_topic("Why cache invalidation fails"),
        "why cache invalidation fails"
    );
    assert_eq!(
        normalize_thinking_topic("The authentication pipeline"),
        "the authentication pipeline"
    );
    // Regression: multi-byte characters must not cause a char-boundary panic
    // when a redundant prefix's byte length lands inside a multi-byte char.
    assert_eq!(
        normalize_thinking_topic("Checking — UTF-8 boundary safety"),
        "the — UTF-8 boundary safety"
    );
    assert_eq!(
        normalize_thinking_topic("验证 UTF-8 边界安全"),
        "the 验证 UTF-8 边界安全"
    );
}

// ---------------------------------------------------------------------------
// Incremental streaming equivalence (ADR-0184)
// ---------------------------------------------------------------------------

/// Deterministic LCG so the corpus/delta splits are stable across runs.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
}

/// Streaming a document through `push_stream` must produce — at *every*
/// intermediate step, not just the end — exactly the blocks a full
/// `parse_blocks(raw)` would produce. This is the correctness contract the
/// frozen-prefix incremental parser hangs its O(delta) cost on.
#[test]
fn push_stream_matches_full_parse_at_every_step() {
    // Corpus exercising every construct the parser knows: fences (open and
    // closed), math, tables, lists, quotes, rules, headings, hard breaks,
    // inline markup, blank-line rhythm, CJK text, trailing whitespace.
    let corpus = "\
# Report

Intro paragraph with `inline code` and **bold** and a https://example.com link.

```rust
fn main() {
    println!(\"hi\");
}
```

$$
\\int_0^1 x^2 dx
$$

| A | B |
|---|---|
| 1 | 2 |
| 中文 | ok |

- first
- second
  - nested

> quoted line
> another quote

---

Step 1: prepare the environment.
Step 2: run the suite with trailing spaces  \n\n1. ordered one\n2. ordered two\n\n```\nunclosed fence content\nand more\n\nTail paragraph with CJK 中文与英文混排。\n";
    let bytes = corpus.as_bytes();

    let mut rng = Lcg(0xDEADBEEF);
    for trial in 0..16 {
        let mut msg = TranscriptMessage::new(Role::Assistant, "");
        let mut pos = 0usize;
        while pos < bytes.len() {
            // Random delta size in 1..=48 bytes, snapped to a char boundary
            // (provider deltas always arrive whole-scalar).
            let mut take = (rng.next() as usize % 48) + 1;
            if pos + take > bytes.len() {
                take = bytes.len() - pos;
            }
            while pos + take < bytes.len() && !corpus.is_char_boundary(pos + take) {
                take += 1;
            }
            let delta = &corpus[pos..pos + take];
            msg.push_stream(delta);
            pos += take;
            let full = parse_blocks(&msg.raw);
            assert_eq!(
                msg.blocks, full,
                "trial {trial}: incremental blocks diverged at byte {pos}"
            );
        }
    }
}

/// The full parse of the final raw must equal a from-scratch message.
#[test]
fn streamed_message_equals_freshly_parsed_message() {
    let corpus = "# Title\n\ntext `code` **bold**\n\n- a\n- b\n\n```\nbody\n```\n\ntail";
    let mut streamed = TranscriptMessage::new(Role::Assistant, "");
    for chunk in corpus.as_bytes().chunks(7) {
        // Snap to char boundary.
        let mut end = chunk.len();
        while !corpus.is_char_boundary(corpus.len().min(end + streamed.raw.len())) {
            end -= 1;
        }
        let start = streamed.raw.len();
        let end = start + end;
        if end > corpus.len() {
            break;
        }
        let delta = &corpus[start..end];
        streamed.push_stream(delta);
    }
    let fresh = TranscriptMessage::new(Role::Assistant, corpus);
    assert_eq!(streamed.raw, fresh.raw);
    assert_eq!(streamed.blocks, fresh.blocks);
}

/// Perf guard for ADR-0184: per-frame cost must be O(live construct), not
/// O(whole message). A representative long stream — bounded paragraphs,
/// regular structure — pushed in small deltas must complete in bounded time.
/// The pre-ADR full-reparse behavior re-parsed the entire accumulated message
/// every push (~O(n²) total) and blows past this budget by an order of
/// magnitude. The bound is deliberately generous (slow CI) so the test never
/// flakes on healthy incremental code while still catching a regression to
/// whole-message re-parsing.
#[test]
fn push_stream_stays_bounded_on_long_streams() {
    let mut msg = TranscriptMessage::new(Role::Assistant, "");
    let delta = "word ".repeat(20); // 100 bytes per push
    let started = std::time::Instant::now();
    for i in 0..6000 {
        let mut chunk = delta.clone();
        if i % 1000 == 0 {
            chunk = format!("\n\n## Section {i}\n\n");
        } else if i % 100 == 0 {
            // Paragraph boundary every ~10 KB, like normal prose.
            chunk = format!("\n\nContinuing with fresh material {i}. ");
        }
        msg.push_stream(&chunk);
    }
    let elapsed = started.elapsed();
    // 600 KB streamed in 6000 deltas: incremental cost is ~1s (~5s under llvm-cov); the
    // full-reparse regression measures ~9s here (>50s under llvm-cov).
    assert!(
        elapsed.as_secs() < 12,
        "push_stream regressed to super-linear cost: {elapsed:?} for 600 KB"
    );
    assert!(
        msg.blocks.len() > 100,
        "structure must have been discovered"
    );
}

/// The wrap cache must be transparent: cached geometry is bit-for-bit what a
/// fresh `wrap_text` / code preparation produces, across widths and content
/// shapes (including CJK and empty input).
#[test]
fn wrap_cache_is_transparent() {
    use crate::render::BlockWrapCache;
    let samples = [
        "",
        "short",
        "A much longer paragraph that will definitely wrap across several lines \
         when the width is small enough, with 中文混排 and `code spans` too.",
        "line one\nline two\n\nline four",
    ];
    let mut cache = BlockWrapCache::default();
    for text in samples {
        for width in [1usize, 8, 40, 200] {
            let cached = cache.wrap_text(text, width);
            let fresh = crate::text_layout::wrap_text(text, width);
            assert_eq!(cached.len(), fresh.len());
            for (c, f) in cached.iter().zip(fresh.iter()) {
                assert_eq!(
                    (c.text.as_str(), c.start_byte, c.end_byte),
                    (f.text.as_str(), f.start_byte, f.end_byte)
                );
            }
        }
    }
    // Code preparation: same logical split, same per-line wrap.
    let code = "fn main() {\n    let s = \"中文字符串很长的确会换行\";\n}\n";
    for width in [20usize, 60] {
        let prepared = cache.prepare_code(code, width);
        let mut fresh: Vec<(usize, Vec<_>)> = Vec::new();
        let mut offset = 0usize;
        for line in code.split('\n') {
            let mut wrapped = crate::text_layout::wrap_text(line, width);
            if wrapped.is_empty() {
                wrapped.push(crate::text_layout::WrappedLine {
                    text: String::new(),
                    start_byte: 0,
                    end_byte: 0,
                });
            }
            fresh.push((offset, wrapped));
            offset += line.len() + 1;
        }
        assert_eq!(prepared.logical.len(), fresh.len());
        for ((po, pw), (fo, fw)) in prepared.logical.iter().zip(fresh.iter()) {
            assert_eq!(po, fo);
            assert_eq!(pw.len(), fw.len());
            for (c, f) in pw.iter().zip(fw.iter()) {
                assert_eq!(
                    (c.text.as_str(), c.start_byte, c.end_byte),
                    (f.text.as_str(), f.start_byte, f.end_byte)
                );
            }
        }
    }
}
