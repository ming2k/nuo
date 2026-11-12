//! Tier 2: the polymorphic scene bar and its drill-in stack.

use super::*;

use super::thread_bar::{id_tail, truncate_to_width};
use crate::shell::surfaces::SceneKind;

/// Interactive bounding boxes for Tier 2 SceneBar navigation elements (ADR-0092 [INV-INTERACT-01]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]

pub struct SceneNavRects {
    /// Back navigation affordance rect (`‹ `).
    pub back: Option<Rect>,
    /// Forward navigation affordance rect (` ›`).
    pub forward: Option<Rect>,
    /// Hit target for each breadcrumb segment by stack depth: (depth_index, rect).
    pub crumbs: Vec<(usize, Rect)>,
}

/// Draw Tier 2: Polymorphic SceneBar (ADR-0062 [INV-HEAD-05], [INV-SCENE-02], ADR-0092 [INV-SCENE-03, INV-INTERACT-01]).
///
/// Universal to all scenes.
/// - Left side: Scene identity and authentic task title (`THREAD <id_tail>  <title>`
///   for thread; `<scene_label>  <context>` for dashboard / settings).
///   If untitled, the title slot remains empty without layout shift.
/// - Right side: Drill-in navigation stack / canonical breadcrumbs (`‹  thread › subagent  ›`)
///   with dedicated back and forward affordances.
pub(crate) fn draw_scene_bar(
    frame: &mut Frame,
    rect: Rect,
    thread_id: Option<&str>,
    hints: &ViewHints<'_>,
    theme: &Theme,
) -> SceneNavRects {
    if rect.height == 0 || (rect.width as usize) < STEP_MIN_WIDTH {
        return SceneNavRects::default();
    }

    let bg = theme.scenebar_bg();
    let fill = Style::default().bg(bg);
    let title_style = fill.fg(theme.heading()).add_modifier(Modifier::BOLD);
    let tag_style = fill.fg(theme.dim());
    let label_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);
    let context_style = if hints.context_warn {
        fill.fg(theme.warn()).add_modifier(Modifier::BOLD)
    } else {
        fill.fg(theme.fg())
    };

    let width = rect.width as usize;
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = width.saturating_sub(2 * pad);

    // Right side: Navigation stack / canonical breadcrumbs (when drill-in depth > 1) (ADR-0092 [INV-SCENE-03])
    struct NavElement {
        crumb_index: Option<usize>,
        is_back: bool,
        is_forward: bool,
        width: usize,
    }

    let (right_spans, right_width, elements) =
        if let Some(crumbs) = hints.breadcrumbs.filter(|c| c.len() > 1) {
            let mut spans = Vec::new();
            let mut total_w = 0usize;
            let mut elems = Vec::new();

            if hints.can_back {
                // A pure chevron: the affordance is a *navigation control*, not
                // a keycap. Chords belong to the Help dialog (ADR-0131 §3),
                // which is the one surface that resolves them from the live
                // keymap — printing `Alt+[` here hardcoded a binding the user
                // may have remapped, and duplicated the discovery surface.
                let back_str = "‹ ";
                let bw = back_str.width();
                let style = fill.fg(theme.dim());
                spans.push(Span::styled(back_str, style));
                elems.push(NavElement {
                    crumb_index: None,
                    is_back: true,
                    is_forward: false,
                    width: bw,
                });
                total_w += bw;
            }

            for (idx, crumb) in crumbs.iter().enumerate() {
                let crumb_str = crumb.breadcrumb_label();
                let cw = crumb_str.width();
                let is_last = idx + 1 == crumbs.len();
                let style = if is_last {
                    label_style
                } else {
                    fill.fg(theme.dim())
                };

                spans.push(Span::styled(crumb_str, style));
                elems.push(NavElement {
                    crumb_index: Some(idx),
                    is_back: false,
                    is_forward: false,
                    width: cw,
                });
                total_w += cw;

                if !is_last {
                    let sep = " › ";
                    let sep_w = sep.width();
                    let sep_style = fill.fg(theme.dim());
                    spans.push(Span::styled(sep, sep_style));
                    elems.push(NavElement {
                        crumb_index: None,
                        is_back: false,
                        is_forward: false,
                        width: sep_w,
                    });
                    total_w += sep_w;
                }
            }

            if hints.can_forward {
                let fwd_str = " ›";
                let fw = fwd_str.width();
                let style = fill.fg(theme.dim());
                spans.push(Span::styled(fwd_str, style));
                elems.push(NavElement {
                    crumb_index: None,
                    is_back: false,
                    is_forward: true,
                    width: fw,
                });
                total_w += fw;
            }

            (spans, total_w, elems)
        } else {
            (Vec::new(), 0, Vec::new())
        };

    // Left side:
    // If Thread scene (or thread_id is provided):
    //   "THREAD " + "<id_tail> " + (optional title)
    // If Non-thread scene:
    //   scene_label + (optional context)
    let mut left_spans = Vec::new();
    let mut left_width = 0usize;

    if hints.kind == SceneKind::Thread {
        const TITLE: &str = "THREAD ";
        left_spans.push(Span::styled(TITLE, title_style));
        left_width += TITLE.width();

        if let Some(id) = thread_id {
            let tag = id_tail(id);
            if !tag.is_empty() {
                left_spans.push(Span::styled(tag.to_string(), tag_style));
                left_width += tag.width();
            }
        }

        if let Some(ctx) = hints.context.filter(|c| !c.is_empty()) {
            let sep_w = 2;
            let budget = text_width.saturating_sub(left_width + sep_w + right_width);
            let truncated = truncate_to_width(ctx, budget);
            if !truncated.is_empty() {
                let w = truncated.width();
                left_spans.push(Span::styled("  ", fill));
                left_spans.push(Span::styled(truncated, title_style));
                left_width += 2 + w;
            }
        }
    } else {
        let lead_label = if hints.breadcrumbs.is_none_or(|c| c.len() <= 1) {
            Some(hints.kind.scene_label())
        } else {
            None
        };

        if let Some(lbl) = lead_label {
            left_spans.push(Span::styled(lbl, label_style));
            left_width += lbl.width();
        }

        if let Some(ctx) = hints.context.filter(|c| !c.is_empty()) {
            let sep_w = if left_width > 0 { 2 } else { 0 };
            let budget = text_width.saturating_sub(left_width + sep_w + right_width);
            let truncated = truncate_to_width(ctx, budget);
            if !truncated.is_empty() {
                if left_width > 0 {
                    left_spans.push(Span::styled("  ", fill));
                    left_width += 2;
                }
                left_width += truncated.width();
                left_spans.push(Span::styled(truncated, context_style));
            }
        }
    }

    let gap = text_width.saturating_sub(left_width + right_width);
    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
    spans.extend(left_spans);
    spans.push(Span::styled(" ".repeat(gap), fill));
    spans.extend(right_spans);
    spans.push(Span::styled(" ".repeat(pad), fill));

    frame.render_widget(&mut Paragraph::new(Line::from(spans)), rect);

    // Compute interactive hit target bounding boxes (ADR-0092 [INV-INTERACT-01])
    let right_start_x = rect.x + pad as u16 + left_width as u16 + gap as u16;
    let mut cur_x = right_start_x;
    let mut nav_rects = SceneNavRects::default();

    for elem in elements {
        let elem_rect = Rect::new(cur_x, rect.y, elem.width as u16, 1);
        if elem.is_back {
            nav_rects.back = Some(elem_rect);
        } else if elem.is_forward {
            nav_rects.forward = Some(elem_rect);
        } else if let Some(idx) = elem.crumb_index {
            nav_rects.crumbs.push((idx, elem_rect));
        }
        cur_x += elem.width as u16;
    }

    nav_rects
}
