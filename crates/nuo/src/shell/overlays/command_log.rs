//! The command-log scene: one command step's whole output, full screen.
//!
//! This is the destination behind the transcript's fold block
//! (`press v view full log`) and the focused-entry verb. It is a **scene**, not a
//! dialog, and that is the whole design: a build log is content, so it belongs to
//! the content layer (ADR-0116 `[INV-LAYER-01]`), and the scene layer already owns
//! everything a reader needs — the tab's navigation stack, `Alt+[`, breadcrumbs,
//! resize, scroll anchoring. The previous dialog form put a thousand-line log
//! inside a floating 60-column panel: two reading positions for one text, and the
//! terminal's own width spent on a border.
//!
//! # What it shows
//!
//! - The head band every scene shares (ADR-0062): TabBar, then the scene row
//!   naming `log`, the command, and which slice is on screen.
//! - The body: every row, numbered, on the code surface — the middle the
//!   transcript folded away included, which is the reason the destination
//!   exists. Rows are flush: the line number is the only column in front of the
//!   text, so the row a reader shift-drags is the row the process printed.
//! - The outcome: a non-zero exit and the harness's termination verdict, stated
//!   with the same words the transcript used. A scene that described an outcome
//!   differently from the body would be a second opinion about one fact.
//! - A footer of the verbs that work here, resolved from the same key tokens the
//!   fold block prints.
//!
//! # Copying
//!
//! Rows are registered on the layout map as real text regions, so the ordinary
//! drag-and-copy path works without a scene-specific implementation. `y` copies
//! the whole visible slice through
//! [`CommandLog::copy_text`](nuo_view::model::command_log::CommandLog::copy_text),
//! the same derivation the receipt toast is built from.

use nuotc::{Constraint, Direction, Frame, Layout, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::shell::disclosure::renderers::base::truncate_to_width;
use crate::shell::render::{Theme, draw_scene_bar, draw_tab_bar};
use crate::shell::view_header::{TabBarNavRects, TabBarProps, ViewHints};
use nuo_ui_primitives::design::{COPY_LOG_KEY, JOIN_CAUSE, LOG_SCOPE_KEY};
use nuo_ui_primitives::primitives::viewport_rect;
use nuo_view::model::command_log::CommandLog;
use nuo_view::model::layout::{BlockRegion, LayoutMap};
use nuo_view::text_layout::padded_tail;

/// The scene's fixed vertical chrome outside the body: the two head rows, the
/// rule under them, and the footer band. Stated once so the body height — and
/// therefore the scroll clamp — cannot drift from what is painted.
pub const LOG_SCENE_CHROME_ROWS: u16 = 4;

/// Rects the scene reports back so pointer targets can be mounted.
#[derive(Debug, Clone, Default)]
pub struct LogSceneRects {
    /// The whole scene area.
    pub area: Rect,
    /// The scrollable body.
    pub body: Rect,
    /// TabBar row.
    pub tab_bar_rect: Option<Rect>,
    /// Tab hit boxes.
    pub tab_rects: Vec<(usize, Rect)>,
    pub tab_prev_rect: Option<Rect>,
    pub tab_next_rect: Option<Rect>,
    pub tab_overflow_rect: Option<Rect>,
    pub tab_notices_rect: Option<Rect>,
    pub tab_help_rect: Option<Rect>,
}

/// What the scene needs to paint itself.
pub struct LogSceneProps<'a> {
    /// The log being read. `None` is a real state (the step vanished under a
    /// transcript rebuild) and is stated as such rather than painted as an
    /// empty page.
    pub log: Option<&'a CommandLog>,
    /// Body scroll offset, clamped here so the app's copy never has to know the
    /// painted height.
    pub scroll: &'a mut usize,
    /// Session id whose transcript this log came from (scene-bar context).
    pub thread_id: Option<&'a str>,
    /// TabBar projection (client-global chrome tier 1).
    ///
    /// The unread-fault count rides inside it (`TabBarProps::unread_faults`), so
    /// the badge is fed by the same projection every other scene's TabBar uses
    /// instead of a second count that could disagree.
    pub tab_bar: Option<TabBarProps<'a>>,
}

