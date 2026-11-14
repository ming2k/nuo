//! Bash / shell command output renderer with fold handling.
use nuotc::{Color, Line, Modifier, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::shell::disclosure::renderers::base::{RenderCtx, nonempty_wrapped};
use crate::shell::disclosure::renderers::payloads::summary::draw_blank_rows;
use crate::shell::render::Theme;
use nuo_ui_primitives::design::{
    BASH_FOLD_INSET_COLS, BASH_FOLD_SIGNATURE_MIN_COLS, COPY_LOG_KEY, JOIN_CAUSE, VIEW_LOG_KEY,
};
use nuo_view::model::layout::{BlockRegion, TOOL_FOLD_BLOCK_IDX, TOOL_FOLD_COPY_BLOCK_IDX};
use nuo_view::model::selection::SelectionState;
use nuo_view::text_layout::{
    WrappedLine, block_selection_range, line_selection, line_spans, padded_tail, wrap_text,
};
use nuo_view::tools::shell_output::{BashFold, OutputCoverage, ShellOutput};

/// Resolve a shell step's `ShellTermination` into a themed footer
/// `(text, style)`, or `None` for a healthy `Exited` run (which carries no
/// extra marker beyond its optional `exit N` line).
///
/// The footer answers the one question the output cannot: *the process did not
/// end on its own — the harness stopped it, and here is why*. Each variant is
/// `state — cause`, the join form `docs/reference/tui/visual-language.md`
/// assigns to a cause that explains a state (the same form as `Edit — <name>`).
///
/// The vocabulary is deliberately plain (`output limit`, not `stream cap`) and
/// free of subsystem names: the reader is a human debugging a command, not the
/// component that named the budget (ADR-0063 §Concise, Professional Footers).
/// Every supervisor-interrupted variant states `terminated` explicitly in the
/// `state — cause` join form, because harness termination is the authentic fact
/// that cannot be recovered from the child output stream alone.
pub(crate) fn termination_footer(
    term: nuo_tool::ShellTermination,
    theme: &Theme,
    bg: Color,
) -> Option<(String, Style)> {
    use nuo_tool::ShellTermination as T;
    let warn_style = Style::default().bg(bg).fg(theme.warn());
    let err_style = Style::default().bg(bg).fg(theme.err());
    // `state — cause` via the shared join token, so the separator/spacing can
    // never drift from `docs/reference/tui/visual-language.md`.
    let cause = |state: &str, why: &str| format!("{state}{JOIN_CAUSE}{why}");
    match term {
        T::Exited => None,
        T::IdleBlocked => Some((cause("terminated", "idle timeout"), warn_style)),
        T::InteractiveBlocked => Some((cause("not started", "command asks for input"), warn_style)),
        T::InputUnanswered => Some((cause("terminated", "input prompt unanswered"), warn_style)),
        T::Timeout => Some((cause("terminated", "timed out"), warn_style)),
        T::Cancelled => Some((cause("terminated", "cancelled"), err_style)),
        T::StreamGuard => Some((cause("terminated", "output limit reached"), warn_style)),
        T::Detached => Some((cause("moved to background", "still running"), warn_style)),
    }
}

/// Dynamic supervisor surface tint for Part 3 Harness Governance band (ADR-0070 → ADR-0093).
fn harness_intervention_surface(term: nuo_tool::ShellTermination, theme: &Theme) -> Color {
    use nuo_tool::ShellTermination as T;
    let tint = match term {
        T::Cancelled => theme.err(),
        _ => theme.warn(),
    };
    match theme.code_surface() {
        Color::Rgb(..) => nuo_ui_primitives::theme::mix(theme.code_surface(), tint, 0.12),
        _ => theme.code_surface(),
    }
}

/// Map POSIX/standard shell exit codes to canonical explanatory descriptions (ADR-0070 → ADR-0093).
fn exit_code_explanation(code: i32) -> &'static str {
    match code {
        1 => "general error",
        2 => "misuse of shell builtins",
        126 => "permission denied / cannot execute",
        127 => "command not found",
        128 => "invalid exit argument",
        130 => "interrupted (SIGINT)",
        131 => "core dumped (SIGQUIT)",
        134 => "aborted (SIGABRT)",
        137 => "killed (SIGKILL / OOM)",
        139 => "segmentation fault (SIGSEGV)",
        143 => "terminated (SIGTERM)",
        _ => "error",
    }
}

