//! Question / user clarification prompt renderer.
use nuotc::{Line, Modifier, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::shell::disclosure::renderers::base::{RenderCtx, nonempty_wrapped};
use nuo_ui_primitives::design::{QUESTION_ANSWER_GAP_COLS, QUESTION_ANSWER_GLYPH};
use nuo_view::model::selection::SelectionState;
use nuo_view::text_layout::{
    WrappedLine, block_selection_range, line_selection, line_spans, padded_tail, wrap_text,
};

/// One question parsed out of an `ask_user` call's arguments, reduced to the
/// fields the expanded body paints. Mirrors the wire `UserQuestion` shape but is
/// parsed defensively from JSON so a restored / partially-persisted call still
/// renders (a missing field degrades, never panics).
struct QuestionSpec {
    header: Option<String>,
    question: String,
    option_count: usize,
    multi_select: bool,
}

/// Parse the `questions` array out of an `ask_user` call's raw arguments.
/// Entries without a `question` string are dropped; malformed JSON yields an
/// empty list (the caller then falls back to the raw result text).
fn parse_question_specs(arguments: &str) -> Vec<QuestionSpec> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return Vec::new();
    };
    let Some(questions) = value.get("questions").and_then(|q| q.as_array()) else {
        return Vec::new();
    };
    questions
        .iter()
        .filter_map(|q| {
            let question = q.get("question").and_then(|v| v.as_str())?;
            Some(QuestionSpec {
                header: q
                    .get("header")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .map(str::to_string),
                question: question.to_string(),
                option_count: q
                    .get("options")
                    .and_then(|v| v.as_array())
                    .map(|o| o.len())
                    .unwrap_or(0),
                multi_select: q
                    .get("multi_select")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            })
        })
        .collect()
}

/// Recover the recorded selection from the tool's result text, or `None` when
/// the request was cancelled (or the result is not an answer payload at all).
///
/// The harness returns the answers as a pretty JSON array-of-arrays, either
/// bare or behind prose ("User answered the question(s). Selected option
/// labels:", or the autonomous "[answered by policy, not by user]" framing).
/// Try the whole string first, then the trailing JSON array — so an answered
/// step renders its selections regardless of which framing produced it.
fn parse_question_answers(output: &str) -> Option<Vec<Vec<String>>> {
    let trimmed = output.trim();
    if let Ok(answers) = serde_json::from_str::<Vec<Vec<String>>>(trimmed) {
        return Some(answers);
    }
    let idx = trimmed.find('[')?;
    serde_json::from_str::<Vec<Vec<String>>>(trimmed[idx..].trim()).ok()
}

/// Emit one logical (already-parsed) row of the question/answer list at
/// `indent` under `style`, wrapping text to `wrap_w` columns (the caller passes
/// the row's own content width — `inner_w` for a flush row, narrowed for an
/// indented continuation answer). When `meta` is provided it is appended,
/// dimmed, to the last wrapped row if it fits within the full row width;
/// otherwise it drops to its own dim row so the tag is never clipped at the
/// right edge.
///
/// Each painted row records a [`BlockRegion`] anchored in the block's *logical*
/// text via `*offset` (advanced by every row's text so a selection spanning the
/// list copies the questions and answers in reading order). The `meta`
/// decoration stays outside the recorded range, like the bash `$ command`
/// prompt line.
#[allow(clippy::too_many_arguments)]
fn emit_question_text(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    indent: usize,
    wrap_w: usize,
    text: &str,
    style: Style,
    meta: Option<&str>,
    meta_style: Style,
    sel_range: Option<(usize, Option<usize>)>,
    offset: &mut usize,
) {
    let bg = style.bg;
    let pad = Style::default().bg(bg);
    let wrapped = nonempty_wrapped(wrap_text(text, wrap_w.max(1)));
    let last = wrapped.len().saturating_sub(1);
    let mut deferred_meta: Option<String> = None;
    for (i, wl) in wrapped.iter().enumerate() {
        let block_wl = WrappedLine {
            text: wl.text.clone(),
            start_byte: *offset + wl.start_byte,
            end_byte: *offset + wl.end_byte,
        };
        let mut line = line_spans(
            &" ".repeat(indent),
            pad,
            &wl.text,
            line_selection(sel_range, &block_wl),
            style,
            ctx.theme.selected(),
        );
        let mut used = indent + wl.text.width();
        if i == last
            && let Some(m) = meta
        {
            if used + 1 + m.width() <= ctx.full_width {
                line.spans.push(Span::styled(" ".to_string(), meta_style));
                line.spans.push(Span::styled(m.to_string(), meta_style));
                used += 1 + m.width();
            } else {
                deferred_meta = Some(m.to_string());
            }
        }
        line.spans
            .push(Span::styled(padded_tail(ctx.full_width, used), pad));
        ctx.paint_text_row(line, mi, block_idx, &block_wl, indent as u16, &[]);
    }
    *offset += text.len() + 1;

    if let Some(m) = deferred_meta {
        let mw = m.width();
        let wl = WrappedLine {
            text: m.clone(),
            start_byte: *offset,
            end_byte: *offset,
        };
        let line = Line::from(vec![
            Span::styled(" ".repeat(indent), pad),
            Span::styled(m, meta_style),
            Span::styled(padded_tail(ctx.full_width, indent + mw), pad),
        ]);
        ctx.paint_text_row(line, mi, block_idx, &wl, indent as u16, &[]);
    }
}

