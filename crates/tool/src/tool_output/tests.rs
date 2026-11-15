#[test]
fn patch_warnings_preserve_wire_compatibility_and_text() {
    let legacy = serde_json::json!({"Patch": {
        "path": "a.rs", "op": "Edit", "old": "before", "new": "after", "start_line": 7
    }});
    let mut patch: super::ToolOutput = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(serde_json::to_value(&patch).unwrap(), legacy);
    let clean_text = patch.to_text();
    if let super::ToolOutput::Patch { warnings, .. } = &mut patch {
        warnings.push("advisory syntax diagnostic".into());
    }
    let restored: super::ToolOutput =
        serde_json::from_value(serde_json::to_value(&patch).unwrap()).unwrap();
    assert!(restored.to_text().starts_with(&clean_text));
    assert!(
        restored
            .to_text()
            .contains("Warning: advisory syntax diagnostic")
    );
    assert!(
        matches!(restored, super::ToolOutput::Patch { old, new, start_line: 7, warnings, .. }
        if old == "before" && new == "after" && warnings.len() == 1)
    );
}

use super::*;

#[test]
fn strip_ansi_removes_sgr_cursor_osc() {
    use super::strip_ansi;
    assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
    assert_eq!(strip_ansi("a\x1b[2Kbc"), "abc");
    assert_eq!(strip_ansi("\x1b]0;title\x07clean"), "clean");
    assert_eq!(strip_ansi("\x1b[1;31mhi\r"), "hi\r");
    assert_eq!(strip_ansi("no escapes here"), "no escapes here");
}

#[test]
fn carriage_return_overwrites_prefix_in_place() {
    // Progress-bar shape: `downloading… 50%` then `\r` then the final
    // frame. The caret returns to column 0, so the longer final segment
    // overwrites the prefix cell-by-cell. CI-log normalization.
    use super::normalize_carriage_returns;
    assert_eq!(
        normalize_carriage_returns("downloading… 50%\rdownloading… 100%"),
        "downloading… 100%"
    );
}

#[test]
fn carriage_return_shorter_final_keeps_prefix_tail() {
    // `foo\rbar`: `bar` overwrites only the first 3 columns, so the
    // surviving tail of `foo` (none here) is replaced — result `bar`.
    // With `longer\rx`: `x` overwrites col 0, the rest of `longer`
    // survives as `xonger`.
    use super::normalize_carriage_returns;
    assert_eq!(normalize_carriage_returns("foo\rbar"), "bar");
    assert_eq!(normalize_carriage_returns("longer\rx"), "xonger");
}

#[test]
fn carriage_return_leading_only_padding() {
    // A `\r` with no preceding text pads up to the caret with spaces.
    use super::normalize_carriage_returns;
    assert_eq!(normalize_carriage_returns("\r  hi"), "  hi");
}

#[test]
fn backspace_steps_one_column() {
    use super::normalize_carriage_returns;
    // `ab\u{8}c`: backspace after `ab` steps to col 1, `c` overwrites `b`
    // → `ac`.
    assert_eq!(normalize_carriage_returns("ab\u{8}c"), "ac");
}

#[test]
fn stray_control_bytes_dropped() {
    // BEL / FF / VT have no single-line rendering; they're stripped so
    // they can't corrupt width math. (ANSI escapes were already removed
    // upstream by `strip_ansi`.)
    use super::normalize_carriage_returns;
    assert_eq!(normalize_carriage_returns("a\u{7}b\u{c}c"), "abc");
}

#[test]
fn carriage_return_passthrough_when_none_present() {
    use super::normalize_carriage_returns;
    assert_eq!(normalize_carriage_returns("plain text"), "plain text");
    // No copy: the fast path returns the input unchanged.
}

#[test]
fn tabs_expanded_to_spaces() {
    // `\t` is expanded to spaces at the next 8-column stop (raw tabs
    // report width 0 to unicode_width, which would desync the wrapper's
    // column math and scramble the band's right edge).
    use super::normalize_carriage_returns;
    assert_eq!(normalize_carriage_returns("a\tb"), "a       b");
    // A leading tab lands on stop 8.
    assert_eq!(normalize_carriage_returns("\tab"), "        ab");
    // Two tabs in a row still snap to stops.
    assert_eq!(normalize_carriage_returns("\t\tend"), "                end");
}

#[test]
fn text_round_trips() {
    assert_eq!(ToolOutput::text("hi").to_text(), "hi");
    let v = ToolOutput::from("x".to_string());
    assert!(matches!(v, ToolOutput::Text(s) if s == "x"));
}