/// Render a command step as a terminal-like `code_bg` block: a `$ command`
/// prompt line first, then the output's head/tail window (ADR-0115), then the
/// exit and coverage footers. Output rows have no line-number gutter.
///
/// Everything below the prompt is derived by
/// [`nuo_view::tools::shell_output::ShellOutput`]: in-band markers
/// (`STDOUT:`, `Exit N`, the harness notes, offload envelopes) are stripped and
/// the facts they carried are re-materialized in the footers, so a restored
/// flat text and a live structured payload paint the same shape. `revealed`
/// turns the fold off for this step — the rows are the same either way, which
/// is what lets the disclosure ladder promise a reveal that always lands.
///
/// The command line is not selectable (it is derived from the call, not the
/// output stream); output rows are.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_command_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    content: &str,
    structured: Option<&nuo_tool::ToolOutput>,
    command: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
    revealed: bool,
) {
    let result_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(result_bg);
    let base = Style::default().bg(result_bg).fg(ctx.theme.code_text());
    let sel_range = block_selection_range(selection, mi, block_idx);
    // The body is a flat, flush-left block: every row starts at `indent`, and
    // the only per-row mark is the `$ ` on the invocation. There is no left
    // track — a column of `│` down 30 rows of stdout reads as a frame the
    // content does not have, and, because the terminal's own shift-drag
    // selection cannot tell chrome from content, every line a user copied out
    // of the terminal arrived with a border glued to its front. The block is
    // identified by its surface, its `$`, and its indentation instead
    // (ADR-0150).
    let body = CommandBodyCtx::new(indent, ctx.theme);

    // The invocation row. `$ ` is drawn in the brand hue because it names the
    // *act* ("this ran"), which is the one cell of a long block that has to be
    // findable at a glance. It is painted, never registered: the region's text
    // is the command itself, so selecting the row and copying yields
    // `cargo build` rather than `$ cargo build`, which a shell would refuse.
    if !command.is_empty() {
        let cmd_style = Style::default().bg(result_bg).fg(ctx.theme.fg());
        let prompt_wrap = inner_w.saturating_sub(PROMPT_COLS as usize).max(1);
        for cont in command.split('\n') {
            let _ = body.emit(
                ctx,
                mi,
                COMMAND_INVOCATION_BLOCK,
                prompt_wrap,
                pad,
                sel_range,
                cont,
                cmd_style,
                PROMPT_COLS as usize,
                0,
            );
        }
    }

    // ADR-0115: one derivation for the rows, the outcome, and what each
    // audience holds — the same `nuo-view` derivation the activation ladder
    // consults, so the fold can only ever hide what activation can reveal.
    let view = ShellOutput::derive(content, structured);

    let wrap_w = inner_w.max(1);
    let mut byte_offset = 0usize;
    if !view.rows.is_empty() {
        // Revealing is a *viewport* choice, never a data choice: the whole log
        // is in `view.rows` either way.
        let fold = if revealed { None } else { view.fold() };
        byte_offset = emit_command_output(
            ctx,
            mi,
            // The output body is block 1: the model stores the invocation as
            // block 0 and the rows as block 1, so a region registered here
            // indexes the string a selection will slice.
            COMMAND_OUTPUT_BLOCK,
            &body,
            wrap_w,
            pad,
            base,
            sel_range,
            &view.rows,
            fold,
            byte_offset,
        );
    }

    // Part 2: Process Exit Outcome Strip (flush under the output body).
    // Only non-zero exits paint a distinct marker with explanation (ADR-0070 → ADR-0093).
    if let Some(code) = view.exit
        && code != 0
    {
        let expl = exit_code_explanation(code);
        let m = format!("exit {} ({})", code, expl);
        let footer_marker_style = Style::default().bg(result_bg).fg(ctx.theme.warn());
        let _ = body.emit(
            ctx,
            mi,
            block_idx,
            wrap_w,
            pad,
            sel_range,
            &m,
            footer_marker_style,
            0,
            byte_offset,
        );
    }

    // Part 3: Harness Governance Strip (flush beneath Part 2).
    // Only rendered when supervisor intervention occurred (timeout, output
    // limit, cancellation) or when a copy of this log was cut.
    let harness_bg = harness_intervention_surface(view.termination, ctx.theme);
    let harness_pad = Style::default().bg(harness_bg);
    let governance_entry =
        termination_footer(view.termination, ctx.theme, harness_bg).or_else(|| {
            coverage_footer(view.coverage).map(|text| {
                let warn_style = Style::default().bg(harness_bg).fg(ctx.theme.warn());
                (text.to_string(), warn_style)
            })
        });

    if let Some((text, style)) = governance_entry {
        let _ = body.emit(
            ctx,
            mi,
            block_idx,
            wrap_w,
            harness_pad,
            sel_range,
            &text,
            style,
            0,
            byte_offset,
        );
    }
}

