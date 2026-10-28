//! Pure derivation of a command step's output view (ADR-0115).
//!
//! Three audiences read a command's output: the model (`nuo-tool`'s
//! `[INV-FOLD-01]` text projection), the human (this crate's fold geometry),
//! and the clipboard. All three must agree on **what rows exist** and **whether
//! the payload in hand is complete**, so the derivation lives here — once —
//! instead of being re-implemented per consumer.
//!
//! This module owns no styling and no drawing: it answers *which* logical rows
//! the body has, what the exit/termination outcome is, and what each audience
//! holds. Renderers attach styles and paint; the model-side projection keeps
//! its own byte-budgeted folding in `nuo-tool`.

use nuo_ui_primitives::design::{BASH_FOLD_HEAD_ROWS, BASH_FOLD_TAIL_ROWS, bash_fold_applies};
use nuo_wire::ToolOutput;
use nuo_wire::tool_output::{ShellTermination, normalize_carriage_returns};

use super::ResultKind;

/// What each audience holds for a command step's output.
///
/// The distinction cannot be inferred from row counts: a step can be complete
/// for the screen and truncated for the model, and a restored session can carry
/// a payload that was already cut before any renderer saw it. Collapsing those
/// two facts into a single "truncated" bool is what used to let the footer
/// claim "partial kept" over a body nothing had cut.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputCoverage {
    /// The UI holds the whole log and so did the model.
    Full,
    /// The UI holds the whole log; the model's copy was cut at the context
    /// watermark (`ToolOutput::Shell::truncated`). Nothing is missing here —
    /// folding is a viewport, and the fold can be revealed.
    ModelTruncated,
    /// The payload arrived already cut (restored session, or a step recorded
    /// before structured payloads). The rows below are a head/tail excerpt of
    /// the original output — the middle is genuinely gone.
    CutBeforeUi,
}

/// Where a folded body's visible window sits and what it hides.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BashFold {
    /// Number of leading rows kept visible (`0..head_end`).
    pub head_end: usize,
    /// Index of the first trailing row kept visible (`tail_start..len`).
    pub tail_start: usize,
    /// How many logical rows sit between the two windows.
    pub hidden: usize,
    /// Bytes of the hidden slice (newline-inclusive), for the `N KB` label.
    pub omitted_bytes: usize,
}

impl BashFold {
    /// The hidden rows of `rows`, in order. Always non-empty for a real fold.
    pub fn hidden_rows<'a>(&self, rows: &'a [String]) -> &'a [String] {
        &rows[self.head_end..self.tail_start]
    }
}

/// A command step's output, derived from its structured payload or — for
/// restored / pre-structured steps — from the flat text the model was given.
#[derive(Clone, Debug)]
pub struct ShellOutput {
    /// Logical output rows, carriage-return/backspace normalized, in paint
    /// order. One entry per row `emit`-style renderers should draw.
    pub rows: Vec<String>,
    /// The parsed exit status, ignoring the synthetic `-1` sentinels older
    /// payloads injected for "no status".
    pub exit: Option<i32>,
    /// Why the step ended.
    pub termination: ShellTermination,
    /// What each audience holds.
    pub coverage: OutputCoverage,
}

impl ShellOutput {
    /// Derive the output view for a command step.
    ///
    /// `content` is the flat model-facing text (the legacy/restore fallback and
    /// the only source before a structured result lands); `structured` wins
    /// whenever it carries a [`ToolOutput::Shell`], because its interleaved
    /// `lines` are the UI-authoritative arrival order and it has not been cut.
    pub fn derive(content: &str, structured: Option<&ToolOutput>) -> Self {
        match structured {
            Some(ToolOutput::Shell {
                stdout,
                stderr,
                lines,
                exit,
                truncated,
                termination,
                ..
            }) => Self {
                rows: structured_rows(lines, stdout, stderr),
                exit: *exit,
                termination: *termination,
                // Structured fields are *not* pre-cut (ADR-0069): `truncated` is
                // a model-context size hint, so the screen holds everything.
                coverage: if *truncated {
                    OutputCoverage::ModelTruncated
                } else {
                    OutputCoverage::Full
                },
            },
            _ => Self::from_flat_text(content),
        }
    }

