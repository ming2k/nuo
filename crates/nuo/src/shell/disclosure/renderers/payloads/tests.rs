use super::*;
use nuo_protocol::tool_output::ShellTermination;
use nuo_view::Theme;
use nuo_view::tools::shell_output::OutputCoverage;
use nuotc::Rect;
use unicode_width::UnicodeWidthStr;

#[test]
fn termination_footer_renders_stream_guard_warning() {
    let theme = nuo_view::Theme::default();
    let footer = termination_footer(
        ShellTermination::StreamGuard,
        &theme,
        theme.match_title_surface(),
    );
    assert!(footer.is_some());
    let (text, style) = footer.unwrap();
    assert_eq!(text, "terminated — output limit reached");
    assert_eq!(style.fg, theme.warn());
    assert_eq!(style.bg, theme.match_title_surface());
}

#[test]
fn termination_footer_renders_professional_supervisor_verdicts() {
    let theme = nuo_view::Theme::default();
    let surface = theme.match_title_surface();

    let timeout = termination_footer(ShellTermination::Timeout, &theme, surface).unwrap();
    assert_eq!(timeout.0, "terminated — timed out");
    assert_eq!(timeout.1.fg, theme.warn());

    let cancelled = termination_footer(ShellTermination::Cancelled, &theme, surface).unwrap();
    assert_eq!(cancelled.0, "terminated — cancelled");
    assert_eq!(cancelled.1.fg, theme.err());

    let idle = termination_footer(ShellTermination::IdleBlocked, &theme, surface).unwrap();
    assert_eq!(idle.0, "terminated — idle timeout");

    let input = termination_footer(ShellTermination::InputUnanswered, &theme, surface).unwrap();
    assert_eq!(input.0, "terminated — input prompt unanswered");

    let interactive =
        termination_footer(ShellTermination::InteractiveBlocked, &theme, surface).unwrap();
    assert_eq!(interactive.0, "not started — command asks for input");

    let detached = termination_footer(ShellTermination::Detached, &theme, surface).unwrap();
    assert_eq!(detached.0, "moved to background — still running");

    assert!(termination_footer(ShellTermination::Exited, &theme, surface).is_none());
}

/// ADR-0115: the footer must name *which* copy is short. A step whose
/// structured payload crossed the model watermark still holds every row on
/// screen, so it may not claim the screen kept a partial log.
#[test]
fn coverage_footer_names_the_short_copy() {
    assert_eq!(coverage_footer(OutputCoverage::Full), None);
    assert_eq!(
        coverage_footer(OutputCoverage::ModelTruncated),
        Some("output truncated for model context"),
        "the screen holds the whole log; only the model's copy was cut",
    );
    assert_eq!(
        coverage_footer(OutputCoverage::CutBeforeUi),
        Some("output truncated | partial kept"),
        "a payload cut before the UI keeps the older, graver wording",
    );
}

/// The fold label must disclose a hidden failure site the user cannot see
/// (`[INV-FOLD-01]`): drawing only a count leaves the diagnosis unreachable.
#[test]
fn fold_label_surfaces_the_first_hidden_failure_signature() {
    let hidden = vec![
        "compiling foo".to_string(),
        "error[E0425]: cannot find value `config` in this scope".to_string(),
        "panicked at src/main.rs:9".to_string(),
    ];
    assert_eq!(
        first_salient_signature(&hidden).as_deref(),
        Some("error[E0425]: cannot find value `config` in this scope"),
    );
    assert_eq!(first_salient_signature(&["quiet line".to_string()]), None);
}