/// Columns the `$ ` prompt adds in front of the command text.
const PROMPT_COLS: u16 = 2;
/// The command block's **invocation** block index. Block 1 is the output body —
/// the same two-block shape
/// [`TranscriptMessage::refresh_tool_step`](nuo_view::model::document::TranscriptMessage)
/// stores, so a region's byte offsets index the string a selection slices.
const COMMAND_INVOCATION_BLOCK: usize = 0;
/// The command block's **output body** block index (the rows below the prompt).
const COMMAND_OUTPUT_BLOCK: usize = 1;

/// The two facts a command body row needs to paint itself: where the block
/// starts, and which foreground each of its parts takes.
///
/// It exists so the prompt row, the output rows, the exit strip and the
/// governance strip share one definition of the body's geometry instead of four
/// copies of `indent`. It deliberately carries **no glyph**: a command body has
/// no per-row mark other than the `$` the invocation row owns (ADR-0150).
struct CommandBodyCtx<'t> {
    /// Column the block starts at — the same column every row of the body,
    /// including the folded block's own inset, resolves against.
    indent: usize,
    /// The surface every body row is painted on, so the fold block's own rows
    /// (and its blank rows) sit on the same tone as the output they interrupt.
    surface: Color,
    prompt: Color,
    dim: Color,
    theme: &'t Theme,
}

impl<'t> CommandBodyCtx<'t> {
    fn new(indent: usize, theme: &'t Theme) -> Self {
        Self {
            indent,
            surface: theme.code_surface(),
            prompt: theme.command_prompt(),
            dim: theme.dim(),
            theme,
        }
    }

