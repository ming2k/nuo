//! Snapshot baselines and layout tests for transcript rendering, message spacing,
//! turn headers, and viewport stability.

use super::helpers::{
    first_content_row, render_transcript_frame, render_transcript_grid, tool_step_structured,
};
use nuo_tool::Role;
use nuo_view::model::document::{NoticeSeverity, TranscriptMessage};

/// Scroll anchoring (resize stability). A manual reading position is stored as
/// a raw content-line offset, and a content line's meaning depends entirely on
/// the wrap width — after a reflow the same offset can point at completely
/// different text. The transcript captures the semantic identity (message id +
/// row offset) of the line at the viewport top and, on the pass that follows a
/// width change (`HeightCache::prepare`), maps it back onto the new layout, so
/// the reading position survives a column resize instead of drifting.
#[test]
fn resize_reanchors_the_manual_viewport_top() {
    use crate::shell::render::HeightCache;

    // A long leading paragraph wraps to many more rows at 60 columns than at
    // 80, so every message after it shifts downward on the narrow reflow —
    // the exact condition under which a stale raw offset drifts onto
    // different content.
    let mut messages = vec![TranscriptMessage::new(
        Role::User,
        "lorem ipsum dolor sit amet ".repeat(30),
    )];
    for i in 0..24 {
        messages.push(TranscriptMessage::new(
            Role::User,
            format!("MARK{i:02} unique line"),
        ));
    }

    let mut cache = HeightCache::default();

    // Locate the anchor target (MARK03) at 80 columns with scroll 0. The grid
    // is a *window* onto the stream: the transcript band starts below the
    // viewport's top margin, so a painted grid row's content-line offset is
    // `grid_row - VIEWPORT_TOP_MARGIN`. The window must also be tall enough
    // that MARK03 is actually on screen at scroll 0 — the long leading
    // paragraph pushes it well past a 30-row terminal.
    let wide = render_transcript_frame(&messages, 80, 60, 0, Some(&mut cache));
    let wide_line = (wide
        .grid
        .lines()
        .position(|row| row.contains("MARK03"))
        .expect("MARK03 is visible at 80 columns")
        - nuo_ui_primitives::primitives::VIEWPORT_TOP_MARGIN as usize) as u16;

    // Park the viewport exactly on MARK03 and let the pass capture the anchor.
    let parked = render_transcript_frame(&messages, 80, 30, wide_line, Some(&mut cache));
    assert!(
        first_content_row(&parked.grid).contains("MARK03"),
        "precondition: MARK03 must top the viewport at 80 columns:\n{}",
        parked.grid
    );

    // Reflow to 60 columns while passing the now-stale 80-column offset. The
    // leading paragraph grew, so the same raw offset no longer lands on
    // MARK03 — this is the drift the fix removes.
    let reflowed = render_transcript_frame(&messages, 60, 30, wide_line, Some(&mut cache));
    assert!(
        !first_content_row(&reflowed.grid).contains("MARK03"),
        "documents the drift: the stale 80-column offset must not already \
         land on MARK03 at 60 columns:\n{}",
        reflowed.grid
    );

    // The width change armed a resolve pass; the renderer mapped the captured
    // anchor onto the new layout.
    let resolved = cache
        .take_resolved()
        .expect("a width change must resolve the captured top-of-viewport anchor");
    assert_ne!(
        resolved as u16, wide_line,
        "the leading paragraph wrapped taller at 60 columns, so the resolved \
         offset must differ from the stale 80-column offset"
    );

    // Rendering at the resolved offset restores the same content at the top:
    // the reading position was preserved across the resize.
    let anchored = render_transcript_frame(&messages, 60, 30, resolved as u16, Some(&mut cache));
    assert!(
        first_content_row(&anchored.grid).contains("MARK03"),
        "scroll anchoring must keep MARK03 at the viewport top after the \
         reflow; resolved line {resolved}:\n{}",
        anchored.grid
    );
}

/// No width change ⇒ no anchor resolution: `HeightCache::prepare` only arms a
/// resolve when the wrap width actually moved, so ordinary frames never
/// re-anchor (which would fight the user's own scrolling).
#[test]
fn steady_width_captures_but_never_resolves() {
    use crate::shell::render::HeightCache;

    let messages: Vec<TranscriptMessage> = (0..40)
        .map(|i| TranscriptMessage::new(Role::User, format!("steady line {i}")))
        .collect();
    let mut cache = HeightCache::default();

    let _ = render_transcript_frame(&messages, 80, 20, 5, Some(&mut cache));
    let _ = render_transcript_frame(&messages, 80, 20, 7, Some(&mut cache));
    assert!(
        cache.take_resolved().is_none(),
        "a steady width must never produce a resolved anchor"
    );
}