/// A signature may only be shown in full, at a word boundary, or not at all.
///
/// `[INV-FOLD-01]` wants the hidden failure site named; correctness wants the
/// name to stay *the same name*. A column-boundary cut turned
/// `error[E0425]: …` into `error[E042…` — an identifier for an error that
/// does not exist — at exactly the widths where a reader most needs the code.
#[test]
fn fold_signature_truncates_at_word_boundaries_or_not_at_all() {
    let signature = "error[E0425]: cannot find value `config` in this scope";

    // Wide enough: verbatim, unchanged.
    assert_eq!(
        fold_signature_for_budget(signature, 200).as_deref(),
        Some(signature)
    );
    // Exactly its own width: still verbatim (no gratuitous ellipsis).
    assert_eq!(
        fold_signature_for_budget(signature, signature.width()).as_deref(),
        Some(signature)
    );

    // Every intermediate budget: the leading token always survives whole, and
    // the fragment always ends on a word plus the marker.
    for budget in 14..signature.len() {
        let Some(shown) = fold_signature_for_budget(signature, budget) else {
            continue;
        };
        assert!(
            shown.width() <= budget,
            "`{shown}` overflows its {budget}-column budget"
        );
        assert!(
            shown.starts_with("error[E0425]"),
            "the error code is the part a reader greps for, budget {budget} gave `{shown}`"
        );
        assert!(
            shown.ends_with('…'),
            "a truncated signature says so, budget {budget} gave `{shown}`"
        );
        // The text before the marker is whole-token prefix of the source.
        let body = shown.trim_end_matches('…');
        assert!(
            signature.starts_with(body),
            "`{body}` must be a prefix of the signature"
        );
        let boundary = &signature[body.len()..];
        assert!(
            boundary.is_empty() || boundary.starts_with(' '),
            "`{body}` must end on a token boundary, not mid-token"
        );
    }

    // Below the floor: nothing, rather than a token that does not exist.
    assert_eq!(fold_signature_for_budget(signature, 4), None);
    // A single unbreakable token cannot be truncated honestly either.
    assert_eq!(fold_signature_for_budget("error[E0425]", 8), None);
    // A path is one token, so it is dropped whole rather than cut: naming
    // `src/` would assert a location that does not exist.
    assert_eq!(
        fold_signature_for_budget("panicked at src/main.rs:9", 20).as_deref(),
        Some("panicked at…")
    );
    assert_eq!(
        fold_signature_for_budget("panicked at src/main.rs:9", 30).as_deref(),
        Some("panicked at src/main.rs:9")
    );
}

#[test]
fn is_matches_header_detects_tool_count_line() {
    assert!(is_matches_header("Found 3 match(es):"));
    assert!(is_matches_header("Found 1 match(es):"));
    assert!(!is_matches_header("src/a.rs:10:let foo = 1;"));
    assert!(!is_matches_header("No matching lines found."));
}