    /// Parse the flat, model-facing text: strip the in-band markers earlier
    /// topologies stamped into it, recover the exit/termination facts those
    /// markers carried, and record that a truncation marker means the text
    /// itself was cut before it reached us.
    pub fn from_flat_text(content: &str) -> Self {
        let mut rows: Vec<String> = Vec::new();
        let mut exit = None;
        let mut termination = ShellTermination::Exited;
        let mut cut = false;
        // Markers are stripped from the stream, but the blank separator rows
        // that surround them are not content: the model-facing text puts a
        // `\n\n` before the `[killed by harness: …]` note and before the
        // truncation banner. Consumers paint the outcome footer flush beneath
        // the output, so a surviving blank row would open a phantom gap between
        // the output and the footer that explains it. Defer blanks: emit one
        // only once a real content row follows it.
        let mut pending_blank = false;

        for raw_line in content.trim_end_matches(&['\r', '\n'][..]).lines() {
            let trimmed = raw_line.trim_end();
            let lead = trimmed.trim_start();

            // 1. Stream band headers carry no output; the bands themselves are
            //    interleaved into `lines` on the structured path.
            if lead == "STDOUT:" || lead == "STDERR:" || lead.starts_with("(success, stderr):") {
                continue;
            }

            // 2. Truncation / offload envelopes. Their presence *is* the
            //    evidence that this text was cut before it reached the UI.
            if is_truncation_marker(lead) {
                cut = true;
                continue;
            }

            // 3. Harness governance notes: recover the termination cause the
            //    note describes, then drop the prose (the footer restates it).
            if let Some(recovered) = termination_from_note(lead) {
                termination = recovered;
                continue;
            }

            // 4. Exit status, re-materialized by the footer. `exit -1` is the
            //    synthetic "no status" sentinel older payloads emitted.
            if let Some(code) = parse_exit_line(lead) {
                if code != -1 {
                    exit = Some(code);
                }
                continue;
            }

            // 5. Authentic output row. Genuine blank rows *inside* the output
            //    stay — they are the command's own formatting.
            if trimmed.is_empty() {
                pending_blank = !rows.is_empty();
                continue;
            }
            if pending_blank {
                rows.push(String::new());
                pending_blank = false;
            }
            // A restored / pre-structured payload can still carry raw `\r` / `\b`
            // from a progress bar. Resolve them here — the derivation is the one
            // place that decides what a row *is*, so the fold's byte accounting,
            // the painter, and the clipboard all measure the same bytes.
            rows.push(normalize_carriage_returns(trimmed));
        }

        Self {
            rows,
            exit,
            termination,
            coverage: if cut {
                OutputCoverage::CutBeforeUi
            } else {
                OutputCoverage::Full
            },
        }
    }

    /// The fold the expanded body would apply, or `None` when the body renders
    /// verbatim (see [`bash_fold_applies`]).
    pub fn fold(&self) -> Option<BashFold> {
        let total = self.rows.len();
        if !bash_fold_applies(total) {
            return None;
        }
        let head_end = BASH_FOLD_HEAD_ROWS;
        let tail_start = total - BASH_FOLD_TAIL_ROWS;
        let omitted_bytes = self.rows[head_end..tail_start]
            .iter()
            .map(|row| row.len() + 1)
            .sum();
        Some(BashFold {
            head_end,
            tail_start,
            hidden: tail_start - head_end,
            omitted_bytes,
        })
    }
}

/// The fold a command step's **expanded** body would hide, or `None`.
///
/// This is the single source of truth shared by the renderer (what to paint)
/// and the activation ladder in
/// [`TranscriptMessage::toggle_expanded`](crate::model::document::TranscriptMessage),
/// which must know *exactly* whether activating the step again would reveal
/// anything — a rung that reveals nothing is a dead key press.
///
/// The guards mirror the body dispatch in the renderer: only steps classified
/// as [`ResultKind::Command`] get a terminal-session body, and explicit
/// `Error` / `PermissionDenied` results render as errors rather than as output.
pub fn command_step_fold(
    name: &str,
    output: Option<&str>,
    structured: Option<&ToolOutput>,
) -> Option<BashFold> {
    if !is_command_body(name, structured) {
        return None;
    }
    ShellOutput::derive(output.unwrap_or(""), structured).fold()
}

/// Whether a step renders a command body at all (and therefore can fold).
pub fn is_command_body(name: &str, structured: Option<&ToolOutput>) -> bool {
    if matches!(
        structured,
        Some(ToolOutput::Error { .. } | ToolOutput::PermissionDenied { .. })
    ) {
        return false;
    }
    super::presenter_for(name).result_kind() == ResultKind::Command
}