/// The resolve pass must walk the **full** transcript, not just the virtual
/// window the stale raw offset would select. A wide-narrow reflow grows the
/// leading paragraph enormously, so the anchored content lands far beyond that
/// window; a windowed walk would never measure it and the resolve would
/// silently fail, leaving the raw offset in place (the drift the fix removes).
#[test]
fn resize_resolves_an_anchor_far_outside_the_stale_window() {
    use crate::shell::render::HeightCache;

    // A very long leading paragraph: at 120 columns it wraps to a handful of
    // rows, but at 40 columns it explodes into dozens — a big downward shift
    // for everything after it.
    let mut messages = vec![TranscriptMessage::new(
        Role::Assistant,
        "alphabet soup words words words ".repeat(120),
    )];
    for i in 0..40 {
        messages.push(TranscriptMessage::new(
            Role::User,
            format!("MARK{i:02} line"),
        ));
    }
    let mut cache = HeightCache::default();

    // Find MARK20's content line at 120 columns (tall window so it is on
    // screen at scroll 0), then park on it to capture the anchor. The leading
    // paragraph wraps to dozens of rows and each MARK spans several content
    // lines, so the probe window must be far taller than a normal terminal.
    let wide = render_transcript_frame(&messages, 120, 200, 0, Some(&mut cache));
    let wide_line = (wide
        .grid
        .lines()
        .position(|row| row.contains("MARK20"))
        .expect("MARK20 is visible at 120 columns")
        - nuo_ui_primitives::primitives::VIEWPORT_TOP_MARGIN as usize) as u16;
    let parked = render_transcript_frame(&messages, 120, 24, wide_line, Some(&mut cache));
    assert!(
        first_content_row(&parked.grid).contains("MARK20"),
        "precondition: MARK20 must top the viewport at 120 columns:\n{}",
        parked.grid
    );

    // Reflow to 48 columns at the stale offset. The width must stay at or
    // above `design::MIN_TERMINAL_COLS` (44): below it the renderer substitutes
    // the "terminal too small" notice and returns before `HeightCache::prepare`,
    // so no anchor resolve is armed. The leading paragraph still dwarfs the
    // 24-row viewport, so MARK20 sits far below the stale offset's window.
    let _ = render_transcript_frame(&messages, 48, 24, wide_line, Some(&mut cache));
    let resolved = cache
        .take_resolved()
        .expect("the full-walk resolve pass must find the anchor even when it is off-window");
    assert!(
        resolved > wide_line as usize,
        "MARK20 must have moved downward after the narrow reflow \
         (stale {wide_line}, resolved {resolved})"
    );

    let anchored = render_transcript_frame(&messages, 48, 24, resolved as u16, Some(&mut cache));
    assert!(
        first_content_row(&anchored.grid).contains("MARK20"),
        "the resolved offset must restore MARK20 at the viewport top:\n{}",
        anchored.grid
    );
}

/// A catalog-sync warning (model-list refresh failure) renders as a notification
/// entry: severity header row, gap, then the wrapped body — never as a bare
/// un-styled text line.
/// A notice body must paint **every** wrapped row, not just its first.
///
/// Regression: `draw_notice_view` built a one-element segment list from the
/// wrapped *slice* while passing the slice's row-relative `start_byte` as
/// `render_row_line`'s `base_offset`. Segment ranges and `base_offset` share the
/// logical-row coordinate space, so on any continuation row no segment covered
/// the slice and the splice produced an empty line — a multi-line notice
/// silently rendered its first row and dropped the rest, while
/// `content_lines` still counted them (so the transcript reserved height for
/// rows that were never painted).
#[test]
fn notice_body_paints_every_wrapped_row() {
    let raw = "aa: could not refresh the model list (model-list HTTP request failed: \
               error sending request for url (https://api.deepseek.com/v1/models)). \
               Showing the previous list.";
    let msg = TranscriptMessage::notice(NoticeSeverity::Warning, raw);
    let grid = render_transcript_grid(&[msg], 60, 24);

    // Every word of the body must be reachable in the painted grid. Checking the
    // tail is enough to catch the drop: the old bug kept the first row only.
    let painted: String = grid.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "error sending request",
        "api.deepseek.com",
        "Showing the previous list.",
    ] {
        assert!(
            painted.contains(needle),
            "notice body dropped {needle:?}; painted text was:\n{grid}"
        );
    }
}