#[test]
fn error_to_text_keeps_error_prefix() {
    // The current UI classifies failure by `starts_with("Error")`; the
    // bridge must preserve that until the UI migrates to `is_error()`.
    let e = ToolOutput::Error {
        message: "boom".into(),
        detail: None,
    };
    assert!(e.to_text().starts_with("Error"));
    assert!(e.is_error());
}

#[test]
fn error_with_detail_appends() {
    let e = ToolOutput::Error {
        message: "boom".into(),
        detail: Some("stack\ntrace".into()),
    };
    assert_eq!(e.to_text(), "Error: boom\nstack\ntrace");
}

#[test]
fn shell_killed_run_appends_harness_note() {
    // An exited run carries no note — the legacy text is exact.
    let exited = ToolOutput::Shell {
        command: "x".into(),
        stdout: "hi\n".into(),
        stderr: "".into(),
        lines: Vec::new(),
        exit: Some(0),
        truncated: false,
        truncated_note: None,
        termination: ShellTermination::Exited,
        detached_job_id: None,
    };
    assert_eq!(exited.to_text(), "hi\n");

    // A harness termination states the fact and the remedy; the model must be
    // able to distinguish it from a genuine process exit.
    for (term, needle) in [
        (
            ShellTermination::IdleBlocked,
            "[terminated by harness: idle timeout",
        ),
        (
            ShellTermination::Timeout,
            "[terminated by harness: timeout reached",
        ),
        (
            ShellTermination::Cancelled,
            "[terminated by harness: cancelled",
        ),
        (
            ShellTermination::StreamGuard,
            "[terminated by harness: output limit reached",
        ),
    ] {
        let o = ToolOutput::Shell {
            command: "x".into(),
            stdout: "partial\n".into(),
            stderr: "".into(),
            lines: Vec::new(),
            exit: None,
            truncated: false,
            truncated_note: None,
            termination: term,
            detached_job_id: None,
        };
        let text = o.to_text();
        assert!(
            text.contains(needle),
            "{term:?} note missing {needle:?}: {text}"
        );
        assert!(o.is_error(), "{term:?} terminated run is a failure");
    }
}

#[test]
fn shell_success_stdout_only_matches_legacy() {
    let o = ToolOutput::Shell {
        command: "echo hi".into(),
        stdout: "hi\n".into(),
        stderr: "".into(),
        lines: Vec::new(),
        exit: Some(0),
        truncated: false,
        truncated_note: None,
        termination: ShellTermination::Exited,
        detached_job_id: None,
    };
    assert_eq!(o.to_text(), "hi\n");
    assert!(!o.is_error());
}

#[test]
fn shell_success_stderr_only_emits_clean_stderr() {
    let o = ToolOutput::Shell {
        command: "x".into(),
        stdout: "".into(),
        stderr: "warn\n".into(),
        lines: Vec::new(),
        exit: Some(0),
        truncated: false,
        truncated_note: None,
        termination: ShellTermination::Exited,
        detached_job_id: None,
    };
    assert_eq!(o.to_text(), "warn\n");
}

#[test]
fn shell_failure_formats_body_with_exit_footer() {
    let o = ToolOutput::Shell {
        command: "false".into(),
        stdout: "out\n".into(),
        stderr: "err\n".into(),
        lines: Vec::new(),
        exit: Some(1),
        truncated: false,
        truncated_note: None,
        termination: ShellTermination::Exited,
        detached_job_id: None,
    };
    assert_eq!(o.to_text(), "out\nerr\nexit 1");
    assert!(o.is_error());
}

#[test]
fn shell_signal_does_not_fake_neg1() {
    let o = ToolOutput::Shell {
        command: "x".into(),
        stdout: "".into(),
        stderr: "killed\n".into(),
        lines: Vec::new(),
        exit: None,
        truncated: false,
        truncated_note: None,
        termination: ShellTermination::Exited,
        detached_job_id: None,
    };
    assert_eq!(o.to_text(), "killed\n");
    assert!(o.is_error());
}

#[test]
fn shell_truncated_wraps_with_markers() {
    let big = "a".repeat(9000);
    let o = ToolOutput::Shell {
        command: "x".into(),
        stdout: big,
        stderr: "".into(),
        lines: Vec::new(),
        exit: Some(0),
        truncated: true,
        truncated_note: None,
        termination: ShellTermination::Exited,
        detached_job_id: None,
    };
    let text = o.to_text();
    assert!(
        text.contains("omitted to protect context budget"),
        "got: {text:.80}"
    );
    // Single giant line without newlines -> reported as characters
    assert!(text.contains("characters ("), "got: {text:.80}");
}

