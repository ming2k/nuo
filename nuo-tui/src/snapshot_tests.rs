//! Snapshot baselines for tool-step rendering.
//!
//! These lock the text/layout of the tool step so the rendering
//! refactor (RenderCtx extraction, and later redesign steps) can be verified
//! behavior-preserving. Snapshots capture the painted grid (cell symbols per
//! row, trailing whitespace trimmed) at a fixed terminal size.
//!
//! Regenerate baselines after an intentional visual change:
//!
//! ```sh
//! INSTA_UPDATE=always cargo test -p nuo-tui snapshot_tests
//! ```

#![cfg(test)]

use nuotc::Rect;

use crate::model::document::{MessageKind, NoticeSeverity, TranscriptMessage};
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use nuo_wire::Role;

use super::Theme;
use super::disclosure::draw_tool_step;

/// Build a finished tool-step message with optional output and expand state.
fn tool_step(
    name: &str,
    arguments: &str,
    output: Option<&str>,
    expanded: bool,
) -> TranscriptMessage {
    let mut m = TranscriptMessage::tool_step("call_test", name, arguments);
    if let MessageKind::ToolStep {
        output: out,
        expanded: exp,
        ..
    } = &mut m.kind
    {
        *out = output.map(str::to_string);
        *exp = expanded;
    }
    m
}

/// Like [`tool_step`] but also sets the structured payload + terminal status,
/// mirroring what `finish_tool_step` produces in production.
fn tool_step_structured(
    name: &str,
    arguments: &str,
    structured: nuo_wire::ToolOutput,
    expanded: bool,
) -> TranscriptMessage {
    let text = structured.to_text();
    let mut m = TranscriptMessage::tool_step("call_test", name, arguments);
    m.finish_tool_step("call_test", text, structured, 0);
    if let MessageKind::ToolStep { expanded: exp, .. } = &mut m.kind {
        *exp = expanded;
    }
    m
}

/// A still-running tool step carrying a partial structured payload, mirroring
/// what `push_tool_stream` produces mid-stream (status stays `Running`).
fn tool_step_streaming(
    name: &str,
    arguments: &str,
    structured: nuo_wire::ToolOutput,
    expanded: bool,
) -> TranscriptMessage {
    let mut m = TranscriptMessage::tool_step("call_test", name, arguments);
    if let MessageKind::ToolStep {
        structured: s,
        expanded: exp,
        ..
    } = &mut m.kind
    {
        *s = Some(Box::new(structured));
        *exp = expanded;
    }
    m
}