#[test]
fn literal_query_rejects_regex_and_empty() {
    assert_eq!(
        literal_query(r#"{"query":"foo","path":"src"}"#).as_deref(),
        Some("foo")
    );
    assert!(literal_query(r#"{"query":"f.o","regex":true}"#).is_none());
    assert!(literal_query(r#"{"query":""}"#).is_none());
    assert!(literal_query(r#"{"path":"src"}"#).is_none());
}

#[test]
fn parse_listing_header_reads_find_files_shape() {
    // `find_files`: no scope path, so the band is just the file count.
    assert_eq!(
        parse_listing_header("Found 27 matching files:").as_deref(),
        Some("Found 27 files")
    );
    assert_eq!(
        parse_listing_header("Found 1 matching files:").as_deref(),
        Some("Found 1 file")
    );
}

#[test]
fn parse_listing_header_is_none_without_the_tool_header() {
    // A restored session may start at the first entry — no band is invented.
    assert!(parse_listing_header("src/main.rs").is_none());
    assert!(parse_listing_header("src/a.rs\nsrc/b.rs").is_none());
    assert!(parse_listing_header("").is_none());
}

#[test]
fn split_dir_leaf_keeps_the_trailing_slash_on_the_directory() {
    assert_eq!(
        split_dir_leaf("docs/adr/0001-x.md"),
        (Some("docs/adr/"), "0001-x.md")
    );
    assert_eq!(split_dir_leaf("src/"), (None, "src/"));
    assert_eq!(split_dir_leaf("Cargo.toml"), (None, "Cargo.toml"));
}

#[test]
fn segment_matches_tiles_text_and_marks_every_occurrence() {
    let segments = segment_matches("let foo = foo;", Some("foo"));
    // Ranges must tile the whole string with no gaps and no overlap.
    let mut cursor = 0usize;
    for &(lo, hi, _) in &segments {
        assert_eq!(lo, cursor, "ranges must be contiguous");
        assert!(hi > lo, "no empty ranges");
        cursor = hi;
    }
    assert_eq!(cursor, "let foo = foo;".len());
    let matched: Vec<&str> = segments
        .iter()
        .filter(|(_, _, m)| *m)
        .map(|&(lo, hi, _)| &"let foo = foo;"[lo..hi])
        .collect();
    assert_eq!(matched, vec!["foo", "foo"]);
}

#[test]
fn segment_matches_none_or_empty_query_is_one_plain_range() {
    assert_eq!(segment_matches("plain text", None), vec![(0, 10, false)]);
    assert_eq!(
        segment_matches("plain text", Some("")),
        vec![(0, 10, false)]
    );
    assert!(segment_matches("", Some("foo")).is_empty());
}

#[test]
fn normalize_code_content_strips_lines_framing_and_embedded_numbers() {
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
    let norm = normalize_code_content(raw, 0, "{}");
    assert_eq!(norm.start_line, 1);
    assert_eq!(
        norm.detected_path.as_deref(),
        Some("docs/adr/0024-two-row-head-band-session-identity-and-scene-row.md")
    );
    assert!(!norm.content.contains("[Lines"));
    assert!(!norm.content.contains("1 |"));
    let lines: Vec<&str> = norm.content.lines().collect();
    assert_eq!(lines[0], "---");
    assert_eq!(lines[1], "id: ADR-0024");
    assert_eq!(lines[8], "---");
    assert_eq!(lines.len(), 10);
    assert_eq!(lines[9], "");
}

#[test]
fn normalize_code_content_handles_offset_read_with_continuation_hint() {
    let raw = "\
[Lines 100-102 of 500 from `src/lib.rs`]
 100 | fn a() {}
 101 | fn b() {}
 102 | fn c() {}
[398 more lines — read with offset=103]";
    let norm = normalize_code_content(raw, 0, "{}");
    assert_eq!(norm.start_line, 100);
    assert_eq!(norm.content, "fn a() {}\nfn b() {}\nfn c() {}");
    assert_eq!(norm.detected_path.as_deref(), Some("src/lib.rs"));
}

#[test]
fn normalize_code_content_handles_colon_delimited_output() {
    let raw = "10: fn hello() {}\n11: fn world() {}";
    let norm = normalize_code_content(raw, 0, "{}");
    assert_eq!(norm.start_line, 10);
    assert_eq!(norm.content, "fn hello() {}\nfn world() {}");
}

#[test]
fn normalize_code_content_preserves_plain_unformatted_code() {
    let raw = "fn main() {\n    let x = 1;\n}\n";
    let norm = normalize_code_content(raw, 1, r#"{"path":"src/main.rs"}"#);
    assert_eq!(norm.start_line, 1);
    assert_eq!(norm.content, raw);
}

#[test]
fn normalize_code_content_uses_arg_offset_when_slice_relative_numbered() {
    let raw = "1 | fn a() {}\n2 | fn b() {}";
    let norm = normalize_code_content(raw, 0, r#"{"path":"src/lib.rs","offset":50}"#);
    assert_eq!(norm.start_line, 50);
    assert_eq!(norm.content, "fn a() {}\nfn b() {}");
}

#[test]
fn find_start_line_in_file_locates_exact_line_number() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test.rs");
    let content = "line 1\nline 2\nline 3\nfn target() {\n    return 42;\n}\nline 7\n";
    std::fs::write(&file_path, content).unwrap();

    // Finding old string returns 1-based start line (line 4)
    let found = find_start_line_in_file(
        "test.rs",
        Some(dir.path()),
        "fn target() {\n    return 42;\n}",
        "fn target() {\n    return 100;\n}",
    );
    assert_eq!(found, Some(4));

    // Finding inside a sub-line snippet
    let found_inner =
        find_start_line_in_file("test.rs", Some(dir.path()), "return 42;", "return 100;");
    assert_eq!(found_inner, Some(5));

    // Finding when old is already replaced (matches new_string)
    let found_new =
        find_start_line_in_file("test.rs", Some(dir.path()), "nonexistent", "return 42;");
    assert_eq!(found_new, Some(5));

    // When neither matches
    let found_none = find_start_line_in_file(
        "test.rs",
        Some(dir.path()),
        "completely missing",
        "also missing",
    );
    assert_eq!(found_none, None);
}

#[test]
fn edit_diff_renders_with_actual_file_line_number() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("a.rs");
    let mut content = String::new();
    for i in 1..=50 {
        content.push_str(&format!("line {i}\n"));
    }
    std::fs::write(&file_path, &content).unwrap();

    let mut terminal = nuotc::TestTerminal::new(80, 20);
    let mut layout_map = nuo_view::model::layout::LayoutMap::default();
    let theme = Theme::default();
    let mut skip_rows = 0;
    let mut y = 0;
    let mut content_lines = 0;
    let mut wrap = crate::shell::render::BlockWrapCache::default();
    let mut diff_cache = DiffCache::default();

    terminal.draw(|f| {
        let mut ctx = RenderCtx {
            frame: f,
            area: Rect::new(0, 0, 80, 20),
            full_width: 80,
            theme: &theme,
            layout_map: &mut layout_map,
            skip_rows: &mut skip_rows,
            y: &mut y,
            content_lines: &mut content_lines,
            wrap: &mut wrap,
            workspace_root: Some(dir.path()),
        };

        draw_tool_result(
            &mut ctx,
            0,
            1,
            "edit_text",
            r#"{"path":"a.rs","old_string":"line 42\n","new_string":"line 42 modified\n"}"#,
            "",
            None, // in-flight or argument-derived
            &mut diff_cache,
            &SelectionState::None,
            0,
            80,
            false,
        );
    });

    let rendered = terminal
        .buffer()
        .cells()
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();

    assert!(
        rendered.contains("@@ -42"),
        "Hunk header must indicate line 42:\n{rendered}"
    );
    assert!(
        rendered.contains("42"),
        "Gutter must contain line 42:\n{rendered}"
    );
    assert!(
        !rendered.contains("@@ -1 ") && !rendered.contains("@@ -1,"),
        "Hunk header must not start from line 1:\n{rendered}"
    );
}

#[test]
fn extract_diff_strings_for_tool_extracts_from_all_forms() {
    // Standard edit_text
    let (old, new) = extract_diff_strings_for_tool(
        "edit_text",
        r#"{"path":"test.rs","old_string":"foo","new_string":"bar"}"#,
    );
    assert_eq!(old, "foo");
    assert_eq!(new, "bar");

    // Namespaced tool name
    let (old_ns, new_ns) = extract_diff_strings_for_tool(
        "provider:edit_text",
        r#"{"path":"test.rs","old_str":"foo","new_str":"bar"}"#,
    );
    assert_eq!(old_ns, "foo");
    assert_eq!(new_ns, "bar");

    // Write file with content
    let (old_wf, new_wf) = extract_diff_strings_for_tool(
        "write_file",
        r#"{"path":"test.rs","content":"hello world"}"#,
    );
    assert_eq!(old_wf, "");
    assert_eq!(new_wf, "hello world");

    // Streaming partial JSON
    let (old_str, new_str) =
        extract_diff_strings_for_tool("edit_text", r#"{"path":"test.rs","old_string":"streaming"#);
    assert_eq!(old_str, "streaming");
    assert_eq!(new_str, "");
}