/// Materialize a structured `Shell` result's rows.
///
/// Prefers the arrival-ordered `lines` (interleaved stdout/stderr, the
/// UI-authoritative order); falls back to the all-stdout-then-all-stderr flat
/// strings for the live-seed window before the final result lands and for
/// restored sessions recorded without `lines`.
fn structured_rows(
    lines: &[nuo_wire::tool_output::ShellLine],
    stdout: &str,
    stderr: &str,
) -> Vec<String> {
    if !lines.is_empty() {
        return lines
            .iter()
            .map(|line| normalize_carriage_returns(&line.text))
            .collect();
    }
    let mut rows = Vec::new();
    for band in [stdout, stderr] {
        let band = band.trim_end_matches(&['\r', '\n'][..]);
        if band.is_empty() {
            continue;
        }
        rows.extend(
            band.split('\n')
                .map(|line| normalize_carriage_returns(line.trim_end())),
        );
    }
    rows
}

/// Leading parenthesis of every truncation / offload envelope this pipeline has
/// ever stamped into a command result (ADR-0057, -0069, -0070, -0093).
fn is_truncation_marker(line: &str) -> bool {
    const MARKERS: &[&str] = &[
        "[Output truncated",
        "[output truncated",
        "[stream truncated",
        "⋯ [Claim-Check Stub:",
        "--- [Middle",
        "[Output was large",
        "[Full raw output saved to:",
        "[Full un-truncated output spooled to:",
        "- Spool File:",
        "- Virtual Path:",
        "[Guidance:",
    ];
    MARKERS.iter().any(|marker| line.starts_with(marker))
}

/// Recover a [`ShellTermination`] from a harness governance note, or `None` when
/// the row is not one.
fn termination_from_note(line: &str) -> Option<ShellTermination> {
    if line.starts_with("[process blocked]")
        || line.starts_with("[terminated by harness: interactive")
        || line.starts_with("[killed by harness: interactive")
    {
        return Some(ShellTermination::InteractiveBlocked);
    }
    if line.starts_with("[process detached]") {
        return Some(ShellTermination::Detached);
    }
    if !(line.starts_with("[terminated by harness:")
        || line.starts_with("[killed by harness:")
        || line.starts_with("[process terminated]")
        || line.starts_with("[process killed]"))
    {
        return None;
    }
    let lower = line.to_ascii_lowercase();
    Some(
        if lower.contains("timeout") || lower.contains("wall-clock") {
            ShellTermination::Timeout
        } else if lower.contains("stream")
            || lower.contains("budget")
            || lower.contains("output limit")
        {
            ShellTermination::StreamGuard
        } else if lower.contains("cancel") {
            ShellTermination::Cancelled
        } else if lower.contains("idle") {
            ShellTermination::IdleBlocked
        } else {
            ShellTermination::Timeout
        },
    )
}