/// Draw the full-screen command log.
pub fn draw_command_log_scene(
    frame: &mut Frame,
    props: LogSceneProps<'_>,
    layout_map: &mut LayoutMap,
    theme: &Theme,
) -> LogSceneRects {
    let LogSceneProps {
        log,
        scroll,
        thread_id,
        tab_bar,
    } = props;

    // A true full-screen surface: clear the whole frame and paint our own
    // backdrop, exactly as the dashboard does, so no transcript pixel shows
    // through a short log.
    frame.render_widget(&mut nuotc::Clear::default(), frame.area());
    frame.render_widget(
        &mut nuotc::Block::default().style(Style::default().bg(theme.app_bg)),
        frame.area(),
    );

    let full = frame.area();
    let area = viewport_rect(frame);

    // Split the viewport explicitly rather than subtracting constants by hand:
    // head band (TabBar + SceneBar) on top, the shared bottom margin, and between
    // them a *footer band* and the scrollable body. Stating the tree makes the
    // body height — and therefore the scroll clamp — a consequence of what is
    // painted, so a row can never be drawn outside the region that was measured
    // for it.
    let rows_area = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),                                   // head band
            Constraint::Min(1),                                      // body
            Constraint::Length(1),                                   // gap above the band
            Constraint::Length(1),                                   // footer band
            Constraint::Length(if area.height > 8 { 1 } else { 0 }), // bottom margin
        ])
        .split(area);
    let head = Rect {
        x: full.x,
        width: full.width,
        ..rows_area[0]
    };
    let body = rows_area[1];
    let footer = rows_area[3];

    let tab_rects = match tab_bar.as_ref() {
        Some(bar) => draw_tab_bar(frame, Rect { height: 1, ..head }, bar, theme),
        None => TabBarNavRects::default(),
    };
    let context = log_context(log);
    draw_scene_bar(
        frame,
        Rect {
            y: head.y.saturating_add(1),
            height: 1,
            ..head
        },
        thread_id,
        &ViewHints {
            kind: crate::shell::surfaces::SceneKind::CommandLog,
            context: Some(context.as_str()),
            context_warn: log.is_some_and(|log| log.outcome_line().is_some()),
            breadcrumbs: None,
            can_back: false,
            can_forward: false,
        },
        theme,
    );

    let rows = log.map(|log| log.visible_rows()).unwrap_or(&[]);
    let total = rows.len();
    let body_h_usize = body.height as usize;
    let max_scroll = total.saturating_sub(body_h_usize);
    *scroll = (*scroll).min(max_scroll);

    let gutter_w = total.to_string().len().max(3);
    // ` ` + gutter + two-column gap. The line number is the scene's only
    // structure column: a full page of a thousand rows needs a way to say "this
    // is row 617", and nothing else is added in front of the text — no `│`, so
    // the row the reader shift-drags is the row the process printed
    // (ADR-0150).
    let prefix_w = 1 + gutter_w + 2;
    let text_w = (body.width as usize).saturating_sub(prefix_w).max(1);
    let bg = theme.code_surface();

    for row_idx in 0..body_h_usize {
        let y = body.y + row_idx as u16;
        let line_idx = *scroll + row_idx;
        let rect = Rect::new(body.x, y, body.width, 1);
        if line_idx >= total {
            // Past the end of the log: the surface, not the terminal background,
            // so a short log still reads as one page.
            frame.render_widget(
                &mut Paragraph::new(Line::from(Span::styled(
                    padded_tail(body.width as usize, 0),
                    Style::default().bg(bg),
                ))),
                rect,
            );
            continue;
        }
        let row = &rows[line_idx];
        let number = format!("{:>width$}", line_idx + 1, width = gutter_w);
        let truncated = truncate_to_width(row, text_w);
        let spans = vec![
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(number.clone(), Style::default().bg(bg).fg(theme.dim())),
            Span::styled("  ", Style::default().bg(bg)),
            Span::styled(
                truncated.to_string(),
                Style::default().bg(bg).fg(theme.code_text()),
            ),
            Span::styled(
                padded_tail(body.width as usize, prefix_w + truncated.width()),
                Style::default().bg(bg),
            ),
        ];
        frame.render_widget(&mut Paragraph::new(Line::from(spans)), rect);

        // Register the row as real text so drag-and-copy works through the
        // ordinary selection path. The painted indent and line-number gutter are
        // chrome: `prefix_cols` is where the text starts, so a drag that begins
        // on the number copies the row, not the number.
        layout_map.push(BlockRegion {
            message_idx: log.map(|l| l.message_idx).unwrap_or(0),
            block_idx: line_idx,
            start_byte: 0,
            end_byte: row.len(),
            text: truncated.clone(),
            prefix_cols: prefix_w as u16,
            rect,
            hidden_ranges: Vec::new(),
        });
    }

    draw_log_footer(frame, footer, log, scroll, max_scroll, theme);

    // A mouse wheel / PgDn must have somewhere to land: expose the body rect as
    // the scene's scroll target.
    LogSceneRects {
        area,
        body,
        tab_bar_rect: Some(Rect { height: 1, ..head }),
        tab_rects: tab_rects.tab_rects,
        tab_prev_rect: tab_rects.prev_rect,
        tab_next_rect: tab_rects.next_rect,
        tab_overflow_rect: tab_rects.overview_rect,
        tab_notices_rect: tab_rects.notices_rect,
        tab_help_rect: tab_rects.help_rect,
    }
}

