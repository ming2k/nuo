//! The context-plan inspector (`/context`, ADR-0220).
//!
//! One scrolling document with four sections: the request's token shape, the
//! declared sources in emission order (band headers inline), the selected
//! block's rendered body, and the lossy decisions plus the prefix-drift verdict.
//!
//! Three properties are structural rather than cosmetic:
//!
//! 1. **The row order is the request's order.** Rows come from `plan.blocks`
//!    verbatim; nothing here sorts by cost (`[INV-CTXPLAN-05]`). A reader
//!    scanning down the list is reading the prompt from top to bottom.
//! 2. **The body is the submitted block.** Expanding a row shows the exact bytes
//!    that block contributes, joined from the plan's own rendered regions — not
//!    a re-render (`[INV-CTXPLAN-04]`).
//! 3. **The residual is shown.** The per-block column is an attribution, not a
//!    partition: tokenizers are not additive, so the region's measured cost can
//!    differ from the sum of its blocks. The difference is a row, not a hidden
//!    rounding.
//!
//! Every row is a [`SelectableRow`] under `MODAL_DOC`, so the whole inspector is
//! drag-selectable and copyable with `Ctrl+Shift+C`.
use graticule::{Frame, Modifier, Span, Style};
use nuo_protocol::{ContextPlanView, PlannedBlockView, Stability};
use nuo_ui_primitives::components::detail::DetailLayout;
use nuo_ui_primitives::design::MODAL_INNER_H_PADDING;
use nuo_ui_primitives::primitives::{
    ContentModalSpec, FooterHint, HeaderPart, content_modal_area, content_modal_probe, keyvocab,
    modal_chrome_rows, modal_frame, modal_header_parts, render_modal_footer,
};
use nuo_view::components::selectable_body::{RowSegment, SelectableRow, render_selectable_body};
use nuo_view::model::layout::LayoutMap;
use nuo_view::model::selection::SelectionState;

use crate::shell::overlays::common::placeholder;
use crate::shell::render::Theme;

/// Body rows plus the visual row of each block, so a selection can be followed.
pub struct ContextPlanBody {
    pub rows: Vec<SelectableRow>,
    /// `row_of_block[i]` is the row index of `plan.blocks[i]`.
    pub row_of_block: Vec<usize>,
}

/// Build the inspector body.
///
/// `selected` indexes `plan.blocks`; `body_open` expands that block's rendered
/// text in place.
pub fn build_context_plan_body(
    plan: Option<&ContextPlanView>,
    selected: usize,
    body_open: bool,
    theme: &Theme,
) -> ContextPlanBody {
    let Some(plan) = plan else {
        return ContextPlanBody {
            rows: vec![SelectableRow::from_line(placeholder(
                "No request has been assembled in this thread yet. Send one message, \
                 then reopen this inspector.",
                true,
                theme.muted(),
            ))],
            row_of_block: Vec::new(),
        };
    };

    let mut layout = request_section(DetailLayout::new(theme), plan, theme);
    layout = layout.section("Sources · emission order");

    let mut row_of_block = Vec::with_capacity(plan.blocks.len());
    // One row per block, plus a band header the first time a band appears.
    // Bands are contiguous by construction (`Policy::compile` orders them), so a
    // repeated header would describe a regrouping the request never performed.
    let mut seen: Vec<Stability> = Vec::new();
    for (index, block) in plan.blocks.iter().enumerate() {
        if !seen.contains(&block.stability) {
            seen.push(block.stability);
            layout = layout.raw_row(band_header(block.stability, plan, theme));
        }
        row_of_block.push(layout.rows().len());
        layout = layout.raw_row(block_row(block, index == selected, theme));
        // A lossy decision and an inactivity reason are *notes about* a row, not
        // part of it. Hanging them off the row would push it past the modal's
        // width and wrap the columns, which is how a table stops being one; they
        // get their own indented line instead, where they can also be longer than
        // the row's remaining budget.
        if let Some(note) = block_note(block, theme) {
            layout = layout.raw_row(note);
        }
        // The body expands *in place*: a reader who opened `project.rules` reads
        // its text where the block sits in the prompt, not in a panel at the
        // bottom of the document. Every later `row_of_block` is measured through
        // the rows this inserted, so hit-testing and follow-scroll stay correct.
        if body_open && index == selected {
            for row in body_rows(block, theme) {
                layout = layout.raw_row(row);
            }
        }
    }
    let mut rows = layout.build();

    let mut tail = DetailLayout::new(theme);
    if !plan.notices.is_empty() {
        tail = tail.section("Loss disclosed");
        for notice in &plan.notices {
            tail = tail.kv(notice.source_id.as_str(), notice.message.replace('\n', " "));
        }
    }
    tail = tail.section("Cacheable prefix");
    tail = tail.raw_row(drift_row(plan, theme));
    rows.append(&mut tail.build());

    ContextPlanBody { rows, row_of_block }
}