    /// Paint one body row and register it as addressable text.
    ///
    /// The region's text is the row's **content**, not the painted line: the
    /// block model stores the content, so the byte offsets a region records must
    /// index that same string or every selection in a command body would slice
    /// the wrong bytes. The `$ ` prompt is therefore painted but not registered —
    /// `prefix_cols` moves the hit-test past it, and a drag that starts on it
    /// resolves to the first content byte. Copying `$ cargo build` into a shell
    /// would not run.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        ctx: &mut RenderCtx<'_, '_>,
        mi: usize,
        block_idx: usize,
        wrap_w: usize,
        pad: Style,
        sel_range: Option<(usize, Option<usize>)>,
        text: &str,
        style: Style,
        prompt_cols: usize,
        mut byte_offset: usize,
    ) -> usize {
        let text = text.trim_end_matches(&['\r', '\n'][..]);
        let prompt_gap = " ".repeat(prompt_cols);
        for logical_line in text.split('\n') {
            let logical_line = nuo_tool::normalize_carriage_returns(logical_line);
            let wrapped = nonempty_wrapped(wrap_text(&logical_line, wrap_w));
            for (wrap_idx, wl) in wrapped.iter().enumerate() {
                let block_wl = WrappedLine {
                    text: wl.text.clone(),
                    start_byte: byte_offset + wl.start_byte,
                    end_byte: byte_offset + wl.end_byte,
                };
                // Continuation rows of a wrapped line never repeat the prompt: a
                // `$` on every row would claim each line ran separately.
                let prompt = if wrap_idx == 0 && prompt_cols > 0 {
                    "$ "
                } else {
                    &prompt_gap[..]
                };
                let mut spans: Vec<Span<'static>> = vec![
                    Span::styled(" ".repeat(self.indent), pad),
                    Span::styled(
                        prompt.to_string(),
                        pad.fg(if prompt.starts_with('$') {
                            self.prompt
                        } else {
                            self.dim
                        }),
                    ),
                ];
                spans.extend(
                    line_spans(
                        "",
                        pad,
                        &wl.text,
                        line_selection(sel_range, &block_wl),
                        style,
                        self.theme.selected(),
                    )
                    .spans,
                );
                let used = self.indent + prompt_cols + wl.text.width();
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint_text_row(
                    Line::from(spans),
                    mi,
                    block_idx,
                    &block_wl,
                    (self.indent + prompt_cols) as u16,
                    &[],
                );
            }
            byte_offset += logical_line.len() + 1;
        }
        byte_offset
    }
}

/// The footer line for a command step whose log was cut *somewhere*.
///
/// ADR-0115: state which copy is short, not just that something is. The old
/// single "output truncated | partial kept" claimed the screen held a partial
/// log even when the screen held every byte — the fold was hiding it, and the
/// fold can be revealed. A statement the user can check is the whole point.
pub(crate) fn coverage_footer(coverage: OutputCoverage) -> Option<&'static str> {
    match coverage {
        OutputCoverage::Full => None,
        // The screen is complete; the model's view was cut at the context
        // watermark, so the fold may hide rows it never saw either.
        OutputCoverage::ModelTruncated => Some("output truncated for model context"),
        // The payload arrived already cut: the middle is gone for everyone.
        OutputCoverage::CutBeforeUi => Some("output truncated | partial kept"),
    }
}

/// Paint `rows`, folding the middle when `fold` is `Some`.
#[allow(clippy::too_many_arguments)]
fn emit_command_output(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    body: &CommandBodyCtx<'_>,
    wrap_w: usize,
    pad: Style,
    row_style: Style,
    sel_range: Option<(usize, Option<usize>)>,
    rows: &[String],
    fold: Option<BashFold>,
    mut byte_offset: usize,
) -> usize {
    let emit = |ctx: &mut RenderCtx<'_, '_>, row: &str, byte_offset: usize| {
        body.emit(
            ctx,
            mi,
            block_idx,
            wrap_w,
            pad,
            sel_range,
            row,
            row_style,
            0,
            byte_offset,
        )
    };

    let Some(fold) = fold else {
        for row in rows {
            byte_offset = emit(ctx, row, byte_offset);
        }
        return byte_offset;
    };

    for row in &rows[..fold.head_end] {
        byte_offset = emit(ctx, row, byte_offset);
    }
    // Advance past the hidden rows so every visible row keeps anchoring at its
    // true `output`-space byte offset — a selection spanning the fold must copy
    // the rows around it exactly as if the middle were on screen.
    for row in fold.hidden_rows(rows) {
        byte_offset += row.len() + 1;
    }
    emit_fold_block(ctx, mi, body, &fold, fold.hidden_rows(rows));
    for row in &rows[fold.tail_start..] {
        byte_offset = emit(ctx, row, byte_offset);
    }
    byte_offset
}