/// The scene row's context: the command, how much of its log is on screen, and
/// the outcome when it failed.
///
/// Assembled here rather than in the caller so the head band and the footer
/// cannot describe different slices.
fn log_context(log: Option<&CommandLog>) -> String {
    let Some(log) = log else {
        return "no log".to_string();
    };
    let mut context = String::new();
    let command = truncate_to_width(&log.command, 72);
    if !command.is_empty() {
        context.push('$');
        context.push(' ');
        context.push_str(&command);
    } else {
        context.push_str("(no invocation recorded)");
    }
    // The slice was narrowed by `h`: say so, because the row count alone would
    // read as "this is the whole log".
    if log.scope != nuo_view::model::command_log::LogScope::Full {
        context.push_str(JOIN_CAUSE);
        context.push_str(log.scope.label());
    }
    if let Some(outcome) = log.outcome_line() {
        context.push_str(JOIN_CAUSE);
        context.push_str(&outcome);
    }
    context
}

/// The band under the body: the row count and read position on the left, the
/// verbs on the right.
///
/// The two clusters are joined by a **gap**, not by concatenation: `20 rows` and
/// `1 / 15` are two different facts and ran together as `20 rows1 / 15` when they
/// were pushed as adjacent spans. The left facts are one muted sentence (count,
/// position, outcome), the right verbs use the shared `key label` vocabulary — the
/// same shape every other footer band has, so the reading surface does not invent
/// a second legend grammar.
fn draw_log_footer(
    frame: &mut Frame,
    rect: Rect,
    log: Option<&CommandLog>,
    scroll: &usize,
    max_scroll: usize,
    theme: &Theme,
) {
    let width = rect.width as usize;
    if rect.height == 0 || width == 0 {
        return;
    }
    let bg = theme.body();
    let fill = Style::default().bg(bg);
    let facts_style = Style::default().bg(bg).fg(theme.muted());
    let dim_style = Style::default().bg(bg).fg(theme.dim());
    let key_style = Style::default()
        .bg(bg)
        .fg(theme.keycap_fg())
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().bg(bg).fg(theme.keycap_label());

    // Left facts, as one sentence with explicit separators.
    let mut facts: Vec<Span<'static>> = Vec::new();
    if let Some(log) = log {
        let rows = log.len();
        let noun = if rows == 1 { "row" } else { "rows" };
        facts.push(Span::styled(format!("{rows} {noun}"), facts_style));
        // The position is a *fact*, not an affordance: it answers "how far in am
        // I" without a scrollbar competing with the row numbers.
        if max_scroll > 0 {
            facts.push(Span::styled("   ", fill));
            facts.push(Span::styled(
                format!("{} / {}", (*scroll).min(max_scroll) + 1, max_scroll + 1),
                dim_style,
            ));
        }
        if let Some(outcome) = log.outcome_line() {
            facts.push(Span::styled("   ", fill));
            facts.push(Span::styled(
                outcome,
                Style::default().bg(bg).fg(theme.warn()),
            ));
        }
    }

    let mut verbs: Vec<(&'static str, &'static str)> = Vec::new();
    // The verb names what pressing it *does*, so it flips with the scope. Printing
    // `hidden only` while already reading the hidden rows would advertise a
    // destination the reader is standing in — the same class of lie as a keycap
    // that does not serve the key it prints.
    if let Some(log) = log.filter(|l| l.can_scope_toggle()) {
        verbs.push((
            LOG_SCOPE_KEY,
            match log.scope {
                nuo_view::model::command_log::LogScope::Full => "hidden only",
                nuo_view::model::command_log::LogScope::HiddenOnly => "full log",
            },
        ));
    }
    verbs.push((COPY_LOG_KEY, "copy"));
    verbs.push(("↑/↓", "scroll"));
    verbs.push(("Esc", "back"));

    // Drop trailing verbs until the band fits: the first ones are the reason the
    // surface exists, so they are the last to go (the same priority rule the
    // modal footers follow).
    let facts_w: usize = facts.iter().map(|s| s.content.width()).sum();
    let verbs_width = |v: &[(&'static str, &'static str)]| -> usize {
        v.iter()
            .map(|(k, l)| k.width() + 1 + l.width())
            .sum::<usize>()
            + 2 * v.len().saturating_sub(1)
    };
    while !verbs.is_empty() && 2 + facts_w + 2 + verbs_width(&verbs) + 2 > width {
        verbs.pop();
    }

    let mut spans: Vec<Span<'static>> = vec![Span::styled("  ", fill)];
    spans.extend(facts);
    let gap = width.saturating_sub(2 + facts_w + verbs_width(&verbs) + 2);
    spans.push(Span::styled(" ".repeat(gap + 2), fill));
    for (idx, (key, label)) in verbs.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled("  ", fill));
        }
        spans.push(Span::styled(key.to_string(), key_style));
        spans.push(Span::styled(format!(" {label}"), label_style));
    }
    let total: usize = spans.iter().map(|s| s.content.width()).sum();
    spans.push(Span::styled(padded_tail(width, total), fill));

    // One centered row, like every other scene's footer band.
    let mid = rect.y + rect.height / 2;
    let blank = Line::from(Span::styled(" ".repeat(width), fill));
    for y in rect.y..rect.y + rect.height {
        let line = if y == mid {
            Line::from(spans.clone())
        } else {
            blank.clone()
        };
        frame.render_widget(
            &mut Paragraph::new(line),
            Rect::new(rect.x, y, rect.width, 1),
        );
    }
}