/// The request accounting section: window, ceiling, region split, and the
/// attribution residual.
fn request_section<'a>(
    mut layout: DetailLayout<'a>,
    plan: &ContextPlanView,
    theme: &Theme,
) -> DetailLayout<'a> {
    layout = layout.section("Request");

    let instructions = plan.fixed_tokens;
    let share = if plan.window_tokens == 0 {
        String::new()
    } else {
        format!(
            " ({:.1}% of window)",
            (instructions as f64 / plan.window_tokens as f64) * 100.0
        )
    };
    layout = layout.kv_bold("Instructions", format!("{instructions} tokens{share}"));
    // The instruction region *is* the cacheable prefix: there is no second text
    // region, so "prefix" and "instructions" name the same bytes (ADR-0231).
    layout = layout.kv_muted(
        "Fixed prefix",
        format!(
            "{} tokens · {} blocks",
            plan.fixed_tokens,
            count_active(plan)
        ),
    );
    if plan.window_tokens > 0 {
        layout = layout.kv_muted("Model window", format!("{} tokens", plan.window_tokens));
    }
    layout = layout.kv_muted(
        "Instruction ceiling",
        format!("{} tokens", plan.instructions_ceiling_tokens),
    );

    match plan.estimate {
        Some(estimate) => {
            layout = layout.kv("Request total", format!("{} tokens", estimate.total_tokens));
            let muted = Style::default().fg(theme.muted());
            layout = layout.kv_indented(
                2,
                "History",
                format!("{} tokens", estimate.history_tokens),
                muted,
            );
            layout = layout.kv_indented(
                2,
                "Instructions",
                format!("{} tokens", estimate.instructions_tokens),
                muted,
            );
            layout = layout.kv_indented(
                2,
                "Tool schemas",
                format!("{} tokens", estimate.tool_schema_tokens),
                muted,
            );
            let residual = plan.attribution_residual().unwrap_or(0);
            layout = layout.kv_styled(
                "Attribution",
                format!("{residual:+} tokens vs measured region (tokenizers are not additive)"),
                muted,
            );
        }
        None => {
            layout = layout.kv_muted(
                "Request total",
                "not measured yet — send a message to measure this plan",
            );
        }
    }
    layout
}

fn count_active(plan: &ContextPlanView) -> usize {
    plan.blocks.iter().filter(|block| block.active).count()
}

/// One band header, carrying the band's own share so the grouping answers
/// "what does this band cost" without a second pass.
fn band_header(stability: Stability, plan: &ContextPlanView, theme: &Theme) -> SelectableRow {
    let active = plan
        .blocks
        .iter()
        .filter(|block| block.active && block.stability == stability);
    let mut tokens = 0usize;
    let mut blocks = 0usize;
    for block in active {
        tokens += block.tokens;
        blocks += 1;
    }
    SelectableRow::from_segments(
        vec![
            Span::styled(
                format!("  {} ", band_label(stability)),
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{tokens} tokens · {blocks} active"),
                Style::default().fg(theme.muted()),
            ),
        ]
        .into_iter()
        .map(|span| RowSegment::styled(span.content.into_owned(), span.style))
        .collect(),
    )
}