#[test]
fn code_to_text_is_the_text() {
    let o = ToolOutput::Code {
        lang: Some("rs".into()),
        text: "fn main() {}".into(),
        start_line: 1,
        prefix: None,
        suffix: None,
    };
    assert_eq!(o.to_text(), "1: fn main() {}");
    assert!(!o.is_error());
}

#[test]
fn code_start_line_round_trips_and_defaults_to_zero() {
    // `start_line` drives per-line numbering in `to_text()` so the model
    // can reference exact file lines. It must survive cloning so an offset
    // snippet keeps its line base.
    let o = ToolOutput::Code {
        lang: None,
        text: "x".into(),
        start_line: 42,
        prefix: None,
        suffix: None,
    };
    assert_eq!(o.to_text(), "42: x");
    let cloned = o.clone();
    match cloned {
        ToolOutput::Code { start_line, .. } => assert_eq!(start_line, 42),
        _ => unreachable!(),
    }
}

#[test]
fn code_prefix_suffix_frame_the_content_for_the_model() {
    // The renderer draws `text`; the model sees framing composed around
    // line-numbered content. This split is what makes pagination loop-safe:
    // the model gets a concrete continuation without polluting the rendered
    // code block.
    let with_both = ToolOutput::Code {
        lang: None,
        text: "body".into(),
        start_line: 100,
        prefix: Some("[f: lines 100-100 of 5000]".into()),
        suffix: Some("[4900 more lines — read with offset=101]".into()),
    };
    assert_eq!(
        with_both.to_text(),
        "[f: lines 100-100 of 5000]\n100: body\n[4900 more lines — read with offset=101]"
    );

    let prefix_only = ToolOutput::Code {
        lang: None,
        text: "body".into(),
        start_line: 100,
        prefix: Some("[f: lines 100-105 of 105]".into()),
        suffix: None,
    };
    assert_eq!(
        prefix_only.to_text(),
        "[f: lines 100-105 of 105]\n100: body"
    );
}

#[test]
fn listing_to_text_frames_the_page() {
    let o = ToolOutput::Listing {
        entries: vec!["src/".into(), "Cargo.toml".into()],
        total: Some(2),
        offset: 1,
        next: None,
    };
    assert_eq!(o.to_text(), "[2 entries]\nsrc/\nCargo.toml");
}

#[test]
fn listing_to_text_frames_a_truncated_page() {
    let o = ToolOutput::Listing {
        entries: vec!["a".into(), "b".into()],
        total: Some(5),
        offset: 3,
        next: Some(5),
    };
    assert_eq!(
        o.to_text(),
        "[5 entries, showing 3-4]\na\nb\n[1 more entry — read with offset=5]"
    );
}

#[test]
fn restored_listing_without_coordinates_renders_bare_entries() {
    let o = ToolOutput::Listing {
        entries: vec!["old".into()],
        total: None,
        offset: 1,
        next: None,
    };
    assert_eq!(o.to_text(), "old");
}

#[test]
fn matches_to_text_joins_lines() {
    let o = ToolOutput::Matches {
        pattern: "foo".into(),
        lines: vec!["a.rs:1:foo".into(), "b.rs:3:foo".into()],
    };
    assert_eq!(o.to_text(), "a.rs:1:foo\nb.rs:3:foo");
}

#[test]
fn subagent_to_text_returns_summary_only() {
    // The parent model only sees the summary; the structured transcript
    // travels out-of-band. This is the contract that lets us persist the
    // subagent transcript without polluting the parent's context window.
    let usage = crate::usage::TokenUsage {
        prompt_tokens: 1000,
        completion_tokens: 200,
        total_tokens: 1200,
        ..Default::default()
    };
    let messages = vec![crate::message::Message::new(
        crate::Role::Assistant,
        "internal",
    )];
    let o = ToolOutput::Subagent {
        summary: "external summary".into(),
        messages,
        usage,
        generation_ms: 0,
        failed: false,
        interrupted: false,
    };
    assert_eq!(o.to_text(), "external summary");
    assert!(!o.is_error());
}