#[test]
fn catalog_sync_warning_notice_renders_as_entry() {
    let raw = "aa: could not refresh the model list (model-list HTTP request failed: \
               error sending request for url (https://api.deepseek.com/v1/models)). \
               Showing the previous list.";
    let msg = TranscriptMessage::notice(NoticeSeverity::Warning, raw);
    let grid = render_transcript_grid(&[msg], 60, 24);

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
            "read",
            r#"{"path":"a.rs"}"#,
            nuo_tool::ToolOutput::Code {
                lang: None,
                text: "x".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        ),
        tool_step_structured(
            "read",
            r#"{"path":"b.rs"}"#,
            nuo_tool::ToolOutput::Code {
                lang: None,
                text: "y".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        ),
        tool_step_structured(
            "read",
            r#"{"path":"c.rs"}"#,
            nuo_tool::ToolOutput::Code {
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
            "read",
            r#"{"path":"a.rs"}"#,
            nuo_tool::ToolOutput::Code {
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
            nuo_tool::ToolOutput::Matches {
                pattern: "foo".into(),
                lines: vec!["src/a.rs:10:1:foo".into(), "src/b.rs:5:1:foo".into()],
            },
            true, // expanded legacy step — compatibility separator follows
        ),
        tool_step_structured(
            "read",
            r#"{"path":"c.rs"}"#,
            nuo_tool::ToolOutput::Code {
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
            "read",
            r#"{"path":"src/lib.rs"}"#,
            nuo_tool::ToolOutput::Code {
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
        header.matches('|').count(),
        0,
        "round and timestamp should not have a separator glyph:\n{grid}"
    );
    assert!(
        header.contains("round 5  "),
        "round and timestamp should have whitespace separator:\n{grid}"
    );
}

/// Command entries render a generic header with a right-aligned time
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
            Some(nuo_protocol::CommandResult::PermissionList {
                allowed: vec!["execute_command".to_string()],
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
        "command entries never show collapsible folding markers:\n{grid}"
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
        grid.contains("• execute_command"),
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
        Some(nuo_protocol::CommandResult::Ack {
            title: "Delegated mode ON".to_string(),
            detail: Some(detail),
        }),
    )];

    // A tall viewport provides the reference logical height without clipping.
    let full = render_transcript_frame(&messages, 60, 80, 0, None);

    // Production keeps a height cache across frames. The first clipped frame
    // must cache the same complete height, never just the painted prefix.
    let mut height_cache = crate::shell::render::HeightCache::default();
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

/// Alignment: the invocation shares the body's leading indent, so a
/// completed entry reads as one aligned block — the invocation never hangs to
/// the left of the result body it introduces.
#[test]
fn command_entry_invocation_aligns_with_result_body() {
    let messages = vec![TranscriptMessage::command_result(
        "new",
        "",
        Some(nuo_protocol::CommandResult::Text(
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

/// Concurrent rendering across different entry types.
/// An in-progress / streaming turn entry and a command entry render together.
/// As the turn entry expands in vertical height (more streaming lines), the
/// command entry below it naturally shifts down by the turn's actual height.
#[test]
fn concurrent_turn_and_command_entries_render_and_expand_dynamically() {
    // Stage 1: User prompt + assistant turn with 2 list items + pending command entry.
    let user_msg = TranscriptMessage::new(nuo_tool::Role::User, "Calculate the plan");
    let assistant_v1 = TranscriptMessage::new(
        nuo_tool::Role::Assistant,
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
        nuo_tool::Role::Assistant,
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

/// The command component exists in two states. A pending entry shows
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
    // headline + dimmed detail split (two-tone ack).
    assert!(
        message.settle_command_result(nuo_protocol::CommandResult::Ack {
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
        !message.settle_command_result(nuo_protocol::CommandResult::Text("ignored".to_string(),)),
        "a completed command cannot settle again"
    );
}

/// Two identical commands dispatched in quick succession settle
/// their own rows (the settle finder targets *pending* rows only, FIFO from
/// the tail) — the second reply must not bounce off the first's completed
/// row.
#[test]
fn identical_commands_each_settle_their_own_row() {
    use nuo_view::model::document::{CommandPhase, TranscriptMessage};

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
            .settle_command_result(nuo_protocol::CommandResult::Text("hit 1".to_string())),
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

/// A pending command that will never receive a reply is cancelled —
/// the row stops promising an output but keeps the invocation readable, and
/// the cancel transition is idempotent (a completed row is never cancelled).
#[test]
fn command_component_cancel_marks_pending_row_settled() {
    let mut pending = TranscriptMessage::pending_command("models", "");
    assert!(pending.cancel_pending_command());
    assert_eq!(
        pending.command_result_phase(),
        Some(nuo_view::model::document::CommandPhase::Cancelled)
    );
    assert!(
        !pending.cancel_pending_command(),
        "cancelling a cancelled row is a no-op"
    );

    let mut completed = TranscriptMessage::command_result(
        "new",
        "",
        Some(nuo_protocol::CommandResult::Text(
            "Started new session: a1b2c3".to_string(),
        )),
    );
    assert!(
        !completed.cancel_pending_command(),
        "a completed row is never cancelled"
    );
    assert_eq!(
        completed.command_result_phase(),
        Some(nuo_view::model::document::CommandPhase::Completed)
    );
}

/// A pending row classifies `Plain` regardless of the reply it will
/// eventually hold — the phase owns presentation until the reply settles, so
/// the layout never re-flows on settle without the reply being present.
#[test]
fn command_component_pending_classifies_plain() {
    let pending = TranscriptMessage::pending_command("new", "");
    assert_eq!(
        pending.command_row_layout(200),
        Some(nuo_view::model::document::CommandRowLayout::Plain),
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
        .position(|row| row.contains("› turn 2"))
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
        "read",
        r#"{"path":"a.rs"}"#,
        nuo_tool::ToolOutput::Code {
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
        .position(|row| row.contains("› turn 7"))
        .expect("turn header must render");
    let tool_idx = rows
        .iter()
        .position(|row| row.contains("Read ") && row.contains('+'))
        .expect("tool header must render");

    assert_eq!(
        rows[turn_idx], "  › turn 7  claude-sonnet high",
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
        "read",
        r#"{"path":"a.rs"}"#,
        nuo_tool::ToolOutput::Code {
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
        .position(|row| row.contains("› turn 2"))
        .expect("turn header must render");
    assert_eq!(
        rows[turn_idx], "  › turn 2  gemini-3-pro",
        "no-effort channel renders anchor  model only:\n{grid}"
    );
}

/// A turn header with model, reasoning effort, and timestamp separates components
/// with spatial distance (2 spaces) and unifies model and effort as one component (1 space).
#[test]
fn turn_header_with_model_effort_and_timestamp() {
    let mut step = tool_step_structured(
        "read",
        r#"{"path":"a.rs"}"#,
        nuo_tool::ToolOutput::Code {
            lang: None,
            text: "x".into(),
            start_line: 1,
            prefix: None,
            suffix: None,
        },
        false,
    )
    .with_turn(13)
    .with_attribution("glm-cn", "glm-5.3")
    .with_effort(Some("xhigh"));
    // Set a known timestamp
    step.sent_at_ms = Some(1_700_000_000_000);
    let time_label = nuo_ui_primitives::time::sent_time_label(1_700_000_000_000);

    let grid = render_transcript_grid(&[step], 72, 14);
    let rows: Vec<&str> = grid.lines().collect();
    let turn_idx = rows
        .iter()
        .position(|row| row.contains("› turn 13"))
        .expect("turn header must render");
    assert!(
        rows[turn_idx].contains("› turn 13")
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
        nuo_tool::Role::Assistant,
        "Hello! How can I help you today with your project?",
    )
    .with_turn(1)
    .with_attribution("anthropic", "claude-sonnet");

    let grid = render_transcript_grid(&[assistant], 72, 14);
    assert!(
        grid.contains("› turn 1  claude-sonnet")
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
        "read",
        r#"{"path":"a.rs"}"#,
        nuo_tool::ToolOutput::Code {
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
        "read",
        r#"{"path":"b.rs"}"#,
        nuo_tool::ToolOutput::Code {
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
        .position(|row| row.contains("› turn 7"))
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
            "read",
            format!(r#"{{"path":"{turn}.rs"}}"#).as_str(),
            nuo_tool::ToolOutput::Code {
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
        .filter(|(_, row)| row.trim_start().starts_with("› turn"))
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
/// must span the insert: exactly one `› turn 60` header for the whole turn, with
/// the steer panel nested inside it, and the work that continued afterwards
/// still under that same header. Left ungrouped, the turn would paint a second
/// ADR-0137: a steer is materialized only at a turn seam, so it TERMINATES the
/// turn band rather than being absorbed into it. The band header is painted once
/// per turn, and the steer sits between two turns — never inside one, where it
/// would either split the band or (under the deleted absorption rule) be
/// swallowed into a turn it does not belong to.
#[test]
fn seam_steer_terminates_the_turn_band() {
    use nuo_view::model::document::{SteerOriginTag, UserMessageOrigin};

    let mut thinking = TranscriptMessage::reasoning("re-read the ADR").with_turn(60);
    thinking.set_reasoning_duration(10);
    let step = |path: &str| {
        tool_step_structured(
            "read",
            format!(r#"{{"path":"{path}"}}"#).as_str(),
            nuo_tool::ToolOutput::Code {
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
    // The admitted form: it landed at the seam after turn 60's output, and it
    // is consumed by turn 61 — which is why it carries `turn 61`.
    let steer = TranscriptMessage::new(nuo_tool::Role::User, "stop and re-read the ADR")
        .with_origin(UserMessageOrigin::Steer)
        .with_round(1)
        .with_turn(61)
        .with_sent_at_ms(1_700_000_000_000)
        .with_origin_tags(vec![SteerOriginTag::InRound]);

    let mut resumed = step("b.rs");
    resumed.turn = Some(61);
    let messages = vec![
        thinking,
        step("a.rs"),
        steer,
        resumed,
        TranscriptMessage::new(nuo_tool::Role::Assistant, "done reading").with_turn(61),
    ];

    let grid = render_transcript_grid(&messages, 72, 40);
    let rows: Vec<&str> = grid.lines().collect();

    // Turn 60 and turn 61 are two distinct bands, each with exactly one header.
    let headers: Vec<&str> = rows
        .iter()
        .copied()
        .filter(|row| row.trim_start().starts_with("› turn"))
        .collect();
    assert_eq!(
        headers.len(),
        2,
        "each turn keeps exactly one header (the steer splits the turns, it does \
         not duplicate a header):\n{grid}"
    );
    assert!(
        headers[0].contains("turn 60") && headers[1].contains("turn 61"),
        "the two bands are consecutive turns, never the same number twice:\n{grid}"
    );

    // The steer renders as a one-row interjection, between the two bands.
    let steer_row = rows
        .iter()
        .position(|row| row.contains("›  steer"))
        .expect("steer interjection renders");
    let first_work = rows
        .iter()
        .position(|row| row.contains("Read a.rs"))
        .expect("pre-steer work renders");
    let resumed_work = rows
        .iter()
        .position(|row| row.contains("Read b.rs"))
        .expect("post-steer work renders");
    assert!(
        first_work < steer_row && steer_row < resumed_work,
        "the steer sits at the seam between the two turns' work:\n{grid}"
    );
}

/// A steer that was already admitted (delivered, stamped with the turn it
/// interrupted) groups identically — the position stamp must not promote it to
/// The same geometry holds for the follow-up weight class: a `RoundOpening`
/// steer keeps full round-panel weight, so its provenance breadcrumb is
/// visible — the tag, not the origin, chooses the weight (ADR-0137).
#[test]
fn round_opening_steer_keeps_round_panel_weight() {
    use nuo_view::model::document::{SteerOriginTag, UserMessageOrigin};

    let steer = TranscriptMessage::new(nuo_tool::Role::User, "adjust the heading")
        .with_origin(UserMessageOrigin::Steer)
        .with_round(3)
        .with_sent_at_ms(1_700_000_000_000)
        .with_origin_tags(vec![SteerOriginTag::RoundOpening]);

    let grid = render_transcript_grid(&[steer], 72, 20);
    assert!(
        grid.contains("› steer"),
        "the round-opening form renders the steer anchor:\n{grid}"
    );
    assert!(
        grid.contains("round 3"),
        "it keeps its round provenance — the panel form carries the breadcrumb:\n{grid}"
    );
}

/// Only steer inserts are absorbed. A notice (e.g. the round-interrupt row) is
/// still a group terminator, so an assistant component after it starts a fresh
/// band rather than being swallowed into the interrupted one.
#[test]
fn notice_still_splits_a_turn_band() {
    let step = tool_step_structured(
        "read",
        r#"{"path":"a.rs"}"#,
        nuo_tool::ToolOutput::Code {
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
        nuo_view::model::document::NoticeSeverity::Warning,
        "stopped by the user",
    );
    let after = tool_step_structured(
        "read",
        r#"{"path":"b.rs"}"#,
        nuo_tool::ToolOutput::Code {
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
        grid.lines()
            .filter(|row| row.trim_start().starts_with("› turn"))
            .count(),
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
            Some(nuo_protocol::CommandResult::Ack {
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
    let round_msg = TranscriptMessage::new(nuo_tool::Role::User, "Hello with round")
        .with_round(1)
        .with_sent_at_ms(epoch_ms);
    let prompt_msg = TranscriptMessage::new(nuo_tool::Role::User, "Hello unpositioned")
        .with_sent_at_ms(epoch_ms);

    let grid_round = render_transcript_grid(&[round_msg], 72, 18);
    assert!(
        grid_round.contains("› round 1"),
        "must render '› round 1' with the canonical lead glyph:\n{grid_round}"
    );
    assert!(
        !grid_round.contains("Sent"),
        "must not contain Sent fallback:\n{grid_round}"
    );

    // Verify the blank line between `› round 1` header and the user message panel.
    let rows: Vec<&str> = grid_round.lines().collect();
    let header_idx = rows
        .iter()
        .position(|r| r.contains("› round 1"))
        .expect("must contain header");
    assert!(
        rows[header_idx + 1].trim().is_empty(),
        "must have an empty line below header:\n{grid_round}"
    );

    let grid_prompt = render_transcript_grid(&[prompt_msg], 72, 18);
    assert!(
        grid_prompt.contains("› prompt"),
        "must render prompt anchor with Unix stdin glyph:\n{grid_prompt}"
    );
    assert!(
        !grid_prompt.contains("Sent"),
        "must not contain Sent fallback:\n{grid_prompt}"
    );
}

#[test]
fn user_steer_and_followup_render_clean_unified_headers() {
    use nuo_view::model::document::UserMessageOrigin;
    let epoch_ms = 1_700_000_000_000;

    // Delivered steer with round & turn provenance
    let mut steer_msg = TranscriptMessage::new(nuo_tool::Role::User, "Adjust heading")
        .with_round(1)
        .with_sent_at_ms(epoch_ms);
    steer_msg.origin = UserMessageOrigin::Steer;
    steer_msg.turn = Some(2);

    let grid_steer = render_transcript_grid(&[steer_msg], 72, 18);
    assert!(
        grid_steer.contains("› steer  round 1 › turn 2"),
        "must render steer with round › turn breadcrumb:\n{grid_steer}"
    );
    assert!(
        !grid_steer.contains("↳"),
        "must never use ↳ glyph in user prompt header:\n{grid_steer}"
    );

    // Delivered follow-up with round provenance
    let mut followup_msg = TranscriptMessage::new(nuo_tool::Role::User, "Next step")
        .with_round(2)
        .with_sent_at_ms(epoch_ms);
    followup_msg.origin = UserMessageOrigin::FollowUp;

    let grid_followup = render_transcript_grid(&[followup_msg], 72, 18);
    assert!(
        grid_followup.contains("› follow-up  round 2"),
        "must render follow-up with round context:\n{grid_followup}"
    );
    assert!(
        !grid_followup.contains("↳"),
        "must never use ↳ glyph in follow-up header:\n{grid_followup}"
    );
}

#[test]
fn user_prompt_sending_and_cancelled_render_clean_headers() {
    let epoch_ms = 1_700_000_000_000;

    // Sending prompt
    let sending_msg = TranscriptMessage::new(nuo_tool::Role::User, "Hello world")
        .with_sent_at_ms(epoch_ms)
        .sending();

    let grid_sending = render_transcript_grid(&[sending_msg], 72, 18);
    assert!(
        grid_sending.contains("› prompt  sending"),
        "must render sending prompt with sending status chip:\n{grid_sending}"
    );

    // Cancelled prompt with round provenance
    let cancelled_round_msg =
        TranscriptMessage::new(nuo_tool::Role::User, "Cancelled round prompt")
            .with_sent_at_ms(epoch_ms)
            .with_round(1)
            .cancelled();

    let grid_cancelled_round = render_transcript_grid(&[cancelled_round_msg], 72, 18);
    assert!(
        grid_cancelled_round.contains("› round 1  cancelled"),
        "must render cancelled prompt with round provenance:\n{grid_cancelled_round}"
    );
}
