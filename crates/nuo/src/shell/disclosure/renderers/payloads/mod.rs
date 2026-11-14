//! Per-tool body content renderers (bash output, grep, find, diff, file write, code blocks).

pub(crate) mod checklist;
pub(crate) mod code;
pub(crate) mod command;
pub(crate) mod diff;
pub(crate) mod listing;
pub(crate) mod matches;
pub(crate) mod question;
pub(crate) mod summary;
pub(crate) mod web;

#[cfg(test)]
mod tests;

pub(crate) use checklist::draw_checklist_content;
pub(crate) use code::{draw_code_content, normalize_code_content};
pub(crate) use command::{command_for, draw_command_content, emit_command_lines};
#[cfg(test)]
pub(crate) use diff::project_frags_to_wrapped;
#[allow(unused_imports)]
pub(crate) use diff::{
    draw_diff_content, extract_diff_strings_for_tool, find_start_line_in_file,
    project_syntax_diff_frags, project_syntax_slice,
};
pub(crate) use listing::draw_listing_content;
pub(crate) use matches::draw_matches_content;
pub(crate) use question::draw_questions_content;
#[allow(unused_imports)]
pub(crate) use summary::{
    draw_blank_rows, draw_semantic_step_summary, draw_step_summary, semantic_tool_summary_line,
    tool_summary_line,
};
#[allow(unused_imports)]
pub(crate) use web::{
    draw_web_article_content, draw_web_search_content, parse_fallback_web_article,
    parse_fallback_web_search,
};

#[cfg(test)]
pub(crate) use command::{
    coverage_footer, first_salient_signature, fold_signature_for_budget, termination_footer,
};
#[cfg(test)]
pub(crate) use listing::{parse_listing_header, split_dir_leaf};
#[cfg(test)]
pub(crate) use matches::{is_matches_header, literal_query, segment_matches};

use crate::shell::disclosure::renderers::base::RenderCtx;
use nuo_view::model::selection::SelectionState;
use nuo_view::text_layout::block_selection_range;
use nuo_view::tools::{DiffCache, ResultKind};
use nuotc::{Modifier, Style};

/// Render an explicit tool failure as error text on the shared code surface.
/// It deliberately has no line-number gutter: these rows describe the failed
/// operation and are not source-file content.
pub(crate) fn draw_tool_error(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    output: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let bg = ctx.theme.code_surface();
    let pad = Style::default().bg(bg);
    let error = Style::default()
        .bg(bg)
        .fg(ctx.theme.err())
        .add_modifier(Modifier::BOLD);
    let selection = block_selection_range(selection, mi, block_idx);
    let _ = emit_command_lines(
        ctx,
        mi,
        block_idx,
        indent,
        inner_w.max(1),
        pad,
        selection,
        output,
        error,
        0,
    );
}