#[test]
fn subagent_payload_returns_messages_and_usage() {
    let usage = crate::usage::TokenUsage {
        prompt_tokens: 50,
        completion_tokens: 10,
        total_tokens: 60,
        ..Default::default()
    };
    let messages = vec![
        crate::message::Message::new(crate::Role::System, "sys"),
        crate::message::Message::new(crate::Role::Assistant, "answer"),
    ];
    let o = ToolOutput::Subagent {
        summary: "s".into(),
        messages: messages.clone(),
        usage,
        generation_ms: 0,
        failed: false,
        interrupted: false,
    };
    let (got_messages, got_usage) = o.subagent_payload().expect("subagent payload");
    assert_eq!(got_messages.len(), 2);
    assert_eq!(got_usage, usage);
}

#[test]
fn non_subagent_payload_returns_none() {
    let o = ToolOutput::text("plain");
    assert!(o.subagent_payload().is_none());
}

#[test]
fn subagent_failed_flag_drives_is_error_not_summary_text() {
    // Regression for the text-sniff removal: a subagent whose summary
    // starts with "Error" but carries `failed: false` must NOT classify
    // as an error, and vice versa.
    let with_flag = ToolOutput::Subagent {
        summary: "partial findings".into(),
        messages: Vec::new(),
        usage: crate::usage::TokenUsage::default(),
        generation_ms: 0,
        failed: true,
        interrupted: false,
    };
    assert!(with_flag.is_error());

    let no_flag = ToolOutput::Subagent {
        summary: "Error: legacy text".into(),
        messages: Vec::new(),
        usage: crate::usage::TokenUsage::default(),
        generation_ms: 0,
        failed: false,
        interrupted: false,
    };
    assert!(!no_flag.is_error());
}

#[test]
fn interrupted_flag_round_trips_and_defaults_to_false() {
    // An interrupted subagent survives serialization with its flag intact.
    let interrupted = ToolOutput::Subagent {
        summary: "Interrupted: stopped".into(),
        messages: Vec::new(),
        usage: crate::usage::TokenUsage::default(),
        generation_ms: 0,
        failed: false,
        interrupted: true,
    };
    let json = serde_json::to_string(&interrupted).unwrap();
    let back: ToolOutput = serde_json::from_str(&json).unwrap();
    assert!(back.subagent_interrupted());
    assert!(!back.is_error());

    // Sessions persisted before the field existed deserialize with
    // `interrupted: false` — never a hard load failure. Build the legacy
    // JSON by serializing and dropping the field, so the shape is exact
    // regardless of TokenUsage's field set.
    let with_flag = ToolOutput::Subagent {
        summary: "x".into(),
        messages: Vec::new(),
        usage: crate::usage::TokenUsage::default(),
        generation_ms: 0,
        failed: true,
        interrupted: false,
    };
    let mut legacy_json = serde_json::to_value(&with_flag).unwrap();
    legacy_json
        .as_object_mut()
        .unwrap()
        .get_mut("Subagent")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("interrupted");
    let legacy: ToolOutput = serde_json::from_value(legacy_json).unwrap();
    assert!(!legacy.subagent_interrupted());
    assert!(legacy.is_error());
}

#[test]
fn web_search_round_trips_and_formats_text() {
    let ws = ToolOutput::WebSearch {
        query: "rust async".into(),
        provider: "DuckDuckGo".into(),
        results: vec![WebSearchHit {
            title: "Async in Rust".into(),
            url: "https://rust-lang.org/async".into(),
            domain: "rust-lang.org".into(),
            snippet: "Async book and guide".into(),
        }],
        truncated: false,
    };
    assert!(!ws.is_error());
    let text = ws.to_text();
    assert!(text.contains("Search results for 'rust async' (via DuckDuckGo)"));
    assert!(text.contains("1. Async in Rust"));
    assert!(text.contains("https://rust-lang.org/async"));

    let json = serde_json::to_string(&ws).unwrap();
    let back: ToolOutput = serde_json::from_str(&json).unwrap();
    assert_eq!(back.to_text(), text);
}

#[test]
fn web_article_round_trips_and_formats_text() {
    let wa = ToolOutput::WebArticle {
        url: "https://example.com/post".into(),
        title: Some("Example Post".into()),
        domain: "example.com".into(),
        markdown: "# Example Post\n\nContent here".into(),
        reader: "Jina".into(),
        tokens: 120,
        truncated: false,
    };
    assert!(!wa.is_error());
    let text = wa.to_text();
    assert!(text.starts_with("[BEGIN UNTRUSTED WEB CONTENT"));
    assert!(text.contains("# Example Post"));
    assert!(text.ends_with("[END UNTRUSTED WEB CONTENT]"));

    let json = serde_json::to_string(&wa).unwrap();
    let back: ToolOutput = serde_json::from_str(&json).unwrap();
    assert_eq!(back.to_text(), text);
}