/// The fold's **block**: a blank row, two annotated rows, a blank row.
///
/// # Why a block and not a divider
///
/// The fold is the one region of a command body that is not process output: it
/// states a count, names the reachable verbs, and nothing it contains was printed
/// by the command. A single row wedged between two output rows had to pretend to
/// be output, so it either stretched a fact and an affordance across the full
/// width (a desert of whitespace between them) or stacked them into one crowded
/// line. Padding it above and below and insetting it makes the distinction
/// *structural*: the reader sees a small object floating inside the log, not
/// another log line.
///
/// # What each row says
///
/// ```text
///   ▎ 246 lines hidden (7.1 KB)
///   ▎ press v view full log, press y copy hidden
/// ```
///
/// The first row is the fact (how much is missing, and how big it was). The second
/// is how to act on it, written as a sentence so the two keys read as words — `v`
/// for *view*, `y` for *yank*. The keys are the only bold, high-contrast cells,
/// which is what makes them the thing the eye lands on while scanning.
///
/// # Why the gutter is a mark, not a track
///
/// Both rows carry the same one-column mark so they read as one object, and the
/// mark stops where the block stops. Nothing in the body carries a track: the
/// terminal's own shift-drag selection cannot tell chrome from content, so a `│`
/// in front of every stdout row meant every copied line arrived with a border
/// glued to it (ADR-0150).
///
/// # Degradation
///
/// Both rows degrade by *choosing a shorter whole rendering*, never by cutting a
/// fragment. A count is the longest form that fits (`246 lines hidden (7.1 KB)` →
/// `246 lines hidden` → `246 hidden` → nothing); the verb row walks a ladder from
/// the full two-clause sentence down to the bare key, and a clause is never cut
/// mid-word. A row that printed `12 lines hidde` or `press v view f` would be
/// asserting something false about the log or the key, which is the class of
/// defect this whole component was rebuilt to remove.
///
/// # Hit targets
///
/// Each verb owns its own region (see [`TOOL_FOLD_BLOCK_IDX`] /
/// [`TOOL_FOLD_COPY_BLOCK_IDX`]): one region per affordance, because a row-wide
/// target could not tell "copy hidden" from "view full log". The region covers
/// the clause the reader perceives as the button, not just its key letter.
fn emit_fold_block(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    body: &CommandBodyCtx<'_>,
    fold: &BashFold,
    hidden_rows: &[String],
) {
    let inset = body.indent + BASH_FOLD_INSET_COLS;
    let glyph = ctx.theme.command_fold_gutter_glyph();
    // No band, no frame: the block sits on the same code surface as the rows it
    // interrupts, and is told apart by three things only — the blank rows above
    // and below, the inset, and the gutter mark. A second background tier here
    // would be decoration competing with the transcript's own surface ladder.
    let fill = Style::default().bg(body.surface);
    let gutter_style = fill.fg(ctx.theme.command_fold_gutter());
    let count_style = fill.fg(ctx.theme.code_text());
    let hint_style = fill.fg(ctx.theme.muted());
    let key_style = fill
        .fg(ctx.theme.keycap_accent())
        .add_modifier(Modifier::BOLD);
    // `gutter + gap` on both text rows: the block's text starts one column past
    // its mark, so the two rows and the blank rows above/below share one edge.
    let text_col = inset + glyph.width() + 1;
    let space = ctx.full_width.saturating_sub(text_col);

    // Blank row above: real estate, not a region. It separates the fold from the
    // head rows so a shift-drag over the head can never include it.
    draw_blank_rows(ctx, fill, 1);

    // ---- Fact row -----------------------------------------------------------
    let omitted_kb = fold.omitted_bytes as f64 / 1024.0;
    let count = fold_count_for_budget(fold.hidden, omitted_kb, space);
    let mut stats_row = vec![
        Span::styled(" ".repeat(inset), fill),
        Span::styled(glyph.to_string(), gutter_style),
        Span::styled(" ", fill),
        Span::styled(count.clone(), count_style),
    ];
    let mut used = text_col + count.width();
    // `[INV-FOLD-01]`: a fold that hides a failure site must say so, or the
    // diagnosis becomes unreachable behind a bare count. The signature rides on
    // the fact row — never the verb row — so it never competes with the keys.
    if let Some(signature) = first_salient_signature(hidden_rows) {
        let budget = ctx
            .full_width
            .saturating_sub(used)
            .saturating_sub(JOIN_CAUSE.width());
        if let Some(shown) = fold_signature_for_budget(&signature, budget) {
            used += JOIN_CAUSE.width() + shown.width();
            stats_row.push(Span::styled(JOIN_CAUSE, gutter_style));
            stats_row.push(Span::styled(shown, fill.fg(ctx.theme.warn())));
        }
    }
    stats_row.push(Span::styled(padded_tail(ctx.full_width, used), fill));
    // The fact row is chrome: it registers nothing, so no selection can capture a
    // sentence the process never printed.
    ctx.paint(Line::from(stats_row));

    // ---- Verb row -----------------------------------------------------------
    let clauses = FOLD_VERB_CLAUSES;
    // The ladder, most informative first. Every rung is a whole rendering, so a
    // clause can never be painted as `press v view f` — a promise of a verb the
    // row then fails to name. It bottoms out at the bare key, which is still the
    // one cell the sentence exists to make findable.
    let ladder = [
        (VerbTerse::Sentence, 2),
        (VerbTerse::Compact, 2),
        (VerbTerse::Sentence, 1),
        (VerbTerse::Compact, 1),
        (VerbTerse::KeyOnly, 2),
        (VerbTerse::KeyOnly, 1),
    ];
    let room = ctx.full_width.saturating_sub(text_col);
    let chosen = ladder
        .into_iter()
        .find(|&(terse, keep)| fold_verbs_width(&clauses[..keep], terse) <= room);

    let mut spans = vec![
        Span::styled(" ".repeat(inset), fill),
        Span::styled(glyph.to_string(), gutter_style),
        Span::styled(" ", fill),
    ];
    used = text_col;
    let mut hit_spans: Vec<(usize, (u16, u16))> = Vec::with_capacity(2);
    if let Some((terse, keep)) = chosen {
        for (idx, clause) in clauses[..keep].iter().enumerate() {
            if idx > 0 {
                spans.push(Span::styled(VERB_JOIN.to_string(), hint_style));
                used += VERB_JOIN.width();
            }
            let (before, after) = clause.connectives(terse);
            let start = used;
            if !before.is_empty() {
                spans.push(Span::styled(before.to_string(), hint_style));
                used += before.width();
            }
            spans.push(Span::styled(clause.key.to_string(), key_style));
            used += clause.key.width();
            spans.push(Span::styled(after.to_string(), hint_style));
            used += after.width();
            hit_spans.push((clause.sentinel(), (start as u16, used as u16)));
        }
    }
    spans.push(Span::styled(padded_tail(ctx.full_width, used), fill));
    if let Some(rect) = ctx.paint(Line::from(spans)) {
        // One region per clause, pushed in reverse so `region_at`'s first-match
        // walk still resolves each clause at any column inside it.
        for (sentinel, (start, end)) in hit_spans.into_iter().rev() {
            ctx.layout_map.push(BlockRegion {
                message_idx: mi,
                block_idx: sentinel,
                start_byte: 0,
                end_byte: 0,
                text: String::new(),
                prefix_cols: 0,
                rect: Rect::new(rect.x + start, rect.y, end.saturating_sub(start), 1),
                hidden_ranges: Vec::new(),
            });
        }
    }

    // Blank row below: symmetric with the row above, so the block is inset on all
    // four sides and the tail rows resume on a clean edge.
    draw_blank_rows(ctx, fill, 1);
    // Both text rows are laid out whole, so the block's painted height is exactly
    // `BASH_FOLD_CHROME_ROWS` and the compile-time assertions on the fold geometry
    // keep holding.
}