fn band_label(stability: Stability) -> &'static str {
    // The band is `Stability`'s own vocabulary; the capitalisation is the only
    // presentation choice made here, and it is made once.
    match stability {
        Stability::Frozen => "Frozen",
        Stability::Route => "Route",
        Stability::Session => "Session",
        Stability::Workspace => "Workspace",
    }
}

/// One declared source.
fn block_row(block: &PlannedBlockView, selected: bool, theme: &Theme) -> SelectableRow {
    let marker = if selected { "▸" } else { " " };
    let id_style = if !block.active {
        Style::default().fg(theme.muted())
    } else if selected {
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg())
    };

    let mut segments = vec![
        RowSegment::styled(format!(" {marker} "), Style::default().fg(theme.brand())),
        RowSegment::styled(format!("{:<26}", block.id), id_style),
        RowSegment::styled(
            format!("{:<20}", block.topic.label()),
            Style::default().fg(theme.dim()),
        ),
        RowSegment::styled(
            format!("{:<9}", block.rank.label()),
            Style::default().fg(theme.dim()),
        ),
    ];

    if block.active {
        segments.push(RowSegment::styled(
            format!("{:>6}  ", block.tokens),
            Style::default().fg(theme.fg()),
        ));
        segments.push(RowSegment::styled(
            share_bar(block.tokens),
            Style::default().fg(theme.ok()),
        ));
    } else {
        segments.push(RowSegment::styled(
            "       —  ".to_string(),
            Style::default().fg(theme.muted()),
        ));
    }

    SelectableRow::from_segments(segments)
}

/// The indented note under a row: what was cut and how to get it back, or why
/// the row contributed nothing.
///
/// A note is a second line rather than a trailing column because it is unbounded
/// prose (`read AGENTS.md from line 412`), and appending it to the row would
/// either wrap the table or truncate the recovery route — the one part of the
/// note that must survive.
fn block_note(block: &PlannedBlockView, theme: &Theme) -> Option<SelectableRow> {
    let (text, style) = match (&block.elision, &block.inactive_reason) {
        (Some(elision), _) => (
            format!(
                "elided {} of {} tokens · {}",
                elision.dropped_tokens(),
                elision.full_tokens,
                elision.resume.instruction()
            ),
            Style::default().fg(theme.warn()),
        ),
        (None, Some(reason)) => (reason.clone(), Style::default().fg(theme.muted())),
        (None, None) => return None,
    };
    Some(SelectableRow::styled(format!("        {text}"), style))
}

/// A ten-column shape cue: one column per hundred tokens, capped. Not a meter —
/// the number beside it is the measurement; this is what makes a 900-token block
/// visibly different from a 90-token one at a glance.
fn share_bar(tokens: usize) -> String {
    const COLUMNS: usize = 10;
    const TOKENS_PER_COLUMN: usize = 100;
    let filled = (tokens / TOKENS_PER_COLUMN).min(COLUMNS);
    if filled == 0 {
        "▏".to_string()
    } else {
        "▊".repeat(filled)
    }
}

/// The selected block's rendered body, as document rows.
fn body_rows(block: &PlannedBlockView, theme: &Theme) -> Vec<SelectableRow> {
    let Some(body) = &block.body else {
        return Vec::new();
    };
    let mut rows = vec![SelectableRow::styled(
        format!("  body of {} · {} tokens", block.id, block.tokens),
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    )];
    for line in body.lines() {
        rows.push(SelectableRow::styled(
            format!("  {line}"),
            Style::default().fg(theme.fg()),
        ));
    }
    rows.push(SelectableRow::empty());
    rows
}

