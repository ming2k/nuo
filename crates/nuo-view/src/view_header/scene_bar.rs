//! Tier 2: the polymorphic scene bar and its drill-in stack (ADR-0062, ADR-0261).

use super::*;

use super::thread_bar::{draw_thread_scene_bar, truncate_role_token, truncate_with};
use crate::view_header::SceneKind;

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

/// Draw Tier 2: Polymorphic SceneBar (ADR-0062 `[INV-SCENE-02]`, ADR-0092
/// `[INV-SCENE-03, INV-INTERACT-01]`, ADR-0261 `[INV-HEAD-09, INV-HEAD-10]`,
/// ADR-0268 `[INV-HEAD-11, INV-HEAD-12]`).
///
/// Universal to all scenes.
/// - For `SceneKind::Thread`: delegates to the single-row Thread SceneBar
///   (`ROLE  posture  <title>` on chromatic surfaces / `[ROLE]` on framed
///   surfaces, with the title slot silent while untitled; right-anchored
///   workspace path on the right).
/// - For non-thread scenes (`Subagent`, `Dashboard`, `Settings`, `CommandLog`,
///   `Diff`): scene label + domain context on the left; drill-in navigation
///   stack (`‹  thread › subagent  ›`) on the right.
pub fn draw_scene_bar(
    frame: &mut Frame,
    rect: Rect,
    hints: &ViewHints<'_>,
    thread_context: Option<&ThreadContext<'_>>,
    theme: &Theme,
) -> SceneNavRects {
    if rect.height == 0 || (rect.width as usize) < STEP_MIN_WIDTH {
        return SceneNavRects::default();
    }

    if hints.kind == SceneKind::Thread {
        draw_thread_scene_bar(frame, rect, hints.context, thread_context, theme);
        return SceneNavRects::default();
    }

    let bg = theme.scenebar_bg();
    let fill = Style::default().bg(bg);
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

    // Left side (non-thread scenes): scene_label + (optional role) + (optional context)
    let mut left_spans = Vec::new();
    let mut left_width = 0usize;

    let lead_label = if hints.breadcrumbs.is_none_or(|c| c.len() <= 1) {
        Some(hints.kind.scene_label())
    } else {
        None
    };

    if let Some(lbl) = lead_label {
        left_spans.push(Span::styled(lbl, label_style));
        left_width += lbl.width();
    }

    let right_reserve = if right_width > 0 {
        right_width + JOIN_ENUMERATE_COLS
    } else {
        0
    };

    if let Some(role) = hints.role.map(str::trim).filter(|r| !r.is_empty()) {
        let sep_w = if left_width > 0 {
            JOIN_ENUMERATE_COLS
        } else {
            0
        };
        let budget = text_width.saturating_sub(left_width + sep_w + right_reserve);
        let token = truncate_role_token(
            role,
            budget,
            theme.elevation.is_chromatic(),
            theme.glyphs.ellipsis,
        );
        if !token.is_empty() {
            if left_width > 0 {
                left_spans.push(Span::styled(" ".repeat(JOIN_ENUMERATE_COLS), fill));
                left_width += JOIN_ENUMERATE_COLS;
            }
            left_width += token.width();
            left_spans.push(Span::styled(token, label_style));
        }
    }

    if let Some(ctx) = hints.context.filter(|c| !c.is_empty()) {
        let sep_w = if left_width > 0 {
            JOIN_ENUMERATE_COLS
        } else {
            0
        };
        let budget = text_width.saturating_sub(left_width + sep_w + right_reserve);
        let truncated = truncate_with(ctx, budget, theme.glyphs.ellipsis);
        if !truncated.is_empty() {
            if left_width > 0 {
                left_spans.push(Span::styled(" ".repeat(JOIN_ENUMERATE_COLS), fill));
                left_width += JOIN_ENUMERATE_COLS;
            }
            left_width += truncated.width();
            left_spans.push(Span::styled(truncated, context_style));
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