/// The folded count, in the most specific form that fits `space` columns.
///
/// Chosen rather than truncated: `12 lines hidde` is a wrong number, and a
/// half-drawn `(` is a syntax error on screen. When nothing fits the row is left
/// with its gutter mark alone, which still says "something is here" without
/// asserting a figure it cannot show.
fn fold_count_for_budget(hidden: usize, omitted_kb: f64, space: usize) -> String {
    for candidate in [
        format!("{hidden} lines hidden ({omitted_kb:.1} KB)"),
        format!("{hidden} lines hidden"),
        format!("{hidden} hidden"),
        format!("{hidden}"),
    ] {
        if candidate.width() <= space {
            return candidate;
        }
    }
    String::new()
}

/// One "press `key` …" clause of the fold block's verb sentence.
///
/// The pieces are kept apart because they are painted in two tones: the
/// connectives are muted, the key is the bold cell the eye is meant to catch.
/// Spelling the sentence as data is also what lets the row drop whole clauses
/// under width pressure instead of cutting mid-word.
///
/// `compact_*` is the same clause with the polite connective removed, used only
/// when the full sentence cannot fit — so a narrow block still *names* its verb
/// rather than being cut to `press v view f`.
#[derive(Clone, Copy)]
struct VerbClause {
    key: &'static str,
    before: &'static str,
    after: &'static str,
    compact_after: &'static str,
}

