//! Snapshot baselines and interaction tests for tool-step rendering.

use super::helpers::{
    render_grid, render_grid_with_map, tool_step, tool_step_streaming, tool_step_structured,
};
use nuo_view::model::selection::SelectionState;

#[test]
fn read_expanded_renders_code_block() {
    let m = tool_step(
        "read",
        r#"{"path":"src/lib.rs"}"#,
        Some("fn main() {\n    let x = 1;\n}\n"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn read_with_offset_numbers_from_start_line() {
    // A structured `Code` carrying `start_line: 100` (as `read` emits
    // when called with `offset: 100`) must number the gutter 100, 101, … —
    // not restart at 1. Also locks that the gutter column widens to fit
    // three-digit line numbers instead of overflowing.
    let m = tool_step_structured(
        "read",
        r#"{"path":"src/lib.rs","offset":100}"#,
        nuo_protocol::ToolOutput::Code {
            lang: Some("rs".into()),
            text: "fn a() {}\nfn b() {}\nfn c() {}\n".into(),
            start_line: 100,
            prefix: None,
            suffix: None,
        },
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn read_structured_code_hides_model_framing_and_numbers_from_offset() {
    // The structured `Code` carries model-facing `prefix`/`suffix` framing; the
    // renderer must draw only the pure `text` with a gutter based at
    // `start_line` — never the `[Lines …]` header or the continuation hint.
    let m = tool_step_structured(
        "read",
        r#"{"path":"src/lib.rs","offset":100,"limit":2}"#,
        nuo_protocol::ToolOutput::Code {
            lang: Some("rs".into()),
            text: "fn a() {}\nfn b() {}".into(),
            start_line: 100,
            prefix: Some("[Lines 100-101 of 500 from `src/lib.rs`]".into()),
            suffix: Some("[398 more lines — read with offset=102]".into()),
        },
        true,
    );
    let grid = render_grid(&m, 80, 40);
    assert!(grid.contains("100 fn a() {}"), "grid:\n{grid}");
    assert!(grid.contains("101 fn b() {}"), "grid:\n{grid}");
    assert!(!grid.contains("[Lines"), "model framing leaked:\n{grid}");
    assert!(
        !grid.contains("more lines"),
        "continuation hint leaked:\n{grid}"
    );
}

#[test]
fn read_unstructured_strips_lines_framing_and_embedded_numbers() {
    let raw = "\
[Lines 1-120 of 205 from `docs/adr/0024-two-row-head-band-session-identity-and-scene-row.md`]
   1 | ---
   2 | id: ADR-0024
   3 | title: \"Two-Row Head Band\"
   4 | status: accepted
   5 | date: 2026-10-06
   6 | scope: tui/nuo-tui
   7 | superseded_by: null
   8 | negative_knowledge: true
   9 | ---
  10 | ";
    let m = tool_step(
        "read",
        r#"{"path":"docs/adr/0024-two-row-head-band-session-identity-and-scene-row.md","offset":1,"limit":120}"#,
        Some(raw),
        true,
    );
    let grid = render_grid(&m, 80, 40);
    assert!(!grid.contains("[Lines"), "lines framing leaked:\n{grid}");
    assert!(
        !grid.contains("1 |"),
        "redundant line number column leaked:\n{grid}"
    );
    assert!(
        !grid.contains("2 |"),
        "redundant line number column leaked:\n{grid}"
    );
    assert!(
        grid.contains("1 ---"),
        "first line not rendered with gutter 1:\n{grid}"
    );
    assert!(
        grid.contains("2 id: ADR-0024"),
        "second line not rendered with gutter 2:\n{grid}"
    );
    assert!(
        grid.contains("9 ---"),
        "ninth line not rendered with gutter 9:\n{grid}"
    );
}

#[test]
fn read_unstructured_with_offset_and_continuation_hint() {
    let raw = "\
[Lines 100-102 of 500 from `src/lib.rs`]
 100 | fn a() {}
 101 | fn b() {}
 102 | fn c() {}
[398 more lines — read with offset=103]";
    let m = tool_step(
        "read",
        r#"{"path":"src/lib.rs","offset":100,"limit":3}"#,
        Some(raw),
        true,
    );
    let grid = render_grid(&m, 80, 40);
    assert!(!grid.contains("[Lines"), "lines framing leaked:\n{grid}");
    assert!(
        !grid.contains("more lines"),
        "continuation hint leaked:\n{grid}"
    );
    assert!(
        !grid.contains("100 |"),
        "redundant line column leaked:\n{grid}"
    );
    assert!(grid.contains("100 fn a() {}"), "line 100 missing:\n{grid}");
    assert!(grid.contains("101 fn b() {}"), "line 101 missing:\n{grid}");
    assert!(grid.contains("102 fn c() {}"), "line 102 missing:\n{grid}");
}

#[test]
fn execute_command_expanded_renders_markers_and_output() {
    let m = tool_step(
        "execute_command",
        r#"{"command":"cargo test"}"#,
        Some("running 3 tests\n.\n.\n.\ntest result: ok. 3 passed\nSTDOUT:\nbuilt ok\nExit 0\n"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn execute_command_expanded_renders_structured_shell() {
    // Structured Shell: stdout and stderr render as separate color bands and a
    // non-zero exit produces an `exit N` footer (the failure is also reflected
    // in the header status glyph). This is the command path in production
    // post-ADR-0001; the marker-sniffing test above covers the legacy fallback.
    let m = tool_step_structured(
        "execute_command",
        r#"{"command":"cargo test"}"#,
        nuo_protocol::ToolOutput::Shell {
            command: "cargo test".into(),
            stdout: "running 3 tests\n...\ntest result: ok. 3 passed".into(),
            stderr: "warning: unused import".into(),
            lines: Vec::new(),
            exit: Some(1),
            truncated: false,
            truncated_note: None,
            termination: nuo_protocol::tool_output::ShellTermination::Exited,
            detached_job_id: None,
        },
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn execute_command_expanded_preserves_stdout_stderr_interleaving() {
    // Regression for the "all-stdout-then-all-stderr" reorder symptom: when
    // `lines` is populated, the renderer must emit them in arrival order, not
    // bucketed by stream. Here a stderr line sits *between* two stdout lines,
    // so a correct render shows `warning: b` between `Compiling a` and
    // `Compiling c` — not pinned to the bottom.
    use nuo_protocol::tool_output::{ShellLine, ShellStream};
    let m = tool_step_structured(
        "execute_command",
        r#"{"command":"cargo build"}"#,
        nuo_protocol::ToolOutput::Shell {
            command: "cargo build".into(),
            stdout: "Compiling a\nCompiling c".into(),
            stderr: "warning: b".into(),
            lines: vec![
                ShellLine {
                    stream: ShellStream::Out,
                    text: "Compiling a".into(),
                },
                ShellLine {
                    stream: ShellStream::Err,
                    text: "warning: b".into(),
                },
                ShellLine {
                    stream: ShellStream::Out,
                    text: "Compiling c".into(),
                },
            ],
            exit: Some(0),
            truncated: false,
            truncated_note: None,
            termination: nuo_protocol::tool_output::ShellTermination::Exited,
            detached_job_id: None,
        },
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn execute_command_expanded_folds_long_output_keeping_tail_events() {
    // A long stdout (well past HEAD + TAIL + 1 = 7 lines) folds its middle
    // into a single `⋯ N lines hidden` row, while the head, the tail, and the
    // trailing `exit 1` event footer all stay visible. This is the fix for the
    // "verbose stdout buries the exit code" symptom on an expanded command step.
    let stdout = (1..=20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let m = tool_step_structured(
        "execute_command",
        r#"{"command":"cargo build"}"#,
        nuo_protocol::ToolOutput::Shell {
            command: "cargo build".into(),
            stdout,
            stderr: String::new(),
            lines: Vec::new(),
            exit: Some(1),
            truncated: false,
            truncated_note: None,
            termination: nuo_protocol::tool_output::ShellTermination::Exited,
            detached_job_id: None,
        },
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

/// The `$` line is the most obviously copyable row in the whole transcript, and
/// it used to be the one row that could not be selected: it was painted with
/// `ctx.paint`, so it registered no region at all.
///
/// A drag that starts on the rail, runs across the prompt and into the output
/// must yield the command and the output — one ordinary copy — with the rail and
/// the `$` left out of the payload, while the painted row still *shows* them.
#[test]
fn dragging_from_the_dollar_prompt_copies_the_command_and_its_output() {
    use nuo_view::model::layout::SemanticCursor;

    let m = tool_step_structured(
        "execute_command",
        r#"{"command":"cargo build"}"#,
        nuo_protocol::ToolOutput::Shell {
            command: "cargo build".into(),
            stdout: "Compiling a\nCompiling b".into(),
            stderr: String::new(),
            lines: Vec::new(),
            exit: Some(0),
            truncated: false,
            truncated_note: None,
            termination: nuo_protocol::tool_output::ShellTermination::Exited,
            detached_job_id: None,
        },
        true,
    );
    let (grid, map) = render_grid_with_map(&m, 80, 20);

    // The painted block still states the invocation. It is flush-left: there is
    // no rail, so the `$` is the body's only per-row mark (ADR-0150).
    assert!(
        grid.contains("$ cargo build"),
        "the prompt line must still be painted:\n{grid}"
    );
    assert!(
        !grid.contains('│'),
        "a command body carries no left track — it would be copied out with the \
         output by a shift-drag:\n{grid}"
    );

    // Locate the rows from the map rather than assuming the step's top: the
    // summary row and the body's top gap sit above them, and a hard-coded row
    // index would silently drift the day either gap changes.
    let prompt_row = map
        .regions()
        .iter()
        .find(|r| r.text.contains("cargo build"))
        .expect("the prompt row is addressable")
        .rect
        .y;
    let last_output_row = map
        .regions()
        .iter()
        .filter(|r| !r.text.is_empty())
        .map(|r| r.rect.y)
        .max()
        .expect("the body rows are addressable");

    // A drag from the very left margin of the invocation row — before the block,
    // on the `$` — down through the last output row.
    let anchor = map
        .cursor_at(0, prompt_row)
        .expect("the prompt row resolves a cursor");
    let head = map
        .cursor_at(40, last_output_row)
        .expect("the output row resolves a cursor");
    let sel = SelectionState::Range { anchor, head };
    let copied = crate::shell::event_loop::transcript::extract_selection_text(
        &sel,
        std::slice::from_ref(&m),
        "",
        &map,
        None,
    )
    .expect("a drag over the prompt and output must copy");

    assert_eq!(
        copied, "cargo build\nCompiling a\nCompiling b",
        "the `$` is display chrome, never clipboard content"
    );
    let _ = SemanticCursor::new(0, 0, 0);
}

/// The fold block offers two verbs, so it needs two hit targets: a click on
/// `copy hidden` must not open the log, and a click on `view full log` must not
/// copy. Both live inside one sentence, so the targets are clause-wide rather
/// than key-wide — a user who clicks the words `view full log` means that verb.
#[test]
fn the_fold_block_registers_one_hit_target_per_verb() {
    use nuo_view::model::layout::{TOOL_FOLD_BLOCK_IDX, TOOL_FOLD_COPY_BLOCK_IDX};

    let stdout = (1..=20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let m = tool_step_structured(
        "execute_command",
        r#"{"command":"cargo build"}"#,
        nuo_protocol::ToolOutput::Shell {
            command: "cargo build".into(),
            stdout,
            stderr: String::new(),
            lines: Vec::new(),
            exit: Some(0),
            truncated: false,
            truncated_note: None,
            termination: nuo_protocol::tool_output::ShellTermination::Exited,
            detached_job_id: None,
        },
        true,
    );
    let (grid, map) = render_grid_with_map(&m, 80, 20);

    let view = map
        .regions()
        .iter()
        .find(|r| r.block_idx == TOOL_FOLD_BLOCK_IDX)
        .expect("the view verb owns a region");
    let copy = map
        .regions()
        .iter()
        .find(|r| r.block_idx == TOOL_FOLD_COPY_BLOCK_IDX)
        .expect("the copy verb owns a region");

    // The two targets do not overlap: one click, one verb.
    assert!(
        view.rect.x + view.rect.width <= copy.rect.x
            || copy.rect.x + copy.rect.width <= view.rect.x,
        "the verbs must not share columns: {view:?} / {copy:?}"
    );
    // Both clauses live on the same painted row, under the block's own gutter.
    assert_eq!(
        view.rect.y, copy.rect.y,
        "the two verbs share the sentence row"
    );
    // And each really sits on the clause it advertises.
    assert_eq!(
        crate::shell::interaction::classify_click(&map, view.rect.x + 1, view.rect.y),
        crate::shell::interaction::ClickTarget::FoldViewLog { message_idx: 0 }
    );
    assert_eq!(
        crate::shell::interaction::classify_click(&map, copy.rect.x + 1, copy.rect.y),
        crate::shell::interaction::ClickTarget::FoldCopyRows { message_idx: 0 }
    );
    assert!(
        grid.contains("view full log"),
        "the block names both verbs in a sentence: {grid}"
    );
    assert!(
        grid.contains("copy hidden"),
        "the block says what `y` copies: {grid}"
    );
}

/// The fold is a **block**: blank row, two annotated rows, blank row, inset two
/// columns from the body it interrupts, and marked with a bounded gutter rather
/// than a track.
///
/// The geometry is asserted rather than eyeballed because every one of those
/// facts is a copy guarantee: the blank rows are what keep a shift-drag over the
/// head or the tail from catching the fold, and the gutter is what keeps the
/// mark out of columns any output row occupies.
#[test]
fn the_fold_is_an_inset_block_with_its_own_bounded_gutter() {
    use nuo_ui_primitives::design::{
        BASH_FOLD_CHROME_ROWS, BASH_FOLD_INSET_COLS, bash_fold_gutter_glyph,
    };

    let stdout = (1..=20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let m = tool_step_structured(
        "execute_command",
        r#"{"command":"cargo build"}"#,
        nuo_protocol::ToolOutput::Shell {
            command: "cargo build".into(),
            stdout,
            stderr: String::new(),
            lines: Vec::new(),
            exit: Some(1),
            truncated: false,
            truncated_note: None,
            termination: nuo_protocol::tool_output::ShellTermination::Exited,
            detached_job_id: None,
        },
        true,
    );
    let (grid, map) = render_grid_with_map(&m, 80, 24);
    let lines: Vec<&str> = grid.lines().collect();

    let fact_row = lines
        .iter()
        .position(|l| l.contains("lines hidden"))
        .expect("the fold states its count");
    let verb_row = fact_row + 1;
    assert!(
        lines[verb_row].contains("view full log"),
        "the fact and the verbs are two rows, not one: {}",
        lines[verb_row]
    );

    // Inset: the mark and the text both start to the right of the body's own
    // `$`, so the block steps in from the content's left edge.
    let body_col = lines
        .iter()
        .find(|l| l.trim_start().starts_with("$ cargo build"))
        .map(|l| l.find('$').expect("painted prompt"))
        .expect("the body is painted");
    let gutter = bash_fold_gutter_glyph(false);
    assert_eq!(
        lines[fact_row].find(gutter),
        Some(body_col + BASH_FOLD_INSET_COLS),
        "the fold is inset by BASH_FOLD_INSET_COLS from the body:\n{grid}"
    );
    // Both text rows carry the same mark in the same column: they read as one
    // object, which is what a `↳`-style second mark would break.
    assert_eq!(
        lines[verb_row].find(gutter),
        Some(body_col + BASH_FOLD_INSET_COLS)
    );

    // Blank rows above and below: the fold never butts against output.
    assert!(
        lines[fact_row - 1].trim().is_empty(),
        "a blank row precedes the block:\n{grid}"
    );
    assert!(
        lines[verb_row + 1].trim().is_empty(),
        "a blank row follows the block:\n{grid}"
    );
    // Those blank rows are budgeted chrome, so folding still saves rows.
    assert_eq!(
        (fact_row - 1)..=(verb_row + 1),
        (fact_row - 1)..=(fact_row - 1 + BASH_FOLD_CHROME_ROWS - 1),
        "the block occupies exactly BASH_FOLD_CHROME_ROWS rows"
    );

    // The gutter is bounded: no output row carries it.
    for line in &lines {
        if line.contains(gutter) {
            assert!(
                line.contains("lines hidden") || line.contains("view full log"),
                "the gutter mark may only appear on the fold's own rows, found: {line}"
            );
        }
    }

    // And the verbs register on the sentence row, not the fact row.
    for sentinel in [
        nuo_view::model::layout::TOOL_FOLD_BLOCK_IDX,
        nuo_view::model::layout::TOOL_FOLD_COPY_BLOCK_IDX,
    ] {
        let region = map
            .regions()
            .iter()
            .find(|r| r.block_idx == sentinel)
            .expect("each verb owns a region");
        assert_eq!(region.rect.y as usize, verb_row);
    }
}

/// Under width pressure the verb sentence drops whole clauses and never truncates
/// a word, and the count — the one fact the block exists to carry — survives at
/// every width.
#[test]
fn the_fold_keeps_its_count_and_drops_whole_verb_clauses() {
    let stdout = (1..=20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let m = tool_step_structured(
        "execute_command",
        r#"{"command":"cargo build"}"#,
        nuo_protocol::ToolOutput::Shell {
            command: "cargo build".into(),
            stdout,
            stderr: String::new(),
            lines: Vec::new(),
            exit: Some(0),
            truncated: false,
            truncated_note: None,
            termination: nuo_protocol::tool_output::ShellTermination::Exited,
            detached_job_id: None,
        },
        true,
    );

    // Every width the UI supports, not a hand-picked set: the previous gradient
    // stopped at 44 columns and this test would then have missed the real defect
    // it now pins (a count cut to `12 lines hidde` and a clause cut to
    // `press v view f` below 36 columns).
    for width in nuo_ui_primitives::design::MIN_TERMINAL_COLS..=100 {
        let grid = render_grid(&m, width, 24);
        let rows: Vec<&str> = grid.lines().collect();
        let fact = rows
            .iter()
            .find(|l| l.contains("hidden"))
            .unwrap_or_else(|| panic!("the fold is painted at {width} columns:\n{grid}"));
        let verbs = fact_row_successor(&rows, fact);

        // The count is one of the chosen whole forms, never a fragment.
        assert!(
            ["12 lines hidden (0.1 KB)", "12 lines hidden", "12 hidden"]
                .iter()
                .any(|form| fact.contains(form)),
            "at {width} columns the fact row must carry a whole count, got: {fact}"
        );
        // And nothing on either row ends mid-word: a trailing `(` or a
        // half-written word means a fragment leaked through.
        for row in [fact, verbs] {
            let body = row.trim_end();
            assert!(
                !body.ends_with('(') && !body.ends_with(", press") && !body.ends_with(", v"),
                "at {width} columns a row was cut mid-token: {row}"
            );
        }
    }
}

/// The row painted directly under the fact row (the verb sentence), or `""` when
/// the fact row is the last painted row.
fn fact_row_successor<'a>(rows: &[&'a str], fact: &str) -> &'a str {
    rows.iter()
        .skip_while(|r| **r != fact)
        .nth(1)
        .copied()
        .unwrap_or("")
}

#[test]
fn search_text_expanded_renders_grouped_matches() {
    let m = tool_step(
        "search_text",
        r#"{"query":"foo","path":"src"}"#,
        Some("Found 3 match(es):\nsrc/a.rs:10:let foo = 1;\nsrc/a.rs:22:foo();\nsrc/b.rs:5:foo,"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn search_web_expanded_renders_semantic_cards() {
    let structured = nuo_protocol::ToolOutput::WebSearch {
        query: "rust 1.85 release notes".into(),
        provider: "DuckDuckGo".into(),
        results: vec![
            nuo_protocol::WebSearchHit {
                title: "Rust 1.85.0 released | Rust Blog".into(),
                url: "https://blog.rust-lang.org/2025/02/20/Rust-1.85.0.html".into(),
                domain: "blog.rust-lang.org".into(),
                snippet: "The Rust team is happy to announce a new version of Rust, 1.85.0. Rust 2024 edition is now stable!".into(),
            },
            nuo_protocol::WebSearchHit {
                title: "What is new in Rust 1.85? - GitHub Discussions".into(),
                url: "https://github.com/rust-lang/rust/discussions/134900".into(),
                domain: "github.com".into(),
                snippet: "A summary of major additions including async closures and standard library improvements.".into(),
            },
        ],
        truncated: false,
    };
    let m = tool_step_structured(
        "search_web",
        r#"{"query":"rust 1.85 release notes"}"#,
        structured,
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 20));
}

#[test]
fn read_url_expanded_renders_article_reader() {
    let structured = nuo_protocol::ToolOutput::WebArticle {
        url: "https://blog.rust-lang.org/2025/02/20/Rust-1.85.0.html".into(),
        title: Some("Announcing Rust 1.85.0 and Rust 2024".into()),
        domain: "blog.rust-lang.org".into(),
        markdown: "# Announcing Rust 1.85.0 and Rust 2024\n\nThe Rust team is happy to announce a new version of Rust, 1.85.0.\n\n## Stabilized Features\n\n- Async closures (`async || { ... }`)\n- Naked functions\n\n```rust\nasync fn test() {}\n```".into(),
        reader: "Jina".into(),
        tokens: 1840,
        truncated: false,
    };
    let m = tool_step_structured(
        "read_url",
        r#"{"url":"https://blog.rust-lang.org/2025/02/20/Rust-1.85.0.html"}"#,
        structured,
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 20));
}

#[test]
fn edit_text_expanded_renders_diff() {
    let m = tool_step(
        "edit_text",
        r#"{"path":"a.rs","old_string":"let x = 1;","new_string":"let x = 2;"}"#,
        Some("Edited a.rs"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 30));
}

#[test]
fn edit_text_prose_diff_suppresses_noisy_word_highlights() {
    let m = tool_step(
        "edit_text",
        r#"{"path":"docs/explanation/tui.md","old_string":"Because the frame is a pure function of state, anything that changes state — a streamed token, a permission request, a mouse drag — shows up on the very next frame with no manual invalidation.","new_string":"Because the frame is a pure function of state, diff compares the back grid against the front grid and walks only the dirty rows from each row's dirty column."}"#,
        Some("Edited docs/explanation/tui.md"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 100, 30));
}

#[test]
fn edit_text_multihunk_interleaves_changes() {
    // Two separated single-token edits: the LCS diff must interleave
    // context/remove/add per hunk rather than all-remove-then-all-add.
    let m = tool_step(
        "edit_text",
        r#"{"path":"a.rs","old_string":"fn one() {\n    return 1;\n}\n\nfn two() {\n    return 2;\n}\n","new_string":"fn one() {\n    return 10;\n}\n\nfn two() {\n    return 20;\n}\n"}"#,
        Some("Edited a.rs"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn edit_text_distant_changes_render_explicit_hunks() {
    // Two changes separated by 10 context lines become two explicit hunks.
    // Each hunk owns a standard @@ range header; omitted source is represented
    // by the gap between hunks rather than a synthetic ellipsis row.
    let old = concat!(
        "line  1\nline  2\nCHANGE\n",
        "line  4\nline  5\nline  6\nline  7\nline  8\nline  9\n",
        "line 10\nline 11\nline 12\nline 13\n",
        "CHANGE\nline 15\nline 16\n",
    );
    let new = concat!(
        "line  1\nline  2\nchange\n",
        "line  4\nline  5\nline  6\nline  7\nline  8\nline  9\n",
        "line 10\nline 11\nline 12\nline 13\n",
        "change\nline 15\nline 16\n",
    );
    let args = format!(
        r#"{{"path":"a.rs","old_string":{},"new_string":{}}}"#,
        serde_json::to_string(old).unwrap(),
        serde_json::to_string(new).unwrap(),
    );
    let m = tool_step("edit_text", &args, Some("Edited a.rs"), true);
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

/// A `find_files` step groups entries sharing a directory under one title band,
/// so the shared prefix is not repeated on every row (the 27-ADR case).
#[test]
fn find_files_expanded_groups_entries_under_directory_titles() {
    let output = concat!(
        "Found 3 matching files:\n",
        "docs/adr/0001-x.md\n",
        "docs/adr/0002-y.md\n",
        "src/main.rs",
    );
    let m = tool_step(
        "find_files",
        r#"{"patterns":["*.md"],"path":"."}"#,
        Some(output),
        true,
    );
    let grid = render_grid(&m, 80, 30);

    assert!(
        !grid.contains("Found 3 files") && !grid.contains("Found 3 matching files"),
        "count band must not be rendered: {grid}"
    );
    assert!(grid.contains("docs/adr/"), "directory title band: {grid}");
    assert!(
        grid.contains("0001-x.md") && grid.contains("0002-y.md"),
        "leaf names render under the title: {grid}"
    );
    assert!(grid.contains("main.rs"), "root-level leaf renders: {grid}");
    assert_eq!(
        grid.matches("docs/adr/").count(),
        1,
        "the directory prefix appears once as a title, not per row: {grid}"
    );
}

#[test]
fn execute_command_running_streams_live_preview() {
    // A long-running command mid-stream: status is still Running (header
    // shows a steady `info` accent) but partial stdout already shows under the
    // header via the structured Shell, instead of freezing on a spinner.
    let m = tool_step_streaming(
        "execute_command",
        r#"{"command":"cargo build"}"#,
        nuo_protocol::ToolOutput::Shell {
            command: "cargo build".into(),
            stdout: "Compiling nuotc v0.1.0\nCompiling nuo v0.1.0".into(),
            stderr: String::new(),
            lines: Vec::new(),
            exit: None,
            truncated: false,
            truncated_note: None,
            termination: nuo_protocol::tool_output::ShellTermination::Exited,
            detached_job_id: None,
        },
        false,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 24));
}

#[test]
fn edit_text_diff_renders_from_structured_patch() {
    // The diff now comes from the ToolOutput::Patch payload (old/new), not
    // from re-parsing the tool arguments.
    let m = tool_step_structured(
        "edit_text",
        r#"{"path":"a.rs","old_string":"let x = 1;","new_string":"let x = 2;"}"#,
        nuo_protocol::ToolOutput::Patch {
            path: "a.rs".into(),
            op: nuo_protocol::PatchOp::Edit,
            old: "let x = 1;".into(),
            new: "let x = 2;".into(),
            start_line: 0,
            warnings: Vec::new(),
        },
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 30));
}

#[test]
fn edit_text_diff_renders_with_explicit_start_line() {
    let m = tool_step_structured(
        "edit_text",
        r#"{"path":"a.rs","old_string":"let x = 1;","new_string":"let x = 2;"}"#,
        nuo_protocol::ToolOutput::Patch {
            path: "a.rs".into(),
            op: nuo_protocol::PatchOp::Edit,
            old: "let x = 1;".into(),
            new: "let x = 2;".into(),
            start_line: 42,
            warnings: Vec::new(),
        },
        true,
    );
    let rendered = render_grid(&m, 80, 30);
    assert!(
        rendered.contains("@@ -42 +42 @@"),
        "Hunk header must use start_line 42:\n{rendered}"
    );
    assert!(
        rendered.contains("42   - let x = 1;"),
        "Removed line must show line 42:\n{rendered}"
    );
    assert!(
        rendered.contains("42+ let x = 2;"),
        "Added line must show line 42:\n{rendered}"
    );
    assert!(
        !rendered.contains("@@ -1 +1 @@"),
        "Must not use line 1:\n{rendered}"
    );
}

#[test]
fn failed_edit_renders_error_instead_of_intended_diff() {
    let m = tool_step_structured(
        "edit_text",
        r#"{"path":"a.rs","old_string":"let x = 1;","new_string":"let x = 2;"}"#,
        nuo_protocol::ToolOutput::Error {
            message: "old_string was not found".into(),
            detail: Some("The file changed before the edit ran.".into()),
        },
        true,
    );
    let rendered = render_grid(&m, 80, 30);

    assert!(rendered.contains("Error: old_string was not found"));
    assert!(rendered.contains("The file changed before the edit ran."));
    assert!(!rendered.contains("- let x = 1;"));
    assert!(!rendered.contains("+ let x = 2;"));
}

#[test]
fn write_file_expanded_renders_diff_with_additions() {
    let m = tool_step_structured(
        "write_file",
        r#"{"path":"src/lib.rs","content":"pub fn hello() -> &'static str {\n    \"world\"\n}"}"#,
        nuo_protocol::ToolOutput::Patch {
            path: "src/lib.rs".into(),
            op: nuo_protocol::PatchOp::Create,
            old: String::new(),
            new: "pub fn hello() -> &'static str {\n    \"world\"\n}".into(),
            start_line: 0,
            warnings: vec!["non-blocking syntax diagnostic: example warning".into()],
        },
        true,
    );
    let rendered = render_grid(&m, 80, 20);

    assert!(rendered.contains("Warning: non-blocking syntax diagnostic: example warning"));
    assert!(rendered.contains("Write src/lib.rs +3"));
    assert!(rendered.contains("+ pub fn hello() -> &'static str {"));
    assert!(rendered.contains("+     \"world\""));
    assert!(rendered.contains("+ }"));
    assert!(rendered.contains("@@ -0,0 +1,3 @@"));
    // Every added line has the green diff_add_bg (#121F16) across its full row width.
    assert!(rendered.contains("#121F16"));
}

#[test]
fn write_file_legacy_arguments_renders_diff() {
    let m = tool_step(
        "write_file",
        r#"{"path":"src/main.rs","content":"fn main() {\n    println!(\"hi\");\n}"}"#,
        Some("Wrote src/main.rs"),
        true,
    );
    let rendered = render_grid(&m, 80, 20);

    assert!(rendered.contains("Write src/main.rs +3"));
    assert!(rendered.contains("+ fn main() {"));
    assert!(rendered.contains("+     println!(\"hi\");"));
    assert!(rendered.contains("+ }"));
    assert!(rendered.contains("@@ -0,0 +1,3 @@"));
}

// Tool-step batch spacing (ADR-0001, layout-owned boundaries)
//
// Known same-turn tool steps stack flush regardless of disclosure state. The
// first tests also lock the compatibility behavior for legacy messages whose
// turn is unknown: adjacent collapsed headers remain compact, while an
// expanded legacy body keeps one separator before the next header. These tests
// render the full transcript (`draw_transcript`, which owns the boundary) so
// the single-step `render_grid` helper cannot mask layout behavior.

#[test]
fn ask_user_expanded_renders_question_answer_pairs() {
    let arguments = serde_json::json!({
        "questions": [
            {
                "header": "Scope",
                "question": "How large should the refactor be?",
                "options": [
                    { "label": "Rework the whole telemetry modal" },
                    { "label": "Keep it minimal" }
                ]
            },
            {
                "header": "Layout",
                "question": "Which layout pieces?",
                "options": [
                    { "label": "Vertical stepper" },
                    { "label": "Delta column" },
                    { "label": "Both" }
                ],
                "multi_select": true
            }
        ]
    })
    .to_string();
    // The harness serializes the selection as a JSON array-of-arrays; the second
    // question is multi-select, so its inner array holds two labels.
    let answer = serde_json::json!([
        ["Rework the whole telemetry modal"],
        ["Vertical stepper", "Delta column"]
    ])
    .to_string();
    let m = tool_step_structured(
        "ask_user",
        &arguments,
        nuo_protocol::ToolOutput::Text(format!(
            "User answered the question(s). Selected option labels:\n{answer}"
        )),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 14));
}

#[test]
fn ask_user_expanded_cancelled_renders_no_answer() {
    let arguments = serde_json::json!({
        "questions": [
            {
                "question": "Proceed with the risky path?",
                "options": [ { "label": "Yes" }, { "label": "No" } ]
            }
        ]
    })
    .to_string();
    let m = tool_step_structured(
        "ask_user",
        &arguments,
        nuo_protocol::ToolOutput::Text(
            "User cancelled the question; no answer was provided.".to_string(),
        ),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 8));
}