/// Render `msg` as a tool step into a fresh `width x height` buffer and
/// return the painted grid as trimmed text rows joined by newlines.
fn render_grid(msg: &TranscriptMessage, width: u16, height: u16) -> String {
    let mut terminal = nuotc::TestTerminal::new(width, height);
    terminal.draw(|f| {
        let area: Rect = f.area();
        let mut layout_map = LayoutMap::default();
        let selection = SelectionState::default();
        let theme = Theme::default();
        let mut diff_cache = crate::tools::DiffCache::default();
        let mut skip_rows = 0usize;
        let mut current_y = area.y;
        let mut content_lines = 0usize;
        let mut sticky = Vec::new();
        let mut wrap_cache = crate::render::BlockWrapCache::default();
        let mut ctx = crate::disclosure::renderers::RenderCtx::from_cursor(
            f,
            area,
            area.width as usize,
            &theme,
            &mut layout_map,
            &mut skip_rows,
            &mut current_y,
            &mut content_lines,
            &mut wrap_cache,
        );
        draw_tool_step(
            &mut ctx,
            msg,
            0,
            &selection,
            None,
            &mut diff_cache,
            &mut sticky,
            false,
            false,
        );
    });

    let buf = terminal.buffer();
    let bw = buf.area().width as usize;
    let mut rows: Vec<String> = Vec::with_capacity(height as usize);
    for y in 0..height as usize {
        let mut row = String::new();
        for x in 0..width as usize {
            let cell = &buf.content[y * bw + x];
            let sym: &str = cell.symbol();
            row.push_str(sym);
        }
        rows.push(row.trim_end().to_string());
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    let grid = rows.join("\n");

    // Style layer: a compact run-length map of the background per row, plus a
    // legend. Text snapshots can't see color, so this makes palette/banding
    // changes (Step 2) visible and reviewable. Symbols are assigned per-frame
    // in first-appearance order; `.` is the terminal default.
    let (bgmap, legend) = background_map(buf);
    if bgmap.is_empty() {
        grid
    } else {
        format!("{grid}\n\nbackgrounds:\n{legend}\n{bgmap}")
    }
}

/// Compact per-row background run-length map + legend for a rendered buffer.
/// Skips rows that are entirely the terminal default so the output stays
/// focused on the painted step.
fn background_map(buf: &nuotc::Grid) -> (String, String) {
    use nuotc::Color;
    type Bg = Color;

    let bw = buf.area().width as usize;
    let bh = buf.area().height as usize;

    let is_default = |bg: Bg| bg == Color::Reset;

    // Distinct bg colors in first-appearance order.
    let mut order: Vec<Bg> = Vec::new();
    for y in 0..bh {
        for x in 0..bw {
            let bg = buf.content[y * bw + x].style().bg;
            if !order.contains(&bg) {
                order.push(bg);
            }
        }
    }
    let sym_of = |bg: Bg| -> char {
        if is_default(bg) {
            '.'
        } else {
            let i = order
                .iter()
                .position(|x| is_default(*x) == is_default(bg) && x == &bg)
                .unwrap_or(0);
            (b'A' + i as u8) as char
        }
    };
    let fmt_color = |bg: Bg| -> String {
        match bg {
            Color::Reset => "reset".to_string(),
            Color::Rgb(r, g, b) => format!("#{:02X}{:02X}{:02X}", r, g, b),
            other => format!("{:?}", other),
        }
    };
    let legend = order
        .iter()
        .map(|&bg| format!("{}={}", sym_of(bg), fmt_color(bg)))
        .collect::<Vec<_>>()
        .join("  ");

    let mut lines: Vec<String> = Vec::new();
    for y in 0..bh {
        let mut runs: Vec<(char, usize)> = Vec::new();
        for x in 0..bw {
            let s = sym_of(buf.content[y * bw + x].style().bg);
            match runs.last_mut() {
                Some((last, n)) if *last == s => *n += 1,
                _ => runs.push((s, 1)),
            }
        }
        while matches!(runs.last(), Some(('.', _))) {
            runs.pop();
        }
        if runs.is_empty() {
            continue;
        }
        let line = runs
            .iter()
            .map(|(s, n)| format!("{}{}", s, n))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(format!("{}: {}", y, line));
    }
    (lines.join("\n"), legend)
}

#[test]
fn read_text_expanded_renders_code_block() {
    let m = tool_step(
        "read_text",
        r#"{"path":"src/lib.rs"}"#,
        Some("fn main() {\n    let x = 1;\n}\n"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn read_text_with_offset_numbers_from_start_line() {
    // A structured `Code` carrying `start_line: 100` (as `read_text` emits
    // when called with `offset: 100`) must number the gutter 100, 101, … —
    // not restart at 1. Also locks that the gutter column widens to fit
    // three-digit line numbers instead of overflowing.
    let m = tool_step_structured(
        "read_text",
        r#"{"path":"src/lib.rs","offset":100}"#,
        nuo_wire::ToolOutput::Code {
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
fn read_text_structured_code_hides_model_framing_and_numbers_from_offset() {
    // The structured `Code` carries model-facing `prefix`/`suffix` framing; the
    // renderer must draw only the pure `text` with a gutter based at
    // `start_line` — never the `[Lines …]` header or the continuation hint.
    let m = tool_step_structured(
        "read_text",
        r#"{"path":"src/lib.rs","offset":100,"limit":2}"#,
        nuo_wire::ToolOutput::Code {
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
        nuo_wire::ToolOutput::Shell {
            command: "cargo test".into(),
            stdout: "running 3 tests\n...\ntest result: ok. 3 passed".into(),
            stderr: "warning: unused import".into(),
            lines: Vec::new(),
            exit: Some(1),
            truncated: false,
            termination: nuo_wire::tool_output::ShellTermination::Exited,
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
    use nuo_wire::tool_output::{ShellLine, ShellStream};
    let m = tool_step_structured(
        "execute_command",
        r#"{"command":"cargo build"}"#,
        nuo_wire::ToolOutput::Shell {
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
            termination: nuo_wire::tool_output::ShellTermination::Exited,
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
        nuo_wire::ToolOutput::Shell {
            command: "cargo build".into(),
            stdout,
            stderr: String::new(),
            lines: Vec::new(),
            exit: Some(1),
            truncated: false,
            termination: nuo_wire::tool_output::ShellTermination::Exited,
            detached_job_id: None,
        },
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn search_text_expanded_renders_grouped_matches() {
    let m = tool_step(
        "search_text",
        r#"{"query":"foo","path":"src"}"#,
        Some("src/a.rs:10:let foo = 1;\nsrc/a.rs:22:foo();\nsrc/b.rs:5:foo,"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn search_web_expanded_renders_semantic_cards() {
    let structured = nuo_wire::ToolOutput::WebSearch {
        query: "rust 1.85 release notes".into(),
        provider: "DuckDuckGo".into(),
        results: vec![
            nuo_wire::WebSearchHit {
                title: "Rust 1.85.0 released | Rust Blog".into(),
                url: "https://blog.rust-lang.org/2025/02/20/Rust-1.85.0.html".into(),
                domain: "blog.rust-lang.org".into(),
                snippet: "The Rust team is happy to announce a new version of Rust, 1.85.0. Rust 2024 edition is now stable!".into(),
            },
            nuo_wire::WebSearchHit {
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
    let structured = nuo_wire::ToolOutput::WebArticle {
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

#[test]
fn list_dir_expanded_renders_listing() {
    let m = tool_step(
        "list_dir",
        r#"{"path":"."}"#,
        Some("src/\ntests/\nCargo.toml\nREADME.md"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 30));
}

#[test]
fn execute_command_running_streams_live_preview() {
    // A long-running command mid-stream: status is still Running (header
    // shows a steady `info` accent) but partial stdout already shows under the
    // header via the structured Shell, instead of freezing on a spinner.
    let m = tool_step_streaming(
        "execute_command",
        r#"{"command":"cargo build"}"#,
        nuo_wire::ToolOutput::Shell {
            command: "cargo build".into(),
            stdout: "Compiling nuotc v0.1.0\nCompiling nuo v0.1.0".into(),
            stderr: String::new(),
            lines: Vec::new(),
            exit: None,
            truncated: false,
            termination: nuo_wire::tool_output::ShellTermination::Exited,
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
        nuo_wire::ToolOutput::Patch {
            path: "a.rs".into(),
            op: nuo_wire::PatchOp::Edit,
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
fn failed_edit_renders_error_instead_of_intended_diff() {
    let m = tool_step_structured(
        "edit_text",
        r#"{"path":"a.rs","old_string":"let x = 1;","new_string":"let x = 2;"}"#,
        nuo_wire::ToolOutput::Error {
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
        nuo_wire::ToolOutput::Patch {
            path: "src/lib.rs".into(),
            op: nuo_wire::PatchOp::Create,
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

/// Render `steps` (already finalized tool-step messages) through the full
/// transcript pipeline and return the painted grid as trimmed rows. Unlike
/// [`render_grid`], this exercises `draw_transcript` so the message-level
/// spacing between consecutive steps is captured. Backgrounds are omitted:
/// these tests are about row counts, not palette.
struct RenderedTranscript {
    grid: String,
    content_lines: usize,
    view_height: u16,
}

fn render_transcript_frame(
    messages: &[TranscriptMessage],
    width: u16,
    height: u16,
    scroll: u16,
    height_cache: Option<&mut super::HeightCache>,
) -> RenderedTranscript {
    use super::{EmptyStateGuidance, QueueBarProps, Theme, TranscriptProps, draw_transcript};
    use crate::model::layout::LayoutMap;

    let theme = Theme::default();
    let selection = SelectionState::default();
    let mut terminal = nuotc::TestTerminal::new(width, height);
    let mut layout_map = LayoutMap::new();
    let mut render = None;
    terminal.draw(|f| {
        render = Some(draw_transcript(
            f,
            &mut layout_map,
            TranscriptProps {
                messages,
                scroll,
                selection: &selection,
                cell_selection: None,
                backoff_clause: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: None,
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache,
            },
        ));
    });

    let buf = terminal.buffer();
    let bw = buf.area().width as usize;
    let mut rows: Vec<String> = Vec::with_capacity(height as usize);
    for y in 0..height as usize {
        let mut row = String::new();
        for x in 0..width as usize {
            row.push_str(buf.content[y * bw + x].symbol());
        }
        rows.push(row.trim_end().to_string());
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    let render = render.expect("draw_transcript must return layout metrics");
    RenderedTranscript {
        grid: rows.join("\n"),
        content_lines: render.content_lines,
        view_height: render.view_height,
    }
}

fn render_transcript_grid(messages: &[TranscriptMessage], width: u16, height: u16) -> String {
    render_transcript_frame(messages, width, height, 0, None).grid
}

/// A catalog-sync warning (model-list refresh failure) renders as a notification
/// entry: severity header row, gap, then the wrapped body — never as a bare
/// un-styled text line.
#[test]
fn catalog_sync_warning_notice_renders_as_entry() {
    let raw = "aa: could not refresh the model list (model-list HTTP request failed: \
               error sending request for url (https://api.deepseek.com/v1/models)). \
               Showing the previous list.";
    let msg = TranscriptMessage::notice(NoticeSeverity::Warning, raw);
    let grid = render_transcript_grid(&[msg], 60, 12);
    println!("{grid}");

    // Row 0 is the page-header band; the entry header is the first painted
    // transcript row.
    let first_line = grid
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    assert!(
        first_line.contains("notification"),
        "notice must render with a notification entry header, got: {first_line:?}"
    );
    // The body wraps inside the band; continuation lines must be flush with
    // the body start, not floating at a random gutter.
    let body_line = grid
        .lines()
        .find(|line| line.contains("error sending request"))
        .unwrap_or_default();
    assert!(
        !body_line.trim().is_empty(),
        "body must contain the wrapped error text, got: {body_line:?}"
    );
    let body_indent = body_line.len() - body_line.trim_start().len();
    let first_body = grid
        .lines()
        .find(|line| line.contains("aa: could not refresh"))
        .unwrap_or_default();
    let first_indent = first_body.len() - first_body.trim_start().len();
    assert_eq!(
        body_indent, first_indent,
        "continuation lines must be flush with the first body line"
    );
    // The entry body is *content*, so it must sit at least one column deeper
    // than the entry head (header row) — head at the gutter, body indented.
    let header_indent = first_line.len() - first_line.trim_start().len();
    assert!(
        first_indent > header_indent,
        "notice body must be indented past the entry head (head={header_indent}, body={first_indent})"
    );
}

/// A batch of collapsed tool steps renders with no blank rows between headers.
#[test]
fn collapsed_tool_steps_stack_flush() {
    let steps = vec![
        tool_step_structured(
            "read_text",
            r#"{"path":"a.rs"}"#,
            nuo_wire::ToolOutput::Code {
                lang: None,
                text: "x".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        ),
        tool_step_structured(
            "read_text",
            r#"{"path":"b.rs"}"#,
            nuo_wire::ToolOutput::Code {
                lang: None,
                text: "y".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        ),
        tool_step_structured(
            "read_text",
            r#"{"path":"c.rs"}"#,
            nuo_wire::ToolOutput::Code {
                lang: None,
                text: "z".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        ),
    ];
    let grid = render_transcript_grid(&steps, 60, 14);
    // The three headers must be adjacent: no blank row between any pair. Each
    // header carries the disclosure marker (`+` collapsed) somewhere in the
    // line, so locate their row indices and assert they are consecutive.
    let header_idx: Vec<usize> = grid
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("Read ") && (l.contains('+') || l.contains('-')))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(header_idx.len(), 3, "expected three Read headers:\n{grid}");
    assert_eq!(
        header_idx[1] - header_idx[0],
        1,
        "first two collapsed headers must be flush (no blank row):\n{grid}"
    );
    assert_eq!(
        header_idx[2] - header_idx[1],
        1,
        "last two collapsed headers must be flush (no blank row):\n{grid}"
    );
}

/// For legacy messages without turn stamps, an expanded body sits flush
/// against its own header but retains one compatibility separator before the
/// next header; collapsed neighbors remain flush.
#[test]
fn expanded_body_flush_to_header_neighbours_stay_flush() {
    let steps = vec![
        tool_step_structured(
            "read_text",
            r#"{"path":"a.rs"}"#,
            nuo_wire::ToolOutput::Code {
                lang: None,
                text: "x".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false, // collapsed — flush against the next step's header
        ),
        tool_step_structured(
            "search_text",
            r#"{"query":"foo","path":"src"}"#,
            nuo_wire::ToolOutput::Matches {
                pattern: "foo".into(),
                lines: vec!["src/a.rs:10:1:foo".into(), "src/b.rs:5:1:foo".into()],
            },
            true, // expanded legacy step — compatibility separator follows
        ),
        tool_step_structured(
            "read_text",
            r#"{"path":"c.rs"}"#,
            nuo_wire::ToolOutput::Code {
                lang: None,
                text: "z".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false, // collapsed legacy step — starts below that separator
        ),
    ];
    insta::assert_snapshot!(render_transcript_grid(&steps, 60, 16));
}

/// A user panel followed by a tool step keeps one explicit blank row between the
/// panel transition and the step header, so the two shapes never visually stick
/// together.
#[test]
fn user_message_before_tool_step_has_single_separator_row() {
    let messages = vec![
        TranscriptMessage::new(Role::User, "inspect files").with_round(1),
        tool_step_structured(
            "read_text",
            r#"{"path":"src/lib.rs"}"#,
            nuo_wire::ToolOutput::Code {
                lang: None,
                text: "pub fn main() {}".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        ),
    ];

    let grid = render_transcript_grid(&messages, 60, 16);
    let rows: Vec<&str> = grid.lines().collect();
    let user_text_idx = rows
        .iter()
        .position(|row| row.contains("inspect files"))
        .expect("user text row must render");
    let tool_idx = rows
        .iter()
        .position(|row| row.contains("Read ") && row.contains('+'))
        .expect("collapsed tool header must render");

    // User row + one bottom transition row + one blank separator row + header.
    assert_eq!(
        tool_idx - user_text_idx,
        3,
        "user panel should have exactly one blank row before a following tool step:\n{grid}"
    );
    assert!(
        rows[tool_idx - 1].trim().is_empty(),
        "row immediately before tool header should be the separator blank row:\n{grid}"
    );
}

/// A sent user header with both a round and timestamp renders clean whitespace
/// separation between the two chips instead of middle dot delimiters.
#[test]
fn sent_user_header_has_clean_metadata_separator() {
    let message = TranscriptMessage::new(Role::User, "inspect files")
        .with_round(5)
        .with_sent_at_ms(1_700_000_000_000);

    let grid = render_transcript_grid(&[message], 60, 14);
    let header = grid
        .lines()
        .find(|row| row.contains("round 5"))
        .expect("sent user header must render");

    assert_eq!(
        header.matches('·').count(),
        0,
        "round and timestamp should not have middle dot separator:\n{grid}"
    );
    assert!(
        header.contains("round 5  "),
        "round and timestamp should have whitespace separator:\n{grid}"
    );
}

/// ADR-0111: command entries render a generic header with a right-aligned time
/// followed directly by their invocation and unfolded result body.
#[test]
fn command_entries_render_header_and_direct_body_without_folding() {
    let messages = vec![
        // Plain: command with no persisted result.
        TranscriptMessage::command_result("compact", "", None),
        // Completed: multi-line permission list rendered directly.
        TranscriptMessage::command_result(
            "permissions",
            "",
            Some(nuo_wire::CommandResult::PermissionList {
                allowed: vec!["run_command".to_string()],
            }),
        ),
    ];

    let grid = render_transcript_grid(&messages, 72, 18);
    let rows: Vec<&str> = grid.lines().collect();

    let plain_idx = rows
        .iter()
        .position(|row| row.contains("⌘ command"))
        .expect("command entry must render entry header ⌘ command");
    assert!(
        rows[plain_idx].trim_start().starts_with("⌘ command"),
        "a command entry renders header with leading ⌘ glyph and command label:\n{grid}"
    );
    assert!(
        !rows[plain_idx].contains('┃'),
        "entry header uses no card bar ┃:\n{grid}"
    );
    assert!(
        grid.contains("/compact"),
        "command invocation must be displayed in the entry body:\n{grid}"
    );

    let permissions_idx = rows
        .iter()
        .rposition(|row| row.contains("⌘ command"))
        .unwrap_or_else(|| panic!("command entry must render its header ⌘ command:\n{grid}"));
    assert!(
        rows[permissions_idx].trim_start().starts_with("⌘ command"),
        "a command entry renders its header with leading ⌘ glyph:\n{grid}"
    );
    assert!(
        !rows[permissions_idx].trim_start().starts_with('+')
            && !rows[permissions_idx].trim_start().starts_with('-'),
        "command entries never show collapsible folding markers (ADR-0111):\n{grid}"
    );

    // Verify 1-line gap between header and body
    assert!(
        rows.get(permissions_idx + 1)
            .is_some_and(|r| r.trim().is_empty()),
        "there must be a 1-row gap between command header and body:\n{grid}"
    );

    assert!(
        grid.contains("/permissions"),
        "concrete command name must render in body:\n{grid}"
    );
    assert!(
        grid.contains("Always-allowed tools:"),
        "the command result body renders directly unfolded:\n{grid}"
    );
    assert!(
        grid.contains("• run_command"),
        "the body's list renders through the block renderer:\n{grid}"
    );
}

/// A command Ack may be taller than the transcript viewport (narrow terminals
/// can also turn a few long detail strings into many wrapped rows). Its
/// renderer must measure every logical row regardless of clipping so the
/// height cache and `max_scroll` can expose the complete result.
#[test]
fn command_ack_height_is_viewport_independent_and_last_row_is_reachable() {
    let last_detail = "final delegated-mode detail";
    let mut detail: Vec<String> = (0..24)
        .map(|index| format!("delegated-mode detail {index:02}"))
        .collect();
    detail.push(last_detail.to_string());
    let messages = vec![TranscriptMessage::command_result(
        "delegate",
        "on",
        Some(nuo_wire::CommandResult::Ack {
            title: "Delegated mode ON".to_string(),
            detail: Some(detail),
        }),
    )];

    // A tall viewport provides the reference logical height without clipping.
    let full = render_transcript_frame(&messages, 60, 80, 0, None);

    // Production keeps a height cache across frames. The first clipped frame
    // must cache the same complete height, never just the painted prefix.
    let mut height_cache = super::HeightCache::default();
    let top = render_transcript_frame(&messages, 60, 12, 0, Some(&mut height_cache));
    assert!(
        top.content_lines > top.view_height as usize,
        "the Ack fixture must overflow the transcript viewport"
    );
    assert_eq!(
        top.content_lines, full.content_lines,
        "logical command height must not depend on viewport clipping"
    );

    let max_scroll = top
        .content_lines
        .saturating_sub(top.view_height as usize)
        .min(u16::MAX as usize) as u16;
    let bottom = render_transcript_frame(&messages, 60, 12, max_scroll, Some(&mut height_cache));
    assert_eq!(
        bottom.content_lines, full.content_lines,
        "cached command height must stay exact at the bottom offset"
    );
    assert!(
        bottom.grid.contains(last_detail),
        "the final Ack detail must be reachable at max_scroll:\n{}",
        bottom.grid
    );
}

/// ADR-0111 alignment: the invocation shares the body's leading indent, so a
/// completed entry reads as one aligned block — the invocation never hangs to
/// the left of the result body it introduces.
#[test]
fn command_entry_invocation_aligns_with_result_body() {
    let messages = vec![TranscriptMessage::command_result(
        "new",
        "",
        Some(nuo_wire::CommandResult::Text(
            "Started new session: a1b2c3".to_string(),
        )),
    )];

    let grid = render_transcript_grid(&messages, 72, 12);
    let rows: Vec<&str> = grid.lines().collect();
    let invocation_idx = rows
        .iter()
        .position(|row| row.contains("/new"))
        .expect("invocation row must render");
    let body_idx = rows
        .iter()
        .position(|row| row.contains("Started new session"))
        .expect("result body row must render");

    let invocation_indent = rows[invocation_idx]
        .chars()
        .take_while(|c| *c == ' ')
        .count();
    let body_indent = rows[body_idx].chars().take_while(|c| *c == ' ').count();
    assert_eq!(
        invocation_indent, body_indent,
        "invocation and result body must share the leading indent:\n{grid}"
    );
    assert!(
        invocation_indent >= 2,
        "the invocation is indented off the transcript edge (prose leading indent):\n{grid}"
    );
}

/// ADR-0111: concurrent rendering across different entry types.
/// An in-progress / streaming turn entry and a command entry render together.
/// As the turn entry expands in vertical height (more streaming lines), the
/// command entry below it naturally shifts down by the turn's actual height.
#[test]
fn concurrent_turn_and_command_entries_render_and_expand_dynamically() {
    // Stage 1: User prompt + assistant turn with 2 list items + pending command entry.
    let user_msg = TranscriptMessage::new(nuo_wire::Role::User, "Calculate the plan");
    let assistant_v1 = TranscriptMessage::new(
        nuo_wire::Role::Assistant,
        "- Step 1: Inspecting files.\n- Step 2: Checking configs.",
    );
    let cmd_msg = TranscriptMessage::pending_command("status", "");

    let msgs_v1 = vec![user_msg.clone(), assistant_v1, cmd_msg.clone()];
    let grid_v1 = render_transcript_grid(&msgs_v1, 80, 24);
    let rows_v1: Vec<&str> = grid_v1.lines().collect();

    let cmd_pos_v1 = rows_v1
        .iter()
        .position(|row| row.contains("⌘ command"))
        .expect("command entry header must be present in v1");

    // Stage 2: Assistant turn receives more streaming tokens (4 extra list items).
    let assistant_v2 = TranscriptMessage::new(
        nuo_wire::Role::Assistant,
        "- Step 1: Inspecting files.\n- Step 2: Checking configs.\n- Step 3: Running benchmarks.\n- Step 4: Compiling binary.\n- Step 5: Verifying checksums.\n- Step 6: Ready.",
    );
    let msgs_v2 = vec![user_msg, assistant_v2, cmd_msg];
    let grid_v2 = render_transcript_grid(&msgs_v2, 80, 24);
    let rows_v2: Vec<&str> = grid_v2.lines().collect();

    let cmd_pos_v2 = rows_v2
        .iter()
        .position(|row| row.contains("⌘ command"))
        .expect("command entry header must be present in v2");

    assert!(
        cmd_pos_v2 > cmd_pos_v1,
        "command entry must shift down as preceding turn expands (v1 pos: {cmd_pos_v1}, v2 pos: {cmd_pos_v2}):\nv1:\n{grid_v1}\nv2:\n{grid_v2}"
    );
    assert_eq!(
        cmd_pos_v2 - cmd_pos_v1,
        4,
        "height delta must exactly equal the 4 added lines of turn output"
    );
}

/// ADR-0108 / ADR-0111: the command component exists in two states. A pending entry shows
/// the header and invocation with no reply; settling it in place with the typed result
/// reveals the completed result body.
#[test]
fn command_component_pending_then_completed() {
    let mut message = TranscriptMessage::pending_command("delegate", "on");

    let pending = render_transcript_grid(std::slice::from_ref(&message), 80, 14);
    assert!(
        pending.contains("⌘ command") && pending.contains("/delegate on"),
        "a pending row shows generic header with invocation in body:\n{pending}"
    );
    assert!(
        !pending.trim_start().starts_with('+') && !pending.contains("\n+"),
        "a pending row shows no disclosure marker:\n{pending}"
    );
    assert!(
        !pending.contains("Delegated mode ON"),
        "a pending row shows no reply:\n{pending}"
    );

    // The reply settles the same component in place. The ack carries the
    // headline + dimmed detail split (ADR-0106 two-tone ack).
    assert!(
        message.settle_command_result(nuo_wire::CommandResult::Ack {
            title: "Delegated mode ON".to_string(),
            detail: Some(vec![
                "File edits & creations are auto-approved".to_string(),
                "Commands are auto-approved (catastrophic hard-denies retained)".to_string(),
            ]),
        }),
        "a pending command settles with its result"
    );
    let completed = render_transcript_grid(std::slice::from_ref(&message), 80, 14);
    assert!(
        completed.contains("⌘ command")
            && completed.contains("/delegate on")
            && completed.contains("Delegated mode ON"),
        "the settled entry renders its header, invocation, and result body:\n{completed}"
    );
    // The headline and its detail lines never collapse onto one row.
    let headline_rows = completed
        .lines()
        .filter(|line| line.contains("Delegated mode ON"))
        .count();
    let detail_rows = completed
        .lines()
        .filter(|line| line.contains("auto-approved"))
        .count();
    assert_eq!(
        headline_rows, 1,
        "the ack headline renders on exactly one row:\n{completed}"
    );
    assert_eq!(
        detail_rows, 2,
        "each detail line owns its own row:\n{completed}"
    );
    assert!(
        !completed.contains("•"),
        "the ack rendering drops the old `•` join entirely:\n{completed}"
    );

    // Settling is one-shot: a second reply must not mutate the component.
    assert!(
        !message.settle_command_result(nuo_wire::CommandResult::Text("ignored".to_string(),)),
        "a completed command cannot settle again"
    );
}

/// ADR-0108: two identical commands dispatched in quick succession settle
/// their own rows (the settle finder targets *pending* rows only, FIFO from
/// the tail) — the second reply must not bounce off the first's completed
/// row.
#[test]
fn identical_commands_each_settle_their_own_row() {
    use crate::model::document::{CommandPhase, TranscriptMessage};

    let mut messages = [
        TranscriptMessage::pending_command("search", "foo"),
        TranscriptMessage::pending_command("search", "foo"),
    ];
    // Mirror the event loop's settle finder: newest pending row with this
    // invocation.
    let target = messages.iter_mut().rev().find(|message| {
        message.is_command_result()
            && message.raw == "/search foo"
            && message.command_result_phase() == Some(CommandPhase::Pending)
    });
    assert!(
        target
            .expect("a pending row exists")
            .settle_command_result(nuo_wire::CommandResult::Text("hit 1".to_string())),
        "the reply settles the newest pending row"
    );
    assert_eq!(
        messages[0].command_result_phase(),
        Some(CommandPhase::Pending),
        "the first row keeps waiting for its own reply"
    );
    assert_eq!(
        messages[1].command_result_phase(),
        Some(CommandPhase::Completed)
    );
}

/// ADR-0108: a pending command that will never receive a reply is cancelled —
/// the row stops promising an output but keeps the invocation readable, and
/// the cancel transition is idempotent (a completed row is never cancelled).
#[test]
fn command_component_cancel_marks_pending_row_settled() {
    let mut pending = TranscriptMessage::pending_command("models", "");
    assert!(pending.cancel_pending_command());
    assert_eq!(
        pending.command_result_phase(),
        Some(crate::model::document::CommandPhase::Cancelled)
    );
    assert!(
        !pending.cancel_pending_command(),
        "cancelling a cancelled row is a no-op"
    );

    let mut completed = TranscriptMessage::command_result(
        "new",
        "",
        Some(nuo_wire::CommandResult::Text(
            "Started new session: a1b2c3".to_string(),
        )),
    );
    assert!(
        !completed.cancel_pending_command(),
        "a completed row is never cancelled"
    );
    assert_eq!(
        completed.command_result_phase(),
        Some(crate::model::document::CommandPhase::Completed)
    );
}

/// ADR-0108: a pending row classifies `Plain` regardless of the reply it will
/// eventually hold — the phase owns presentation until the reply settles, so
/// the layout never re-flows on settle without the reply being present.
#[test]
fn command_component_pending_classifies_plain() {
    let pending = TranscriptMessage::pending_command("new", "");
    assert_eq!(
        pending.command_row_layout(200),
        Some(crate::model::document::CommandRowLayout::Plain),
        "a pending row has no result to classify by"
    );
}

/// Expanded reasoning traces keep block gaps but do not add a blank row before
/// the first body line or append a trailing bottom gap. Layout-level spacing
/// owns the boundary before the following assistant text.
#[test]
fn reasoning_trace_spacing_has_internal_gaps_and_single_trailing_separator() {
    let mut reasoning =
        TranscriptMessage::reasoning("first thought\n\nsecond thought").with_turn(1);
    reasoning.pin_reasoning_expanded(true);
    let assistant = TranscriptMessage::new(Role::Assistant, "final answer").with_turn(2);
    let messages = vec![reasoning, assistant];

    let grid = render_transcript_grid(&messages, 72, 18);
    let rows: Vec<&str> = grid.lines().collect();
    let summary_idx = rows
        .iter()
        .position(|row| row.contains("Thinking"))
        .expect("reasoning summary must render");
    let first_idx = rows
        .iter()
        .position(|row| row.contains("first thought"))
        .expect("first reasoning block must render");
    let second_idx = rows
        .iter()
        .position(|row| row.contains("second thought"))
        .expect("second reasoning block must render");
    let turn2_idx = rows
        .iter()
        .position(|row| row.contains("> turn 2"))
        .expect("turn 2 header must render");
    let answer_idx = rows
        .iter()
        .position(|row| row.contains("final answer"))
        .expect("assistant text must render");

    assert_eq!(
        first_idx - summary_idx,
        1,
        "reasoning body should start directly below its summary:\n{grid}"
    );
    assert_eq!(
        second_idx - first_idx,
        2,
        "reasoning blocks should be separated by one blank row:\n{grid}"
    );
    assert_eq!(
        turn2_idx - second_idx,
        2,
        "turn 2 header should have layout separation after turn 1:\n{grid}"
    );
    assert_eq!(
        answer_idx - turn2_idx,
        2,
        "turn 2 header and its body text should have one gap row:\n{grid}"
    );
}

/// The turn header labels the group and keeps one row before its first
/// component.
#[test]
fn default_turn_header_has_one_gap_before_first_tool() {
    let step = tool_step_structured(
        "read_text",
        r#"{"path":"a.rs"}"#,
        nuo_wire::ToolOutput::Code {
            lang: None,
            text: "x".into(),
            start_line: 1,
            prefix: None,
            suffix: None,
        },
        false,
    )
    .with_turn(7)
    .with_attribution("anthropic", "claude-sonnet")
    .with_effort(Some("high"));

    let grid = render_transcript_grid(&[step], 72, 14);
    let rows: Vec<&str> = grid.lines().collect();
    let turn_idx = rows
        .iter()
        .position(|row| row.contains("> turn 7"))
        .expect("turn header must render");
    let tool_idx = rows
        .iter()
        .position(|row| row.contains("Read ") && row.contains('+'))
        .expect("tool header must render");

    assert_eq!(
        rows[turn_idx], "  > turn 7  claude-sonnet high",
        "turn header renders anchor  model effort:\n{grid}"
    );
    assert_eq!(
        tool_idx - turn_idx,
        2,
        "turn header and first tool should have one blank row:\n{grid}"
    );
}

/// A channel that ran without a reasoning effort keeps the header quiet —
/// no dangling modifier for non-reasoning turns.
#[test]
fn turn_header_omits_effort_when_absent() {
    let step = tool_step_structured(
        "read_text",
        r#"{"path":"a.rs"}"#,
        nuo_wire::ToolOutput::Code {
            lang: None,
            text: "x".into(),
            start_line: 1,
            prefix: None,
            suffix: None,
        },
        false,
    )
    .with_turn(2)
    .with_attribution("google", "gemini-3-pro");

    let grid = render_transcript_grid(&[step], 72, 14);
    let rows: Vec<&str> = grid.lines().collect();
    let turn_idx = rows
        .iter()
        .position(|row| row.contains("> turn 2"))
        .expect("turn header must render");
    assert_eq!(
        rows[turn_idx], "  > turn 2  gemini-3-pro",
        "no-effort channel renders anchor  model only:\n{grid}"
    );
}

/// A turn header with model, reasoning effort, and timestamp separates components
/// with spatial distance (2 spaces) and unifies model and effort as one component (1 space).
#[test]
fn turn_header_with_model_effort_and_timestamp() {
    let mut step = tool_step_structured(
        "read_text",
        r#"{"path":"a.rs"}"#,
        nuo_wire::ToolOutput::Code {
            lang: None,
            text: "x".into(),
            start_line: 1,
            prefix: None,
            suffix: None,
        },
        false,
    )
    .with_turn(13)
    .with_attribution("zai", "glm-5.3")
    .with_effort(Some("xhigh"));
    // Set a known timestamp
    step.sent_at_ms = Some(1_700_000_000_000);
    let time_label = crate::time::sent_time_label(1_700_000_000_000);

    let grid = render_transcript_grid(&[step], 72, 14);
    let rows: Vec<&str> = grid.lines().collect();
    let turn_idx = rows
        .iter()
        .position(|row| row.contains("> turn 13"))
        .expect("turn header must render");
    assert!(
        rows[turn_idx].contains("> turn 13")
            && rows[turn_idx].contains("glm-5.3")
            && rows[turn_idx].contains("xhigh")
            && !rows[turn_idx].contains("(xhigh)")
            && rows[turn_idx].contains(&time_label),
        "turn header renders anchor  model effort  time:\n{grid}"
    );
}

/// Pure conversational text (prose without tool steps) also uniformly renders
/// its turn header.
#[test]
fn pure_prose_assistant_turn_renders_turn_header() {
    let assistant = TranscriptMessage::new(
        nuo_wire::Role::Assistant,
        "Hello! How can I help you today with your project?",
    )
    .with_turn(1)
    .with_attribution("anthropic", "claude-sonnet");

    let grid = render_transcript_grid(&[assistant], 72, 14);
    assert!(
        grid.contains("> turn 1  claude-sonnet")
            && grid.contains("Hello! How can I help you today with your project?"),
        "pure prose turn must render its turn header:\n{grid}"
    );
}

/// Thinking and the tool batch are separate visual segments, while parallel
/// tools inside the batch stay flush.
#[test]
fn same_turn_segments_have_gaps_but_parallel_tools_stay_flush() {
    let mut thinking = TranscriptMessage::reasoning("inspect the files").with_turn(7);
    thinking.set_reasoning_duration(10);
    let first = tool_step_structured(
        "read_text",
        r#"{"path":"a.rs"}"#,
        nuo_wire::ToolOutput::Code {
            lang: None,
            text: "a".into(),
            start_line: 1,
            prefix: None,
            suffix: None,
        },
        false,
    )
    .with_turn(7);
    let second = tool_step_structured(
        "read_text",
        r#"{"path":"b.rs"}"#,
        nuo_wire::ToolOutput::Code {
            lang: None,
            text: "b".into(),
            start_line: 1,
            prefix: None,
            suffix: None,
        },
        false,
    )
    .with_turn(7);

    let grid = render_transcript_grid(&[thinking, first, second], 72, 16);
    let rows: Vec<&str> = grid.lines().collect();
    let turn_idx = rows
        .iter()
        .position(|row| row.contains("> turn 7"))
        .expect("turn header must render");
    let thinking_idx = rows
        .iter()
        .position(|row| row.contains("Thinking") || row.contains("Thought"))
        .expect("thinking summary must render");
    let tool_idx: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.contains("Read ") && row.contains('+'))
        .map(|(index, _)| index)
        .collect();

    assert_eq!(
        thinking_idx,
        turn_idx + 2,
        "header → thinking needs one blank row:\n{grid}"
    );
    assert_eq!(
        tool_idx,
        vec![thinking_idx + 2, thinking_idx + 3],
        "thinking → tools needs one row; parallel tools must be flush:\n{grid}"
    );
}

/// A turn boundary, unlike a component boundary inside a turn, keeps the
/// standard single separator row.
#[test]
fn different_tool_turns_have_one_vertical_gap() {
    let make_step = |turn: u64| {
        tool_step_structured(
            "read_text",
            format!(r#"{{"path":"{turn}.rs"}}"#).as_str(),
            nuo_wire::ToolOutput::Code {
                lang: None,
                text: turn.to_string(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        )
        .with_turn(turn)
    };
    let grid = render_transcript_grid(&[make_step(1), make_step(2)], 72, 16);
    let rows: Vec<&str> = grid.lines().collect();
    let turn_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.contains("> turn"))
        .map(|(index, _)| index)
        .collect();
    let tool_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.contains("Read ") && row.contains('+'))
        .map(|(index, _)| index)
        .collect();

    assert_eq!(turn_rows.len(), 2, "expected two turn headers:\n{grid}");
    assert_eq!(tool_rows.len(), 2, "expected two tool headers:\n{grid}");
    assert_eq!(
        turn_rows[1] - tool_rows[0],
        2,
        "turns need exactly one blank separator row:\n{grid}"
    );
    assert_eq!(
        tool_rows[1] - turn_rows[1],
        2,
        "turn header → tool needs one blank row:\n{grid}"
    );
}

/// A steer typed while a turn is still producing output is staged at the live
/// tail, and the turn keeps appending its own components *after* it. The band
/// must span the insert: exactly one `> turn 60` header for the whole turn, with
/// the steer panel nested inside it, and the work that continued afterwards
/// still under that same header. Left ungrouped, the turn would paint a second
/// header and read as two turns numbered 60.
#[test]
fn steer_insert_mid_turn_keeps_one_turn_header() {
    use crate::model::document::{DeliveryStatus, UserMessageOrigin};

    let mut thinking = TranscriptMessage::reasoning("re-read the ADR").with_turn(60);
    thinking.set_reasoning_duration(10);
    let step = |path: &str| {
        tool_step_structured(
            "read_text",
            format!(r#"{{"path":"{path}"}}"#).as_str(),
            nuo_wire::ToolOutput::Code {
                lang: None,
                text: "a".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        )
        .with_turn(60)
    };
    // The queued form the live tail carries: no position yet, staged mid-turn.
    let steer = TranscriptMessage::new(nuo_wire::Role::User, "stop and re-read the ADR")
        .with_origin(UserMessageOrigin::Steer)
        .with_sent_at_ms(1_700_000_000_000)
        .queued();
    assert_eq!(steer.delivery, DeliveryStatus::Queued);

    let messages = vec![
        thinking,
        step("a.rs"),
        steer,
        step("b.rs"),
        TranscriptMessage::new(nuo_wire::Role::Assistant, "done reading").with_turn(60),
    ];

    let grid = render_transcript_grid(&messages, 72, 40);
    let rows: Vec<&str> = grid.lines().collect();
    let header_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.contains("> turn"))
        .map(|(index, _)| index)
        .collect();

    assert_eq!(
        header_rows.len(),
        1,
        "the interrupted turn keeps exactly one header:\n{grid}"
    );
    assert!(grid.contains("> turn 60"), "band labels the turn:\n{grid}");
    assert!(
        grid.contains("< steer"),
        "the steer panel still renders inside the band:\n{grid}"
    );

    // Both halves of the turn's work sit below that single header — the insert
    // must not have pushed the resumed work into a new band.
    let resumed = rows
        .iter()
        .position(|row| row.contains("Read b.rs"))
        .expect("the turn's post-steer work renders");
    let first_work = rows
        .iter()
        .position(|row| row.contains("Read a.rs"))
        .expect("the turn's pre-steer work renders");
    let steer_row = rows
        .iter()
        .position(|row| row.contains("< steer"))
        .expect("steer panel renders");
    assert!(
        first_work < steer_row && steer_row < resumed,
        "the steer stays between the two halves of its turn:\n{grid}"
    );
    assert!(
        resumed > header_rows[0],
        "resumed work stays under the original header:\n{grid}"
    );
}

/// A steer that was already admitted (delivered, stamped with the turn it
/// interrupted) groups identically — the position stamp must not promote it to
/// a group terminator.
#[test]
fn delivered_steer_mid_turn_keeps_one_turn_header() {
    use crate::model::document::UserMessageOrigin;

    let step = |path: &str| {
        tool_step_structured(
            "read_text",
            format!(r#"{{"path":"{path}"}}"#).as_str(),
            nuo_wire::ToolOutput::Code {
                lang: None,
                text: "a".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        )
        .with_turn(60)
    };
    let mut steer = TranscriptMessage::new(nuo_wire::Role::User, "adjust the heading")
        .with_origin(UserMessageOrigin::Steer)
        .with_round(3)
        .with_turn(60)
        .with_sent_at_ms(1_700_000_000_000);
    steer.delivery = crate::model::document::DeliveryStatus::Delivered;

    let messages = vec![step("a.rs"), steer, step("b.rs")];
    let grid = render_transcript_grid(&messages, 72, 40);

    assert_eq!(
        grid.lines().filter(|row| row.contains("> turn")).count(),
        1,
        "a delivered steer stays inside its turn band:\n{grid}"
    );
    assert!(
        grid.contains("< steer  round 3 › turn 60"),
        "the steer keeps its provenance breadcrumb:\n{grid}"
    );
}

/// Only steer inserts are absorbed. A notice (e.g. the round-interrupt row) is
/// still a group terminator, so an assistant component after it starts a fresh
/// band rather than being swallowed into the interrupted one.
#[test]
fn notice_still_splits_a_turn_band() {
    let step = tool_step_structured(
        "read_text",
        r#"{"path":"a.rs"}"#,
        nuo_wire::ToolOutput::Code {
            lang: None,
            text: "a".into(),
            start_line: 1,
            prefix: None,
            suffix: None,
        },
        false,
    )
    .with_turn(60);
    let notice = TranscriptMessage::notice(
        crate::model::document::NoticeSeverity::Warning,
        "stopped by the user",
    );
    let after = tool_step_structured(
        "read_text",
        r#"{"path":"b.rs"}"#,
        nuo_wire::ToolOutput::Code {
            lang: None,
            text: "b".into(),
            start_line: 1,
            prefix: None,
            suffix: None,
        },
        false,
    )
    .with_turn(60);

    let grid = render_transcript_grid(&[step, notice, after], 72, 40);
    assert_eq!(
        grid.lines().filter(|row| row.contains("> turn")).count(),
        2,
        "a notice terminates the band, so the following component re-anchors:\n{grid}"
    );
}

#[test]
fn command_component_renders_lead_symbols_and_timestamps() {
    let epoch_ms = 1_700_000_000_000; // Produces a deterministic HH:MM label
    let messages = vec![
        TranscriptMessage::command_result(
            "delegate",
            "on",
            Some(nuo_wire::CommandResult::Ack {
                title: "Delegated mode ON".to_string(),
                detail: Some(vec!["All tool permissions are auto-approved".to_string()]),
            }),
        )
        .with_sent_at_ms(epoch_ms),
        TranscriptMessage::command_result("compact", "", None).with_sent_at_ms(epoch_ms),
    ];

    let grid = render_transcript_grid(&messages, 140, 18);
    assert!(
        grid.contains("⌘ command") && grid.contains("/delegate on"),
        "slash command must render with ⌘ command header and invocation in body:\n{grid}"
    );
    assert!(
        grid.contains("Delegated mode ON"),
        "ack headline must render in body:\n{grid}"
    );
    assert!(
        grid.lines()
            .any(|line| line.contains("All tool permissions are auto-approved")),
        "ack detail line must render below the headline on its own row:\n{grid}"
    );
    assert!(
        grid.contains("⌘ command") && grid.contains("/compact"),
        "plain command must render with ⌘ command header and invocation in body:\n{grid}"
    );
    assert!(
        !grid.contains("▌ Sent"),
        "command rows must never render ▌ Sent:\n{grid}"
    );
}

#[test]
fn user_messages_render_timestamps_and_never_sent_marker() {
    let epoch_ms = 1_700_000_000_000;
    let round_msg = TranscriptMessage::new(nuo_wire::Role::User, "Hello with round")
        .with_round(1)
        .with_sent_at_ms(epoch_ms);
    let prompt_msg = TranscriptMessage::new(nuo_wire::Role::User, "Hello unpositioned")
        .with_sent_at_ms(epoch_ms);

    let grid_round = render_transcript_grid(&[round_msg], 72, 18);
    assert!(
        grid_round.contains("< round 1"),
        "must render '< round 1' with Unix stdin glyph:\n{grid_round}"
    );
    assert!(
        !grid_round.contains("Sent"),
        "must not contain Sent fallback:\n{grid_round}"
    );

    // Verify the blank line between `< round 1` header and the user message panel.
    let rows: Vec<&str> = grid_round.lines().collect();
    let header_idx = rows
        .iter()
        .position(|r| r.contains("< round 1"))
        .expect("must contain header");
    assert!(
        rows[header_idx + 1].trim().is_empty(),
        "must have an empty line below header:\n{grid_round}"
    );

    let grid_prompt = render_transcript_grid(&[prompt_msg], 72, 18);
    assert!(
        grid_prompt.contains("< prompt"),
        "must render prompt anchor with Unix stdin glyph:\n{grid_prompt}"
    );
    assert!(
        !grid_prompt.contains("Sent"),
        "must not contain Sent fallback:\n{grid_prompt}"
    );
}

#[test]
fn user_steer_and_followup_render_clean_unified_headers() {
    use crate::model::document::{DeliveryStatus, UserMessageOrigin};
    let epoch_ms = 1_700_000_000_000;

    // Delivered steer with round & turn provenance
    let mut steer_msg = TranscriptMessage::new(nuo_wire::Role::User, "Adjust heading")
        .with_round(1)
        .with_sent_at_ms(epoch_ms);
    steer_msg.origin = UserMessageOrigin::Steer;
    steer_msg.turn = Some(2);

    let grid_steer = render_transcript_grid(&[steer_msg], 72, 18);
    assert!(
        grid_steer.contains("< steer  round 1 › turn 2"),
        "must render steer with round › turn breadcrumb:\n{grid_steer}"
    );
    assert!(
        !grid_steer.contains("↳"),
        "must never use ↳ glyph in user prompt header:\n{grid_steer}"
    );

    // Queued steer
    let mut queued_steer = TranscriptMessage::new(nuo_wire::Role::User, "Pending steer")
        .with_sent_at_ms(epoch_ms);
    queued_steer.origin = UserMessageOrigin::Steer;
    queued_steer.delivery = DeliveryStatus::Queued;

    let grid_queued_steer = render_transcript_grid(&[queued_steer], 72, 18);
    assert!(
        grid_queued_steer.contains("< steer  queued"),
        "must render queued steer with clean upright status:\n{grid_queued_steer}"
    );

    // Delivered follow-up with round provenance
    let mut followup_msg = TranscriptMessage::new(nuo_wire::Role::User, "Next step")
        .with_round(2)
        .with_sent_at_ms(epoch_ms);
    followup_msg.origin = UserMessageOrigin::FollowUp;

    let grid_followup = render_transcript_grid(&[followup_msg], 72, 18);
    assert!(
        grid_followup.contains("< follow-up  round 2"),
        "must render follow-up with round context:\n{grid_followup}"
    );
    assert!(
        !grid_followup.contains("↳"),
        "must never use ↳ glyph in follow-up header:\n{grid_followup}"
    );

    // Queued follow-up
    let mut queued_followup =
        TranscriptMessage::new(nuo_wire::Role::User, "Queued next step")
            .with_sent_at_ms(epoch_ms);
    queued_followup.origin = UserMessageOrigin::FollowUp;
    queued_followup.delivery = DeliveryStatus::Queued;

    let grid_queued_followup = render_transcript_grid(&[queued_followup], 72, 18);
    assert!(
        grid_queued_followup.contains("< follow-up  queued"),
        "must render queued follow-up with clean upright status:\n{grid_queued_followup}"
    );
}

#[test]
fn user_prompt_sending_and_cancelled_render_clean_headers() {
    let epoch_ms = 1_700_000_000_000;

    // Sending prompt
    let sending_msg = TranscriptMessage::new(nuo_wire::Role::User, "Hello world")
        .with_sent_at_ms(epoch_ms)
        .sending();

    let grid_sending = render_transcript_grid(&[sending_msg], 72, 18);
    assert!(
        grid_sending.contains("< prompt  sending"),
        "must render sending prompt with sending status chip:\n{grid_sending}"
    );

    // Cancelled prompt with round provenance
    let cancelled_round_msg =
        TranscriptMessage::new(nuo_wire::Role::User, "Cancelled round prompt")
            .with_sent_at_ms(epoch_ms)
            .with_round(1)
            .cancelled();

    let grid_cancelled_round = render_transcript_grid(&[cancelled_round_msg], 72, 18);
    assert!(
        grid_cancelled_round.contains("< round 1  cancelled"),
        "must render cancelled prompt with round provenance:\n{grid_cancelled_round}"
    );
}