/// How much of a clause is painted. Ordered from most to least informative, and
/// every level is a *complete* rendering — which is why this is an enum and not a
/// width budget for `truncate_to_width`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VerbTerse {
    /// `press v view full log`
    Sentence,
    /// `v view full log`
    Compact,
    /// `v`
    KeyOnly,
}

impl VerbClause {
    fn connectives(self, terse: VerbTerse) -> (&'static str, &'static str) {
        match terse {
            VerbTerse::Sentence => (self.before, self.after),
            VerbTerse::Compact => ("", self.compact_after),
            VerbTerse::KeyOnly => ("", ""),
        }
    }
    /// Which click sentinel this clause owns. One place, so the sentence and the
    /// hit target cannot disagree about which verb a clause names.
    fn sentinel(self) -> usize {
        if self.key == VIEW_LOG_KEY {
            TOOL_FOLD_BLOCK_IDX
        } else {
            TOOL_FOLD_COPY_BLOCK_IDX
        }
    }
}

/// Columns `clauses` occupy when painted as one row.
fn fold_verbs_width(clauses: &[VerbClause], terse: VerbTerse) -> usize {
    clauses
        .iter()
        .map(|c| {
            let (before, after) = c.connectives(terse);
            before.width() + c.key.width() + after.width()
        })
        .sum::<usize>()
        + VERB_JOIN.width() * clauses.len().saturating_sub(1)
}

/// Separator between two verb clauses: a comma and one space, because the row
/// reads as a sentence ("press v …, press y …") rather than as a legend.
const VERB_JOIN: &str = ", ";

/// The fold block's verbs, in priority order.
///
/// `v` (read the whole log) leads because it is the reason the block exists; `y`
/// is the shortcut and is the one that yields under pressure.
const FOLD_VERB_CLAUSES: [VerbClause; 2] = [
    VerbClause {
        key: VIEW_LOG_KEY,
        before: "press ",
        after: " view full log",
        compact_after: " view full log",
    },
    VerbClause {
        key: COPY_LOG_KEY,
        before: "press ",
        after: " copy hidden",
        compact_after: " copy hidden",
    },
];