/// Parse an `exit N` / `Exit N` row into its status.
fn parse_exit_line(line: &str) -> Option<i32> {
    let rest = line
        .strip_prefix("Exit ")
        .or_else(|| line.strip_prefix("exit "))?;
    rest.trim().parse::<i32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_ui_primitives::design::BASH_FOLD_MIN_VERBATIM_ROWS;
    use nuo_wire::tool_output::{ShellLine, ShellStream};

    fn shell(stdout: &str, stderr: &str, lines: Vec<ShellLine>, exit: Option<i32>) -> ToolOutput {
        ToolOutput::Shell {
            command: "cargo build".into(),
            stdout: stdout.into(),
            stderr: stderr.into(),
            lines,
            exit,
            truncated: false,
            truncated_note: None,
            termination: ShellTermination::Exited,
            detached_job_id: None,
        }
    }

    fn line(stream: ShellStream, text: &str) -> ShellLine {
        ShellLine {
            stream,
            text: text.into(),
        }
    }

    fn numbered(rows: usize) -> String {
        (1..=rows)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn structured_lines_win_over_flat_bands() {
        let out = shell(
            "band-only",
            "",
            vec![
                line(ShellStream::Out, "first"),
                line(ShellStream::Err, "second"),
            ],
            Some(2),
        );
        let view = ShellOutput::derive("ignored", Some(&out));
        assert_eq!(view.rows, vec!["first".to_string(), "second".to_string()]);
        assert_eq!(view.exit, Some(2));
        assert_eq!(view.coverage, OutputCoverage::Full);
    }

    #[test]
    fn flat_bands_seed_the_rows_before_a_result_lands() {
        let out = shell("a\nb\n", "e\n", Vec::new(), None);
        let view = ShellOutput::derive("", Some(&out));
        assert_eq!(view.rows, vec!["a".to_string(), "b".into(), "e".into()]);
    }

    #[test]
    fn truncated_structured_output_still_holds_every_row() {
        let mut out = shell("", "", Vec::new(), Some(0));
        if let ToolOutput::Shell {
            stdout, truncated, ..
        } = &mut out
        {
            *stdout = numbered(400);
            *truncated = true;
        }
        let view = ShellOutput::derive("", Some(&out));
        assert_eq!(view.rows.len(), 400, "the screen keeps the whole log");
        assert_eq!(view.coverage, OutputCoverage::ModelTruncated);
    }

    #[test]
    fn flat_text_markers_are_stripped_and_their_facts_recovered() {
        let content = "[Output truncated: 5509 chars total; full raw log spilled to disk]\n\
                       STDOUT:\n\
                       building\n\
                       error: linker `lld` not found\n\
                       \n\
                       [killed by harness: wall-clock timeout]\n\
                       exit 101\n";
        let view = ShellOutput::from_flat_text(content);
        assert_eq!(
            view.rows,
            vec![
                "building".to_string(),
                "error: linker `lld` not found".into()
            ],
            "markers and their separator blanks are not output",
        );
        assert_eq!(view.exit, Some(101));
        assert_eq!(view.termination, ShellTermination::Timeout);
        assert_eq!(view.coverage, OutputCoverage::CutBeforeUi);
    }

    /// A restored payload can carry raw `\r` / `\b` from a progress bar. The
    /// derivation resolves them with terminal *overwrite* semantics (the cursor
    /// returns to the line start, so a repainting progress bar collapses to its
    /// final frame) — once, so the fold's byte accounting, the painter, and the
    /// clipboard all measure the same bytes.
    #[test]
    fn flat_text_resolves_carriage_return_progress_bars() {
        let view = ShellOutput::from_flat_text("step 1\rstep 2\rdone\nback\x08space\n");
        assert_eq!(
            view.rows,
            vec!["done 2".to_string(), "bacspace".into()],
            "a repainted line keeps its final frame, and a backspace steps back",
        );
    }

    #[test]
    fn flat_text_without_markers_is_complete() {
        let view = ShellOutput::from_flat_text("one\ntwo\nexit 0\n");
        assert_eq!(view.rows, vec!["one".to_string(), "two".into()]);
        assert_eq!(view.exit, Some(0));
        assert_eq!(view.coverage, OutputCoverage::Full);
    }

    #[test]
    fn synthetic_negative_one_exit_is_ignored() {
        let view = ShellOutput::from_flat_text("out\nexit -1\n");
        assert_eq!(view.rows, vec!["out".to_string()]);
        assert_eq!(view.exit, None);
    }

    #[test]
    fn fold_is_tail_heavy_and_lossless_at_the_boundary() {
        let view = ShellOutput::from_flat_text(&numbered(BASH_FOLD_MIN_VERBATIM_ROWS));
        assert_eq!(
            view.fold(),
            None,
            "folding at the boundary would render as many rows as it hides",
        );

        let rows = BASH_FOLD_MIN_VERBATIM_ROWS + 1;
        let view = ShellOutput::from_flat_text(&numbered(rows));
        let fold = view.fold().expect("one row past the boundary must fold");
        assert_eq!(fold.head_end, BASH_FOLD_HEAD_ROWS);
        assert_eq!(fold.tail_start, rows - BASH_FOLD_TAIL_ROWS);
        assert_eq!(
            fold.hidden,
            rows - BASH_FOLD_HEAD_ROWS - BASH_FOLD_TAIL_ROWS
        );
        assert_eq!(
            fold.hidden_rows(&view.rows),
            (BASH_FOLD_HEAD_ROWS + 1..=fold.tail_start)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .as_slice(),
        );
    }

    #[test]
    fn command_step_fold_ignores_non_command_and_error_steps() {
        assert!(command_step_fold("read_text", Some(&numbered(50)), None).is_none());
        let err = ToolOutput::Error {
            message: numbered(50),
            detail: None,
        };
        assert!(command_step_fold("execute_command", Some(&numbered(50)), Some(&err)).is_none());
        assert!(command_step_fold("execute_command", Some(&numbered(50)), None).is_some());
    }

    /// The activation ladder and the renderer must agree on folded-ness, or a
    /// key press that promises to reveal would reveal nothing.
    #[test]
    fn ladder_and_renderer_agree_on_folded_ness() {
        for rows in 0..(BASH_FOLD_MIN_VERBATIM_ROWS + 4) {
            let text = numbered(rows);
            let via_renderer = ShellOutput::derive(&text, None).fold().is_some();
            let via_ladder = command_step_fold("execute_command", Some(&text), None).is_some();
            assert_eq!(via_renderer, via_ladder, "{rows} rows");
        }
    }
}