/// The drift verdict. `Head` is painted as the defect it is: a frame or `Frozen`
/// block moved with no route change, which is impossible by construction
/// (`[INV-CTXPLAN-02]`).
fn drift_row(plan: &ContextPlanView, theme: &Theme) -> SelectableRow {
    use nuo_protocol::PrefixDrift;
    let (text, style) = match &plan.drift {
        PrefixDrift::None => (
            "none — the cacheable prefix is byte-identical".to_string(),
            Style::default().fg(theme.ok()),
        ),
        PrefixDrift::Suffix { blocks } => (
            format!("suffix moved · {}", blocks.join(", ")),
            Style::default().fg(theme.muted()),
        ),
        PrefixDrift::Head { blocks } => (
            format!(
                "DEFECT — a frame/frozen block moved with no route change · {}",
                blocks.join(", ")
            ),
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD),
        ),
        PrefixDrift::Route => (
            "route changed (expected: a new prefix is a new context)".to_string(),
            Style::default().fg(theme.muted()),
        ),
    };
    SelectableRow::from_segments(vec![
        RowSegment::styled("Drift  ".to_string(), Style::default().fg(theme.dim())),
        RowSegment::styled(text, style),
    ])
}

/// Draw the inspector. Returns the painted panel rect so the event loop can
/// register it as the click-outside-to-dismiss target.
///
/// `block_rows` is an out-parameter, not a return value: the caller needs the
/// rect and the rows, and the rows are only meaningful together with the paint
/// that produced them. Filling it here (rather than re-deriving the body in the
/// input path) is what keeps a click addressing the row the reader sees.
#[allow(clippy::too_many_arguments)]
pub fn draw_context_plan_modal(
    frame: &mut Frame,
    plan: Option<&ContextPlanView>,
    loading: bool,
    selected: usize,
    body_open: bool,
    scroll: &mut usize,
    block_rows: &mut Vec<usize>,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> graticule::Rect {
    let geometry = ContentModalSpec::CONTEXT_PLAN;
    let probe = content_modal_probe(frame, geometry);
    let _body_width = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);

    if loading {
        block_rows.clear();
        let rows = vec![SelectableRow::from_line(placeholder(
            "Querying the measured context plan…",
            false,
            theme.muted(),
        ))];
        return paint(
            frame, geometry, rows, scroll, None, theme, selection, layout_map, false, false,
        );
    }

    let body = build_context_plan_body(plan, selected, body_open, theme);
    *block_rows = body.row_of_block.clone();
    let follow = body.row_of_block.get(selected).copied();
    paint(
        frame,
        geometry,
        body.rows,
        scroll,
        follow,
        theme,
        selection,
        layout_map,
        plan.is_some_and(|plan| !plan.blocks.is_empty()),
        body_open,
    )
}

#[allow(clippy::too_many_arguments)]
fn paint(
    frame: &mut Frame,
    geometry: ContentModalSpec,
    rows: Vec<SelectableRow>,
    scroll: &mut usize,
    follow: Option<usize>,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
    interactive: bool,
    body_open: bool,
) -> graticule::Rect {
    // Size the modal to the **wrapped** document, not to the logical row count:
    // an elision note or a long block id wraps, and a height computed from
    // `rows.len()` would then hide the last section behind a scroll — which is
    // how a reader loses the drift verdict, the one line the inspector exists to
    // deliver.
    let probe = content_modal_probe(frame, geometry);
    let body_width = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);
    let visual_rows: usize = rows
        .iter()
        .map(|row| row.visual_row_count(body_width))
        .sum();
    let desired = (visual_rows as u16).saturating_add(modal_chrome_rows(geometry.modal_spec()));
    let area = content_modal_area(frame, geometry, desired);
    let modal = modal_frame(frame, area, theme, true, true);

    modal_header_parts(
        frame,
        modal.header,
        &[HeaderPart::title("Context Plan")],
        theme,
    );

    render_selectable_body(
        frame, modal.body, &rows, scroll, follow, theme, selection, layout_map,
    );

    if let Some(footer_area) = modal.footer {
        let mut hints = Vec::new();
        if interactive {
            hints.push(FooterHint::navigation(keyvocab::ARROWS_UD, "select"));
            hints.push(FooterHint::primary(
                crate::shell::keymap::Key::ENTER.display(),
                if body_open { "hide body" } else { "show body" },
            ));
        }
        hints.push(FooterHint::always(
            crate::shell::keymap::Key::ESC.display(),
            "close",
        ));
        render_modal_footer(frame, footer_area, &hints, theme);
    }

    area
}

#[cfg(test)]
mod tests;