/// The failure signature as it may be shown in `budget` columns, or `None` when
/// it cannot be shown honestly.
///
/// A signature is a machine token followed by prose (`error[E0425]: cannot find
/// value …`, `panicked at src/main.rs:9`). Cutting it at a *column* boundary
/// produces a different machine token — `error[E042…` is not an error code, and
/// `panicked at src/ma` names no file — so the only faithful renderings are
/// whole-token ones:
///
/// - the whole signature, when it fits;
/// - as many whole tokens as fit, then `…` (`error[E0425]: cannot find…`);
/// - nothing at all, when not even the first token fits.
///
/// The first token is the floor because it is the part a reader greps for. When
/// it cannot fit, the caller prints the count alone — honest — and the verb one
/// row below reaches the full text.
///
/// Working in tokens rather than columns is the whole point: `unicode-width` can
/// tell us where a glyph ends but not where an identifier ends, and only the
/// latter is safe to cut.
pub(crate) fn fold_signature_for_budget(signature: &str, budget: usize) -> Option<String> {
    if budget < BASH_FOLD_SIGNATURE_MIN_COLS {
        return None;
    }
    if signature.width() <= budget {
        return Some(signature.to_string());
    }
    let marker = '…';
    let mut tokens = signature.split_whitespace();
    // One column is reserved for the marker on every truncated rendering.
    let mut shown = tokens.next()?.to_string();
    if shown.width() + 1 > budget {
        return None;
    }
    for token in tokens {
        if shown.width() + 1 + token.width() + 1 > budget {
            break;
        }
        shown.push(' ');
        shown.push_str(token);
    }
    shown.push(marker);
    Some(shown)
}

/// First salient failure signature among the folded rows, if any.
///
/// Shares `nuo-tool`'s scanner with the model-facing fold so the human and the
/// model are pointed at the same line.
pub(crate) fn first_salient_signature(hidden_rows: &[String]) -> Option<String> {
    let omitted = hidden_rows.join("\n");
    nuo_tool::vfs::extract_salient_signatures(&omitted, 1)
        .into_iter()
        .next()
        .map(|(_, signature)| signature)
}

/// Emit a (possibly multi-line) bash body section at `indent`, wrapping to
/// `wrap_w`, all rows in `style`, anchoring selection byte ranges at
/// `*byte_offset` (advanced past the section).
///
/// Used by the renderers whose body is a plain text run rather than a derived
/// output view (tool errors, patch warnings). Command bodies go through
/// [`CommandBodyCtx`], which also owns the prompt row; both share the same
/// flush, unmarked row shape.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_command_lines(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    indent: usize,
    wrap_w: usize,
    pad: Style,
    sel_range: Option<(usize, Option<usize>)>,
    text: &str,
    style: Style,
    mut byte_offset: usize,
) -> usize {
    let text = text.trim_end_matches(&['\r', '\n'][..]);
    for logical_line in text.split('\n') {
        let logical_line = nuo_tool::normalize_carriage_returns(logical_line);
        let wrapped = nonempty_wrapped(wrap_text(&logical_line, wrap_w));
        for wl in &wrapped {
            let block_wl = WrappedLine {
                text: wl.text.clone(),
                start_byte: byte_offset + wl.start_byte,
                end_byte: byte_offset + wl.end_byte,
            };
            let mut line = line_spans(
                "",
                pad,
                &wl.text,
                line_selection(sel_range, &block_wl),
                style,
                ctx.theme.selected(),
            );
            line.spans.insert(0, Span::styled(" ".repeat(indent), pad));
            let used = indent + wl.text.width();
            line.spans
                .push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint_text_row(line, mi, block_idx, &block_wl, indent as u16, &[]);
        }
        byte_offset += logical_line.len() + 1;
    }
    byte_offset
}

/// Resolve the shell command for a `bash` step: prefer the structured
/// [`ToolOutput::Shell`](nuo_tool::ToolOutput) payload (set as soon as the
/// call starts, so it is available even while streaming), falling back to
/// parsing the JSON arguments for legacy / restored sessions without a
/// structured payload.
pub(crate) fn command_for(structured: Option<&nuo_tool::ToolOutput>, arguments: &str) -> String {
    if let Some(nuo_tool::ToolOutput::Shell { command, .. }) = structured
        && !command.is_empty()
    {
        return command.clone();
    }
    nuo_view::model::document::parse_arguments_kv(arguments)
        .iter()
        .find(|(k, _)| k == "command")
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}