/// Render an expanded `ask_user` step as a question→answer list.
///
/// The old path let the step fall through to the generic code renderer, which
/// dumped the harness's answer JSON as an unreadable line-numbered blob and
/// never showed the questions at all. Here the questions are recovered from the
/// call's `arguments` (header chip, text, option count, multi-select flag) and
/// re-paired with the recorded selection from the result (`output`), so the
/// reader sees *what was asked* and *what was chosen* — and a cancelled request
/// reads as `↳ cancelled — no answer` rather than an empty array.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_questions_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    output: &str,
    arguments: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(code_bg);
    let sel_range = block_selection_range(selection, mi, block_idx);
    // Rows at `indent` wrap to `inner_w` (the caller's content width); an
    // indented continuation answer wraps to whatever that indent leaves.
    let content_w = inner_w.max(1);

    let questions = parse_question_specs(arguments);
    let answers = parse_question_answers(output);

    // Legacy / truncated arguments (no parseable questions): render the raw
    // result text so the step is never blank instead of silently empty.
    if questions.is_empty() {
        let fallback = Style::default().bg(code_bg).fg(ctx.theme.muted());
        let text = output.trim();
        let text = if text.is_empty() {
            "(no question recorded)"
        } else {
            text
        };
        let mut offset = 0usize;
        emit_question_text(
            ctx,
            mi,
            block_idx,
            indent,
            content_w,
            text,
            fallback,
            None,
            fallback,
            sel_range,
            &mut offset,
        );
        return;
    }

    let header_style = Style::default()
        .bg(code_bg)
        .fg(ctx.theme.info())
        .add_modifier(Modifier::BOLD);
    let question_style = Style::default().bg(code_bg).fg(ctx.theme.fg());
    let meta_style = Style::default().bg(code_bg).fg(ctx.theme.dim());
    let answer_style = Style::default().bg(code_bg).fg(ctx.theme.muted());
    let cancel_style = Style::default().bg(code_bg).fg(ctx.theme.warn());

    // A continuation answer (the 2nd+ option of a multi-select) aligns under the
    // first answer's label: the glyph width plus its gap.
    let label_indent = indent + QUESTION_ANSWER_GLYPH.width() + QUESTION_ANSWER_GAP_COLS;
    let label_w = content_w
        .saturating_sub(label_indent.saturating_sub(indent))
        .max(1);

    let policy_answered = output.contains("answered by policy");
    let total = questions.len();
    let mut offset = 0usize;

    for (q_idx, q) in questions.iter().enumerate() {
        if q_idx > 0 {
            ctx.paint(Line::from(Span::styled(
                padded_tail(ctx.full_width, 0),
                pad,
            )));
        }

        if let Some(header) = &q.header {
            emit_question_text(
                ctx,
                mi,
                block_idx,
                indent,
                content_w,
                header,
                header_style,
                None,
                meta_style,
                sel_range,
                &mut offset,
            );
        }

        let meta = format!(
            "({} option{}{})",
            q.option_count,
            if q.option_count == 1 { "" } else { "s" },
            if q.multi_select { ", multi-select" } else { "" }
        );
        emit_question_text(
            ctx,
            mi,
            block_idx,
            indent,
            content_w,
            &q.question,
            question_style,
            Some(&meta),
            meta_style,
            sel_range,
            &mut offset,
        );

        match &answers {
            Some(all) => {
                let labels = all.get(q_idx).map(Vec::as_slice).unwrap_or(&[]);
                if labels.is_empty() {
                    emit_question_text(
                        ctx,
                        mi,
                        block_idx,
                        indent,
                        content_w,
                        "<no option selected>",
                        meta_style,
                        None,
                        meta_style,
                        sel_range,
                        &mut offset,
                    );
                    continue;
                }
                for (i, label) in labels.iter().enumerate() {
                    let (row_indent, row_w, text) = if i == 0 {
                        (
                            indent,
                            content_w,
                            format!("{QUESTION_ANSWER_GLYPH} {label}"),
                        )
                    } else {
                        (label_indent, label_w, label.clone())
                    };
                    emit_question_text(
                        ctx,
                        mi,
                        block_idx,
                        row_indent,
                        row_w,
                        &text,
                        answer_style,
                        None,
                        meta_style,
                        sel_range,
                        &mut offset,
                    );
                }
            }
            None => {
                // Cancelled: one step-level status, shown once after the last
                // question so it reads as "the whole request was dropped".
                if q_idx + 1 == total {
                    emit_question_text(
                        ctx,
                        mi,
                        block_idx,
                        indent,
                        content_w,
                        &format!("{QUESTION_ANSWER_GLYPH} cancelled — no answer"),
                        cancel_style,
                        None,
                        meta_style,
                        sel_range,
                        &mut offset,
                    );
                }
            }
        }
    }

    if policy_answered {
        // Autonomous sessions settle by policy, not by a human — label it so the
        // answers are never mistaken for a real user decision.
        emit_question_text(
            ctx,
            mi,
            block_idx,
            indent,
            content_w,
            "[answered by policy, not by the user]",
            meta_style,
            None,
            meta_style,
            sel_range,
            &mut offset,
        );
    }
}