/// Render an expanded tool step's content — no `Result`/`Diff` label, no
/// separator; just the tool-specific block dispatched by `result_kind`. Known
/// tools with structured output get a specialized renderer; everything else
/// falls back to a line-numbered code block via [`draw_code_content`]. `bash`
/// additionally prefixes the block with a `$ command` line so the whole step
/// reads like a terminal session.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_tool_result(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    message_id: u64,
    name: &str,
    arguments: &str,
    output: &str,
    structured: Option<&nuo_tool::ToolOutput>,
    diff_cache: &mut DiffCache,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
    revealed: bool,
) {
    let block_idx = 1usize;
    // Explicit tool errors are terminal results, not the successful shape the
    // presenter normally renders. In particular, a failed edit has no Patch;
    // deriving a diff from its call arguments would show an intended change
    // that never reached disk and hide the actionable failure message.
    if matches!(
        structured,
        Some(nuo_tool::ToolOutput::Error { .. } | nuo_tool::ToolOutput::PermissionDenied { .. })
    ) {
        draw_tool_error(ctx, mi, block_idx, output, selection, indent, inner_w);
        return;
    }

    let kind = match nuo_view::tools::presenter_for(name).result_kind() {
        ResultKind::Auto => match structured {
            Some(nuo_tool::ToolOutput::Listing { .. }) => ResultKind::Listing,
            _ => ResultKind::Code,
        },
        kind => kind,
    };
    match kind {
        ResultKind::Listing => {
            draw_listing_content(ctx, mi, block_idx, output, selection, indent, inner_w)
        }
        ResultKind::Matches => draw_matches_content(
            ctx, mi, block_idx, output, arguments, selection, indent, inner_w,
        ),
        ResultKind::Command => {
            let command = command_for(structured, arguments);
            draw_command_content(
                ctx, mi, block_idx, output, structured, &command, selection, indent, inner_w,
                revealed,
            );
        }
        ResultKind::Auto | ResultKind::Code => {
            // Prefer the structured payload: `Code::text` is pure file content
            // (the model-facing `prefix`/`suffix` framing is ignored here) and
            // `start_line` carries the read `offset` so an offset snippet
            // numbers from its true file line. `lang` is surfaced as a
            // language-tag line so the block matches the markdown code band.
            // Legacy/restored steps without a payload fall back to the
            // flattened `output` string with `start_line = 0` (slice-relative
            // 1-based numbering).
            let (raw_content, raw_start_line, explicit_lang) = match structured {
                Some(nuo_tool::ToolOutput::Code {
                    text,
                    start_line,
                    lang,
                    ..
                }) => (text.as_str(), *start_line, lang.as_deref()),
                Some(nuo_tool::ToolOutput::Patch {
                    new, start_line, ..
                }) => (new.as_str(), *start_line, None),
                _ => (output, 0, None),
            };

            let normalized = normalize_code_content(raw_content, raw_start_line, arguments);

            // The file path is authoritative for file content; `lang` is a
            // narrow extension hint (e.g. `"rs"`) whose whole job is the
            // language-tag line. Preferring the path keeps syntax highlighting
            // when the hint is a bare extension that `from_path` can't resolve.
            let path_lang = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| {
                    v.get("path")
                        .or_else(|| v.get("file_path"))
                        .or_else(|| v.get("filename"))
                        .or_else(|| v.get("file"))
                        .and_then(|p| p.as_str())
                        .map(nuo_ui_primitives::syntax::Language::from_path)
                })
                .or_else(|| {
                    normalized
                        .detected_path
                        .as_deref()
                        .map(nuo_ui_primitives::syntax::Language::from_path)
                });
            let syntax_lang = path_lang
                .or_else(|| explicit_lang.map(nuo_ui_primitives::syntax::Language::from_path))
                .unwrap_or(nuo_ui_primitives::syntax::Language::Plain);

            draw_code_content(
                ctx,
                mi,
                block_idx,
                &normalized.content,
                normalized.start_line,
                explicit_lang,
                syntax_lang,
                selection,
                indent,
                inner_w,
            )
        }
        ResultKind::Diff => {
            // Prefer the structured Patch payload (old/new from the result);
            // fall back to argument-derived rows only for legacy/restored
            // completed steps. Both paths are cached by stable message id and
            // exact source, so animation frames never repeat Myers/word diffing.
            let path_buf: Option<String> = match structured {
                Some(nuo_tool::ToolOutput::Patch { .. }) => None,
                _ => {
                    let map = if let Ok(serde_json::Value::Object(obj)) =
                        serde_json::from_str::<serde_json::Value>(arguments)
                    {
                        obj
                    } else {
                        nuo_view::tools::parse_streaming_arguments(arguments)
                    };
                    map.get("path")
                        .or_else(|| map.get("file_path"))
                        .or_else(|| map.get("filename"))
                        .or_else(|| map.get("file"))
                        .and_then(|p| p.as_str())
                        .map(|s| s.to_string())
                }
            };
            let path_ref = match structured {
                Some(nuo_tool::ToolOutput::Patch { path, .. }) => Some(path.as_str()),
                _ => path_buf.as_deref(),
            };
            let lang = path_ref
                .map(nuo_ui_primitives::syntax::Language::from_path)
                .unwrap_or(nuo_ui_primitives::syntax::Language::Plain);

            let hunks = match structured {
                Some(nuo_tool::ToolOutput::Patch {
                    old,
                    new,
                    start_line,
                    ..
                }) => {
                    let mut start = *start_line;
                    if start == 0
                        && let Some(path) = path_ref
                    {
                        if let Some(resolved) =
                            find_start_line_in_file(path, ctx.workspace_root, old, new)
                        {
                            start = resolved;
                        }
                    }
                    diff_cache.patch(message_id, old, new, start)
                }
                _ => {
                    let (old_buf, new_buf) = extract_diff_strings_for_tool(name, arguments);
                    if !old_buf.is_empty() || !new_buf.is_empty() {
                        let start = if let Some(path) = path_ref {
                            find_start_line_in_file(path, ctx.workspace_root, &old_buf, &new_buf)
                                .unwrap_or(0)
                        } else {
                            0
                        };
                        diff_cache.patch(message_id, &old_buf, &new_buf, start)
                    } else {
                        diff_cache.legacy_arguments(message_id, name, arguments)
                    }
                }
            };
            if let Some(nuo_tool::ToolOutput::Patch { warnings, .. }) = structured {
                for warning in warnings {
                    let text = format!("Warning: {warning}");
                    let bg = ctx.theme.code_surface();
                    let style = Style::default().bg(bg).fg(ctx.theme.warn());
                    let _ = emit_command_lines(
                        ctx,
                        mi,
                        block_idx,
                        indent,
                        inner_w.max(1),
                        Style::default().bg(bg),
                        block_selection_range(selection, mi, block_idx),
                        &text,
                        style,
                        0,
                    );
                }
            }
            if hunks.is_empty() && !output.trim().is_empty() {
                draw_code_content(
                    ctx,
                    mi,
                    block_idx,
                    output,
                    1,
                    None,
                    nuo_ui_primitives::syntax::Language::Plain,
                    selection,
                    indent,
                    inner_w,
                );
            } else {
                draw_diff_content(ctx, hunks.as_ref(), indent, inner_w, lang);
            }
        }
        ResultKind::Checklist => {
            draw_checklist_content(
                ctx, mi, block_idx, output, arguments, selection, indent, inner_w,
            );
        }
        ResultKind::WebSearch => {
            draw_web_search_content(
                ctx, mi, block_idx, output, arguments, structured, selection, indent, inner_w,
            );
        }
        ResultKind::WebArticle => {
            draw_web_article_content(
                ctx, mi, block_idx, output, arguments, structured, selection, indent, inner_w,
            );
        }
        ResultKind::Questions => {
            draw_questions_content(
                ctx, mi, block_idx, output, arguments, selection, indent, inner_w,
            );
        }
    }
}
