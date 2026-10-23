//! Polymorphic spatial header topology for every scene (ADR-0060, ADR-0062).
//!
//! The head band is the sovereign chrome pinned to the terminal's top edge.
//! Under ADR-0062 it is structured across a polymorphic hierarchy:
//!
//! - **Tier 1 — Client Global TabBar (`draw_tab_bar`).**
//!   Fixed height: 1 row. Owned by the Client container. Projects open tabs
//!   with clean semantic titles (e.g. `Refactor DB`, `dashboard`, `settings`)
//!   or compact untitled fallbacks (`thread#b3c4`), strictly omitting ordinal
//!   prefixes (`1:`) [INV-TAB-09]. Carries the standing `C-x menu` client affordance
//!   on the right edge.
//!
//! - **Tier 2 — Polymorphic SceneBar (`draw_scene_bar`).**
//!   Fixed height: 1 row. Owned by the active scene view [INV-SCENE-02]. Displays
//!   the primary task identity on the left:
//!   - For `Thread`: `THREAD <id_tail>` + authentic task title (with vestigial
//!     dummy `thread` placeholders eliminated when untitled [INV-SCENE-01]).
//!   - For Non-Thread (`Dashboard`, `Settings`): scene label + domain summary context.
//!   - On the right: right-aligned drill-in navigation stack / breadcrumbs
//!     (`‹ Alt+[  thread › subagent  Alt+] ›`) with interactive navigation targets (ADR-0092 [INV-SCENE-03, INV-INTERACT-01]).
//!
//! - **Tier 3 — Sovereign ThreadContextBar (`draw_thread_context_bar`).**
//!   Height: 1 row on Thread viewports; **0 rows on non-thread scenes** (`Dashboard`, `Settings`)
//!   [INV-HEAD-05]. Reads *who → how → where* across the row: the staffing
//!   `[ROLE]` badge (brand), the persistent safety posture tokens (`auto`,
//!   `open`; warning tone) on the left, and the bound workspace path pinned to
//!   the right edge, where it shortens from the ancestor end and therefore
//!   never moves the governance cluster around.
use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{
    CONTEXT_BAR_WORKSPACE_MIN_COLS, JOIN_ENUMERATE_COLS, STEP_MIN_WIDTH, TRANSCRIPT_H_INSET, Theme,
};
use crate::components::path::{PathFormatStrategy, PathStyle, PathView};
use crate::surfaces::SceneKind;

/// Canonical scene identifier alias for header presentation (ADR-0042).
pub(crate) type ViewKind = crate::surfaces::SceneKind;

/// Maximum display width allocated to individual tab titles before truncation.
pub(crate) const MAX_TAB_TITLE_WIDTH: usize = 20;

/// Scene context for header presentation (ADR-0060, ADR-0062).
///
/// Focuses purely on viewport context: authentic task title or domain summary
/// on the left, and navigation stack / drill-in breadcrumbs on the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ViewHints<'a> {
    /// Which scene the row describes — decides the leading scene label.
    pub kind: ViewKind,
    /// Scene context shown: the thread's authentic chat title, a subagent's
    /// task label, the dashboard's fleet summary, or the settings breadcrumb.
    pub context: Option<&'a str>,
    /// `true` while the context is an attention state (e.g. the dashboard has
    /// a session needing approval), escalating it to the warning tone.
    pub context_warn: bool,
    /// Optional breadcrumbs from the active tab's history stack (ADR-0040, ADR-0042, ADR-0060).
    pub breadcrumbs: Option<&'a [crate::surfaces::SceneKind]>,
    /// Whether back navigation is available in the active tab.
    pub can_back: bool,
    /// Whether forward navigation is available in the active tab.
    pub can_forward: bool,
}

impl<'a> ViewHints<'a> {
    #[allow(dead_code)]
    pub fn simple(kind: ViewKind) -> Self {
        Self {
            kind,
            context: None,
            context_warn: false,
            breadcrumbs: None,
            can_back: false,
            can_forward: false,
        }
    }
}

/// Canonical client tabbar projection props (ADR-0039, ADR-0060, ADR-0062 [INV-TAB-09]).
///
/// Tier 1: Client Global TabBar is owned purely by the Client container.
/// It strictly carries open tabs and active tab index — no thread-specific
/// governance metadata (role, workspace, safety posture).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct TabBarProps<'a> {
    pub tabs: Option<&'a [crate::surfaces::ClientTab]>,
    pub active_tab: usize,
}

/// Canonical thread execution governance context (ADR-0043, ADR-0060, ADR-0062 [INV-HEAD-05, INV-HEAD-07, INV-HEAD-08]).
///
/// Tier 3: Sovereign ThreadContextBar is owned strictly by the Thread domain.
/// Displays staffing role, safety posture tokens, and bound workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct ThreadContext<'a> {
    /// Active staffing role for this thread (e.g. "developer").
    pub role: Option<&'a str>,
    /// Tilde-shortened workspace path (e.g. `~/projects/xx`), or empty when unbound.
    pub workspace: &'a str,
    /// `true` while the thread runs unattended (`--unattended` / `/unattended on`).
    pub unattended: bool,
    /// `false` while workspace filesystem confinement is off (`/confinement off`).
    pub confined: bool,
}

/// Draw Tier 1: Client Global TabBar (ADR-0039, ADR-0040, ADR-0060 [INV-TAB-09], ADR-0062).
///
/// Projects open tabs with clean semantic titles without ordinal prefixes (`1:`),
/// with the client command namespace (`C-x menu`) anchored to the right edge.
/// Returns bounding rects for mouse click hit-testing.
pub(crate) fn draw_tab_bar(
    frame: &mut Frame,
    rect: Rect,
    tab_bar: &TabBarProps<'_>,
    theme: &Theme,
) -> Vec<(usize, Rect)> {
    let full_width = rect.width as usize;
    if full_width < STEP_MIN_WIDTH || rect.height == 0 {
        return Vec::new();
    }

    let bg = theme.tabbar_bg();
    let fill = Style::default().bg(bg);
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = full_width.saturating_sub(2 * pad);

    // C-x affordance belongs to the client-level TabBar / Header (ADR-0043 [INV-UI-02])
    let affordance = crate::components::keycap::KeyAffordance::tabbar_menu(SCENE_NAMESPACE_LABEL);
    let right_width = affordance.width();

    // Tab workspace rendering (ADR-0039 [INV-TAB-01], ADR-0040 [INV-UI-01], ADR-0060 [INV-TAB-09])
    if let Some(tabs) = tab_bar.tabs.filter(|t| !t.is_empty()) {
        let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
        let mut tab_rects = Vec::with_capacity(tabs.len());
        let mut current_x = rect.x.saturating_add(pad as u16);
        let available_for_tabs = text_width.saturating_sub(right_width);
        let mut used_tab_w = 0usize;

        for (i, tab) in tabs.iter().enumerate() {
            let active = i == tab_bar.active_tab;
            let label = match &tab.kind {
                crate::surfaces::TabKind::Thread(id) => {
                    if let Some(title) = tab.display_title() {
                        let truncated = truncate_to_width(title, MAX_TAB_TITLE_WIDTH);
                        truncated.to_string()
                    } else {
                        format!("thread#{}", id_tail(id))
                    }
                }
                crate::surfaces::TabKind::Dashboard => "dashboard".to_string(),
                crate::surfaces::TabKind::Settings => "settings".to_string(),
            };

            let text = format!(" {label} ");
            let tab_w = text.width();
            let separator_w = if i + 1 < tabs.len() { 1 } else { 0 };

            if used_tab_w + tab_w > available_for_tabs && i > 0 {
                break;
            }

            let tab_style = if theme.elevation == nuotc::ElevationArchetype::Structured {
                if active {
                    Style::default().add_modifier(Modifier::REVERSE | Modifier::BOLD)
                } else {
                    Style::default().fg(theme.dim())
                }
            } else if active {
                Style::default()
                    .bg(theme.selected_bg)
                    .fg(theme.heading())
                    .add_modifier(Modifier::BOLD)
            } else {
                let inactive_bg = if i % 2 == 0 {
                    theme.panel()
                } else {
                    theme.body()
                };
                Style::default().bg(inactive_bg).fg(theme.text_muted)
            };

            // Contiguous hit target: tab 0 subsumes leading padding, and every tab
            // includes its trailing separator, eliminating dead gaps between tabs.
            let hit_start = if i == 0 { rect.x } else { current_x };
            let hit_end = current_x.saturating_add((tab_w + separator_w) as u16);
            if hit_start < rect.right() {
                let w = hit_end.min(rect.right()).saturating_sub(hit_start);
                if w > 0 {
                    tab_rects.push((i, Rect::new(hit_start, rect.y, w, 1)));
                }
            }

            spans.push(Span::styled(text, tab_style));
            used_tab_w += tab_w;
            current_x = current_x.saturating_add(tab_w as u16);

            if i + 1 < tabs.len() && used_tab_w + separator_w <= available_for_tabs {
                spans.push(Span::styled(" ", fill));
                used_tab_w += separator_w;
                current_x = current_x.saturating_add(separator_w as u16);
            }
        }

        let gap = text_width.saturating_sub(used_tab_w + right_width);
        spans.push(Span::styled(" ".repeat(gap), fill));

        // Right-aligned C-x affordance
        spans.extend(affordance.render_tabbar_spans(theme, bg));
        spans.push(Span::styled(" ".repeat(pad), fill));

        frame.render_widget(Paragraph::new(Line::from(spans)), rect);
        return tab_rects;
    }

    // Fallback: Brand mark + C-x affordance
    let brand = "nuo";
    let left_w = brand.width();
    let gap = text_width.saturating_sub(left_w + right_width);

    let mut spans = vec![
        Span::styled(" ".repeat(pad), fill),
        Span::styled(brand, fill.fg(theme.brand()).add_modifier(Modifier::BOLD)),
        Span::styled(" ".repeat(gap), fill),
    ];

    spans.extend(affordance.render_tabbar_spans(theme, bg));
    spans.push(Span::styled(" ".repeat(pad), fill));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    Vec::new()
}

/// Interactive bounding boxes for Tier 2 SceneBar navigation elements (ADR-0092 [INV-INTERACT-01]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SceneNavRects {
    /// Back navigation affordance rect (`‹ Alt+[`).
    pub back: Option<Rect>,
    /// Forward navigation affordance rect (`Alt+] ›`).
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
/// - Right side: Drill-in navigation stack / canonical breadcrumbs (`‹ Alt+[  thread › subagent  Alt+] ›`)
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
                let back_str = "‹ Alt+[ ";
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
                let fwd_str = "  Alt+] ›";
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

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);

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

/// One persistent safety posture token of the ThreadContextBar.
///
/// Posture is reported as a *closed vocabulary* of short lowercase tokens, not
/// as sentences: the bar is read at a glance, and the ambient honesty rule is
/// that **only deviations from the safe default are drawn** (interactive
/// confirmation + workspace-confined filesystem), so a normally-behaved
/// session spends zero columns on safety prose.
///
/// Each token is chosen to be self-describing — `auto` and `open` name the
/// state they assert — so no tooltip layer is needed. The authoritative
/// long-form wording (usage, subcommands, examples) lives in the slash-command
/// catalog (`BuiltinCmd::SPECS` in `nuo-server`), which is where
/// `/unattended on|off` and `/confinement on|off` explain themselves. Adding a
/// second explanation surface here would fork that text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PostureToken {
    /// Tool calls are approved without asking (`--unattended` / `/unattended on`).
    Auto,
    /// File tools may leave the workspace (`/confinement off`).
    Open,
}

impl PostureToken {
    /// The token's display text. Deliberately lowercase: the uppercase form
    /// reads as a second badge competing with `[ROLE]`, and `[N queued]`-style
    /// bracketed caps already mean *count* in the footer stack.
    const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Open => "open",
        }
    }
}

/// The posture tokens a context currently owes the user, in a fixed display
/// order. Empty means "the session is behaving safely" — the overwhelmingly
/// common case, and the one that must cost nothing on the row.
fn posture_tokens(context: &ThreadContext<'_>) -> Vec<PostureToken> {
    let mut tokens = Vec::with_capacity(2);
    if context.unattended {
        tokens.push(PostureToken::Auto);
    }
    if !context.confined {
        tokens.push(PostureToken::Open);
    }
    tokens
}

/// Display width of the posture block including its own inter-token gaps.
///
/// Reserved up-front so the role badge cannot eat into it: posture is safety
/// state, and a token clipped to a prefix (`op`) misreports the session.
fn posture_block_width(tokens: &[PostureToken]) -> usize {
    let labels = tokens.iter().map(|t| t.label().width()).sum::<usize>();
    let gaps = tokens.len().saturating_sub(1) * JOIN_ENUMERATE_COLS;
    labels + gaps
}

/// Truncate a role badge to fit `budget`, keeping the bracket pair intact.
///
/// The brackets are structural — they mark the badge slot, and `visual-language`
/// reserves that shape for it — so a shortened role still reads as a badge
/// (`[SECURITY-AU…]`) instead of degrading into a bare fragment. Below a
/// readable floor the badge is dropped entirely rather than emitted as `[D…]`:
/// a one-letter role identifies nobody, so the columns are better spent on the
/// path. Constants are ASCII-explicit so the math does not depend on the theme.
fn truncate_badge(badge: &str, budget: usize, ellipsis: &str) -> String {
    /// `[]` plus at least three legible inner characters before the ellipsis
    /// (`[DEV…]`). Anything shorter — `[D…]` — names nobody.
    const BADGE_MIN_COLS: usize = 2 + 3;
    const BRACKETS: usize = 2;
    if badge.width() <= budget {
        return badge.to_string();
    }
    if budget < BADGE_MIN_COLS {
        return String::new();
    }
    let inner = badge
        .strip_prefix('[')
        .and_then(|b| b.strip_suffix(']'))
        .unwrap_or(badge);
    format!("[{}]", truncate_with(inner, budget - BRACKETS, ellipsis))
}

/// Draw Tier 3: Sovereign ThreadContextBar (ADR-0062 [INV-HEAD-05]).
///
/// Only rendered on thread-backed viewports (0 rows on non-thread scenes).
///
/// Row grammar, left to right — *who → how → where*:
/// - `[ROLE]` staffing badge, brand tone (absent when no role is staffed).
/// - Posture tokens (`auto`, `open`), warning tone, joined by the peer
///   enumeration gap. Absent entirely in the safe default.
/// - The bound workspace path, right-anchored (dim ancestor directory, crisp stem).
///
/// Anchoring the workspace on the right is a correctness property, not a
/// preference: the path is the only element that may shrink, so pinning it to
/// the right edge keeps the governance cluster (role + posture) at a fixed
/// screen position — toggling `/unattended` can no longer re-truncate the
/// path out from under the reader, and a narrow terminal degrades by
/// shortening the path rather than by stealing columns from safety.
pub(crate) fn draw_thread_context_bar(
    frame: &mut Frame,
    rect: Rect,
    context: &ThreadContext<'_>,
    theme: &Theme,
) {
    let full_width = rect.width as usize;
    if full_width < STEP_MIN_WIDTH || rect.height == 0 {
        return;
    }

    let bg = theme.contextbar_bg();
    let fill = Style::default().bg(bg);
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = full_width.saturating_sub(2 * pad);

    let badge_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);
    let posture_style = fill.fg(theme.warn()).add_modifier(Modifier::BOLD);

    // ---- Left cluster: role badge + posture tokens ----
    // Priority is posture (safety) > role (identity) > path (location). The
    // posture block is budgeted *first* and is never truncated: a clipped
    // `open` reading `op` is a lie about the session's blast radius, which is
    // worse than showing no path at all. The role badge therefore absorbs the
    // pressure, shortening with an ellipsis; the path takes what is left.
    let tokens = posture_tokens(context);
    let posture_w = posture_block_width(&tokens);
    let badge_gap = if tokens.is_empty() {
        0
    } else {
        JOIN_ENUMERATE_COLS
    };
    let badge = context.role.map_or_else(String::new, |role| {
        let full = format!("[{}]", role.to_uppercase());
        truncate_badge(
            &full,
            text_width.saturating_sub(posture_w + badge_gap),
            theme.glyphs.ellipsis,
        )
    });

    let mut left_spans: Vec<Span<'static>> = Vec::new();
    let mut left_width = 0usize;
    if !badge.is_empty() {
        left_width += badge.width();
        left_spans.push(Span::styled(badge, badge_style));
    }
    for token in &tokens {
        let label = token.label();
        if left_width > 0 {
            left_spans.push(Span::styled(" ".repeat(JOIN_ENUMERATE_COLS), fill));
            left_width += JOIN_ENUMERATE_COLS;
        }
        left_width += label.width();
        left_spans.push(Span::styled(label, posture_style));
    }

    // ---- Right cluster: the workspace path (the only shrinkable element) ----
    // The path may claim everything the governance cluster leaves, less the
    // inter-cluster gap. That gap is charged only when the path is actually
    // drawn — otherwise the dropped path's separator would push the row one
    // column past its budget and clip the trailing pad.
    let sep_w = if left_width > 0 && !context.workspace.is_empty() {
        JOIN_ENUMERATE_COLS
    } else {
        0
    };
    let ws_budget = text_width.saturating_sub(left_width + sep_w);
    let ws_spans = workspace_spans(context.workspace, ws_budget, fill, theme);
    let ws_width: usize = ws_spans.iter().map(Span::width).sum();
    let drawn_sep = if ws_width > 0 { sep_w } else { 0 };

    let gap = text_width.saturating_sub(left_width + drawn_sep + ws_width);

    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
    spans.extend(left_spans);
    spans.push(Span::styled(" ".repeat(drawn_sep + gap), fill));
    spans.extend(ws_spans);
    spans.push(Span::styled(" ".repeat(pad), fill));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// Render the workspace path for the right edge of the context bar.
///
/// The path goes through the shared [`PathView`] shortening ladder
/// (`Full` → `.../parent/leaf` → fish → basename → middle-truncate) instead of
/// a plain right-side `truncate_to_width`, for two reasons:
///
/// 1. **Information order.** A tail-truncated path (`~/projects…`) drops the
///    leaf — the part that names the project — while keeping ancestors the
///    user already knows. The ladder keeps the leaf and elides ancestors.
/// 2. **Honesty floor.** [`CONTEXT_BAR_WORKSPACE_MIN_COLS`] is the ladder's own
///    last stop (the bare leaf), so at that width the path still names the
///    project. Below it there is nothing faithful left to draw, and the path is
///    dropped rather than replaced by a fragment that asserts a location it
///    cannot show.
///
/// Ancestor directories are styled dimmed (`theme.dim()`), and the project
/// stem is styled crisp (`theme.fg()`), matching the semantic path language.
fn workspace_spans(
    workspace: &str,
    budget: usize,
    fill: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    if workspace.is_empty() || budget < CONTEXT_BAR_WORKSPACE_MIN_COLS {
        return Vec::new();
    }
    let style = PathStyle::Custom {
        dir: fill.fg(theme.dim()),
        stem: fill.fg(theme.fg()),
        ext: fill.fg(theme.muted()),
        line_col: fill.fg(theme.brand()),
    };
    let view = PathView::from_str(workspace)
        .strategy(PathFormatStrategy::Adaptive)
        .max_width(budget)
        .style(style);
    let marker = theme.glyphs.ellipsis;
    view.to_spans(theme)
        .into_iter()
        .map(|span| {
            let content = if marker == "…" {
                span.content
            } else {
                std::borrow::Cow::Owned(span.content.replace('…', marker))
            };
            Span::styled(content, span.style.bg(fill.bg))
        })
        .collect()
}

/// The scene row's name for the `C-x` scene namespace (ADR-0023, ADR-0043).
const SCENE_NAMESPACE_LABEL: &str = "menu";

/// Disambiguate thread ids by taking their last four characters.
fn id_tail(id: &str) -> &str {
    let len = id.len();
    if len <= 4 { id } else { &id[len - 4..] }
}

/// Truncate `s` to at most `max_width` display columns using the theme's
/// ellipsis glyph.
fn truncate_to_width(s: &str, max_width: usize) -> String {
    truncate_with(s, max_width, "…")
}

/// Truncate `s` to `max_width` columns, appending `ellipsis` when cut.
///
/// The marker comes from the caller rather than being hardcoded so a
/// non-Unicode terminal degrades with the rest of the chrome
/// (`theme.glyphs.ellipsis` is `...` under `ASCII_GLYPHS`, `…` under
/// `UNICODE_GLYPHS`). The result never exceeds `max_width`, marker included.
fn truncate_with(s: &str, max_width: usize, ellipsis: &str) -> String {
    let ellipsis_w = ellipsis.width();
    if max_width == 0 {
        return String::new();
    }
    if s.width() <= max_width {
        return s.to_string();
    }
    // `max_width == 1` with a wide marker has no room for content; emitting the
    // bare marker is still honest about the truncation.
    let budget = max_width.saturating_sub(ellipsis_w);
    let mut out = String::new();
    let mut current_width = 0;
    for ch in s.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if current_width + ch_width > budget {
            break;
        }
        out.push(ch);
        current_width += ch_width;
    }
    out.push_str(ellipsis);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_row1(width: u16, tab_bar: TabBarProps<'_>) -> String {
        let cells = render_row1_cells(width, tab_bar, &Theme::default());
        cells.iter().map(|c| c.symbol()).collect()
    }

    fn render_row1_cells(width: u16, tab_bar: TabBarProps<'_>, theme: &Theme) -> Vec<nuotc::Cell> {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_tab_bar(frame, frame.area(), &tab_bar, theme);
        });
        terminal.buffer().content.clone()
    }

    fn render_row2(width: u16, thread_id: Option<&str>, hints: ViewHints<'_>) -> String {
        let cells = render_row2_cells(width, thread_id, hints, &Theme::default());
        cells.iter().map(|c| c.symbol()).collect()
    }

    fn render_row2_cells(
        width: u16,
        thread_id: Option<&str>,
        hints: ViewHints<'_>,
        theme: &Theme,
    ) -> Vec<nuotc::Cell> {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_scene_bar(frame, frame.area(), thread_id, &hints, theme);
        });
        terminal.buffer().content.clone()
    }

    fn render_row3(width: u16, context: ThreadContext<'_>) -> String {
        let cells = render_row3_cells(width, context, &Theme::default());
        cells.iter().map(|c| c.symbol()).collect()
    }

    fn render_row3_cells(
        width: u16,
        context: ThreadContext<'_>,
        theme: &Theme,
    ) -> Vec<nuotc::Cell> {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_thread_context_bar(frame, frame.area(), &context, theme);
        });
        terminal.buffer().content.clone()
    }

    fn render_row3_with_theme(width: u16, context: ThreadContext<'_>, theme: &Theme) -> String {
        render_row3_cells(width, context, theme)
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn hints<'a>(kind: ViewKind) -> ViewHints<'a> {
        ViewHints::simple(kind)
    }

    #[test]
    fn row1_renders_tabs_without_ordinal_prefixes() {
        let tabs = vec![
            crate::surfaces::ClientTab::thread("sess-01a2b3c4", None::<String>),
            crate::surfaces::ClientTab::dashboard(),
        ];
        let tab_bar = TabBarProps {
            tabs: Some(&tabs),
            active_tab: 0,
        };
        let row = render_row1(80, tab_bar);
        assert!(row.contains("thread#b3c4"), "{row}");
        assert!(
            !row.contains("1:thread#b3c4"),
            "tabs must not use ordinal prefix: {row}"
        );
        assert!(row.contains("dashboard"), "{row}");
        assert!(
            !row.contains("2:dashboard"),
            "tabs must not use ordinal prefix: {row}"
        );
        assert!(
            row.contains("C-x menu"),
            "compact client menu on TabBar: {row}"
        );
    }

    #[test]
    fn row1_renders_semantic_tab_titles_with_wider_budget() {
        let tabs = vec![
            crate::surfaces::ClientTab::thread(
                "sess-01a2b3c4",
                Some("Refactor Database Connection"),
            ),
            crate::surfaces::ClientTab::thread("sess-05d6e7f8", None::<String>),
            crate::surfaces::ClientTab::dashboard(),
        ];
        let tab_bar = TabBarProps {
            tabs: Some(&tabs),
            active_tab: 0,
        };
        let row = render_row1(80, tab_bar);
        assert!(
            row.contains("Refactor Database C…"),
            "semantic title projected with wider budget: {row}"
        );
        assert!(!row.contains("1:Refactor"), "no ordinal prefix: {row}");
        assert!(
            row.contains("thread#e7f8"),
            "untitled tab falls back to placeholder thread#tail: {row}"
        );
        assert!(row.contains("dashboard"), "{row}");
    }

    #[test]
    fn row1_tab_rects_and_styling() {
        let tabs = vec![
            crate::surfaces::ClientTab::thread("sess-01a2b3c4", None::<String>),
            crate::surfaces::ClientTab::dashboard(),
            crate::surfaces::ClientTab::settings(),
        ];
        let tab_bar = TabBarProps {
            tabs: Some(&tabs),
            active_tab: 0,
        };
        let mut terminal = nuotc::TestTerminal::new(80, 1);
        let theme = Theme::default();
        let mut tab_rects = Vec::new();
        terminal.draw(|frame| {
            tab_rects = draw_tab_bar(frame, frame.area(), &tab_bar, &theme);
        });
        assert_eq!(tab_rects.len(), 3);
        assert_eq!(tab_rects[0].0, 0);
        assert_eq!(tab_rects[1].0, 1);
        assert_eq!(tab_rects[2].0, 2);
        assert_eq!(tab_rects[0].1.y, 0);
        assert_eq!(tab_rects[0].1.x, 0, "tab 0 subsumes leading pad");
        assert_eq!(
            tab_rects[1].1.x,
            tab_rects[0].1.right(),
            "tab 1 starts seamlessly where tab 0 ends"
        );
        assert_eq!(
            tab_rects[2].1.x,
            tab_rects[1].1.right(),
            "tab 2 starts seamlessly where tab 1 ends"
        );
        assert!(tab_rects[0].1.width > 0);
        assert!(tab_rects[1].1.x > tab_rects[0].1.x);
    }

    #[test]
    fn row1_renders_brand_when_no_tabs() {
        let tab_bar = TabBarProps::default();
        let row = render_row1(80, tab_bar);
        assert!(row.contains("nuo"), "brand shown: {row}");
        assert!(row.contains("C-x menu"), "client menu shown: {row}");
    }

    #[test]
    fn row2_renders_thread_identity_and_task_title() {
        let row = render_row2(
            80,
            Some("sess-01a2b3c4"),
            ViewHints {
                kind: SceneKind::Thread,
                context: Some("Fix the retry loop"),
                ..hints(SceneKind::Thread)
            },
        );
        assert!(
            row.starts_with("  THREAD b3c4  Fix the retry loop"),
            "{row}"
        );
    }

    #[test]
    fn row2_renders_untitled_thread_without_placeholder() {
        let row = render_row2(80, Some("sess-01a2b3c4"), hints(SceneKind::Thread));
        assert!(row.starts_with("  THREAD b3c4"), "{row}");
        assert!(
            !row.contains("thread#"),
            "clean thread identifier without redundant label: {row}"
        );
    }

    #[test]
    fn row2_renders_non_thread_scene_labels() {
        for (kind, label) in [
            (SceneKind::Subagent, "subagent"),
            (SceneKind::Settings, "settings"),
            (SceneKind::Dashboard, "dashboard"),
        ] {
            assert_eq!(kind.scene_label(), label);
            let row = render_row2(80, None, hints(kind));
            assert!(row.starts_with(&format!("  {label}")), "{kind:?}: {row}");
        }
    }

    #[test]
    fn row2_context_tone_escalates_when_flagged() {
        let theme = Theme::default();
        let first_fg = |cells: &[nuotc::Cell], needle: &str| {
            let idx = cells
                .iter()
                .position(|c| c.symbol() == needle)
                .expect("needle present");
            cells[idx].fg
        };
        let quiet = render_row2_cells(
            80,
            None,
            ViewHints {
                context: Some("main running"),
                ..hints(SceneKind::Dashboard)
            },
            &theme,
        );
        let urgent = render_row2_cells(
            80,
            None,
            ViewHints {
                context: Some("needs approval"),
                context_warn: true,
                ..hints(SceneKind::Dashboard)
            },
            &theme,
        );
        assert_eq!(
            first_fg(&quiet, "m"),
            theme.fg(),
            "quiet context is foreground"
        );
        assert_eq!(
            first_fg(&urgent, "n"),
            theme.warn(),
            "flagged context takes the warning tone"
        );
    }

    #[test]
    fn row2_renders_right_aligned_drill_in_stack() {
        let crumbs = [SceneKind::Thread, SceneKind::Subagent];
        let row = render_row2(
            80,
            None,
            ViewHints {
                kind: SceneKind::Subagent,
                context: Some("[EXPLORE] inspect codebase"),
                context_warn: false,
                breadcrumbs: Some(&crumbs),
                can_back: true,
                can_forward: false,
            },
        );
        assert!(
            row.contains("[EXPLORE] inspect codebase"),
            "shows scene context: {row}"
        );
        assert!(row.contains("‹ Alt+["), "shows back affordance: {row}");
        assert!(
            row.contains("thread › subagent"),
            "shows canonical breadcrumb trail: {row}"
        );
        assert!(
            !row.contains("< thread >"),
            "no pseudo-tag delimiter artifact: {row}"
        );
        let ctx_pos = row.find("[EXPLORE]").unwrap();
        let stack_pos = row.find("‹ Alt+[").unwrap();
        assert!(ctx_pos < stack_pos, "context left, stack right: {row}");

        // With can_forward = true
        let row_fwd = render_row2(
            80,
            None,
            ViewHints {
                kind: SceneKind::Subagent,
                context: Some("[EXPLORE] inspect codebase"),
                context_warn: false,
                breadcrumbs: Some(&crumbs),
                can_back: true,
                can_forward: true,
            },
        );
        assert!(
            row_fwd.contains("Alt+] ›"),
            "shows forward affordance: {row_fwd}"
        );
    }

    #[test]
    fn row2_computes_nav_hit_rects_accurately() {
        let crumbs = [SceneKind::Thread, SceneKind::Subagent];
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(80, 1);
        let mut nav_rects = SceneNavRects::default();
        terminal.draw(|frame| {
            nav_rects = draw_scene_bar(
                frame,
                frame.area(),
                None,
                &ViewHints {
                    kind: SceneKind::Subagent,
                    context: Some("task 1"),
                    context_warn: false,
                    breadcrumbs: Some(&crumbs),
                    can_back: true,
                    can_forward: true,
                },
                &theme,
            );
        });

        assert!(nav_rects.back.is_some());
        assert_eq!(nav_rects.crumbs.len(), 2);
        assert!(nav_rects.forward.is_some());
        let back_rect = nav_rects.back.unwrap();
        assert!(back_rect.width > 0);
        let crumb0 = nav_rects.crumbs[0].1;
        assert!(crumb0.x > back_rect.x);
        let crumb1 = nav_rects.crumbs[1].1;
        assert!(crumb1.x > crumb0.x);
        let fwd_rect = nav_rects.forward.unwrap();
        assert!(fwd_rect.x > crumb1.x);
    }

    #[test]
    fn row2_band_paints_the_full_row_width() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(60, 1);
        terminal.draw(|frame| {
            draw_scene_bar(
                frame,
                frame.area(),
                None,
                &hints(SceneKind::Settings),
                &theme,
            );
        });
        for cell in &terminal.buffer().content {
            assert_eq!(cell.bg, theme.scenebar_bg());
        }
    }

    #[test]
    fn row3_reads_who_how_where_left_to_right() {
        let context = ThreadContext {
            workspace: "~/projects/xx",
            role: Some("developer"),
            unattended: true,
            confined: false,
        };
        let row = render_row3(80, context);
        // Who (`[ROLE]`) → how (`auto`/`open`) → where (workspace, right-anchored).
        assert!(row.starts_with("  [DEVELOPER]  auto  open"), "{row}");
        assert!(row.trim_end().ends_with("~/projects/xx"), "{row}");
    }

    #[test]
    fn row3_reports_only_deviations_from_the_safe_default() {
        // Interactive confirmation + workspace-confined filesystem is the
        // default posture: it costs zero columns.
        let context = ThreadContext {
            workspace: "~/projects/xx",
            role: Some("developer"),
            unattended: false,
            confined: true,
        };
        let row = render_row3(80, context);
        assert!(
            row.starts_with("  [DEVELOPER]  "),
            "no posture tokens: {row}"
        );
        assert!(!row.contains("auto"), "{row}");
        assert!(!row.contains("open"), "{row}");
    }

    #[test]
    fn row3_posture_tokens_are_lowercase_and_unbracketed() {
        // Caps + brackets are already spoken for: `[ROLE]` is the badge slot and
        // `[N queued]` means a count in the footer stack. Posture stays a plain
        // lowercase word so it cannot be misread as either.
        assert_eq!(PostureToken::Auto.label(), "auto");
        assert_eq!(PostureToken::Open.label(), "open");
        for label in [PostureToken::Auto.label(), PostureToken::Open.label()] {
            assert!(
                !label.chars().any(char::is_uppercase) && !label.contains(['[', ']']),
                "{label}"
            );
        }
    }

    #[test]
    fn row3_posture_tokens_join_with_the_peer_enumeration_gap() {
        let context = ThreadContext {
            workspace: "",
            role: Some("developer"),
            unattended: true,
            confined: false,
        };
        let row = render_row3(80, context);
        assert!(
            row.contains("[DEVELOPER]  auto  open"),
            "peer tokens use a plain two-column gap, no glyph: {row}"
        );
    }

    #[test]
    fn row3_workspace_free_hides_workspace_and_shows_role_badge() {
        let context = ThreadContext {
            workspace: "",
            role: Some("philosophist"),
            unattended: false,
            confined: true,
        };
        let row = render_row3(80, context);
        assert!(row.starts_with("  [PHILOSOPHIST]"), "{row}");
        assert!(!row.contains("~/"), "{row}");
    }

    #[test]
    fn row3_shows_custom_user_role_badge() {
        let context = ThreadContext {
            workspace: "~/projects/xx",
            role: Some("security-auditor"),
            unattended: false,
            confined: true,
        };
        let row = render_row3(80, context);
        assert!(row.starts_with("  [SECURITY-AUDITOR]"), "{row}");
    }

    /// `[INV-HEAD-07]` anchor stability: flipping a posture token must not reflow
    /// the *path*, because the path is right-anchored and the governance
    /// cluster grows into the gap between them.
    #[test]
    fn row3_toggling_posture_never_reflow_the_workspace_path() {
        let quiet = ThreadContext {
            workspace: "~/projects/nuo",
            role: Some("developer"),
            unattended: false,
            confined: true,
        };
        let loud = ThreadContext {
            unattended: true,
            confined: false,
            ..quiet
        };
        let quiet_row = render_row3(80, quiet);
        let loud_row = render_row3(80, loud);
        for row in [&quiet_row, &loud_row] {
            assert!(row.trim_end().ends_with("~/projects/nuo"), "{row}");
        }
        // The path sits at the same column in both rows.
        assert_eq!(
            quiet_row.find("~/projects/nuo"),
            loud_row.find("~/projects/nuo"),
            "quiet: {quiet_row}\nloud: {loud_row}"
        );
    }

    /// `[INV-HEAD-07]` safety-first degradation: as the row narrows, the path
    /// shortens while `[ROLE]` and the posture tokens survive intact.
    #[test]
    fn row3_narrow_row_keeps_posture_and_shortens_the_path() {
        let context = ThreadContext {
            workspace: "~/projects/nuo/crates/nuo-tui",
            role: Some("developer"),
            unattended: true,
            confined: false,
        };
        // Wide: the full path survives.
        let wide = render_row3(80, context);
        assert!(wide.contains("~/projects/nuo/crates/nuo-tui"), "{wide}");

        // Narrower: still governed, path shortened from the ancestor end so the
        // leaf — the part that names the project — is what remains.
        let narrow = render_row3(46, context);
        assert!(narrow.contains("[DEVELOPER]  auto  open"), "{narrow}");
        assert!(narrow.contains("nuo-tui"), "leaf survives: {narrow}");
        assert!(
            !narrow.contains("~/projects/nuo/crates"),
            "ancestors elided: {narrow}"
        );
    }

    /// The row must never paint past its own width. Regression guard for the
    /// inter-cluster gap: when the path is dropped the gap must be dropped with
    /// it, or the row overruns by two columns and clips the trailing pad.
    #[test]
    fn row3_never_paints_beyond_the_row_width() {
        let context = ThreadContext {
            workspace: "~/projects/nuo",
            role: Some("developer"),
            unattended: true,
            confined: false,
        };
        for width in 8u16..=120 {
            let row = render_row3(width, context);
            assert_eq!(
                row.chars().count(),
                width as usize,
                "row must fill exactly {width} columns: {row:?}"
            );
        }
    }

    /// Below the honesty floor the path is dropped wholesale: a two-letter
    /// sliver would still claim to be the workspace while naming nothing, so
    /// posture keeps the row.
    #[test]
    fn row3_drops_the_path_rather_than_lying_about_it() {
        let context = ThreadContext {
            workspace: "~/projects/nuo/crates/nuo-tui",
            role: Some("developer"),
            unattended: true,
            confined: false,
        };
        // 30 columns → 26 text columns. The governance cluster is 23 wide, so
        // it survives whole while the path is left a 1-column budget — below
        // the honesty floor, so it is dropped rather than drawn as `…`.
        let row = render_row3(30, context);
        assert!(row.contains("[DEVELOPER]  auto  open"), "{row}");
        assert!(!row.contains("~"), "no fake path: {row}");
        assert!(!row.contains("…"), "no fake path: {row}");
    }

    /// Posture is safety state, so it outranks every other element on the row
    /// (`[INV-HEAD-08]`): a role badge long enough to crowd it must yield, and
    /// the token must never be clipped into a prefix that misreports the session.
    #[test]
    fn row3_posture_outranks_a_long_role_badge() {
        let context = ThreadContext {
            workspace: "~/projects/nuo",
            role: Some("a-very-long-custom-user-role-name"),
            unattended: true,
            confined: false,
        };
        let row = render_row3(44, context);
        assert!(row.contains("auto  open"), "posture intact: {row}");
        assert!(row.contains('…'), "role badge elided instead: {row}");
        assert!(
            !row.contains("A-VERY-LONG-CUSTOM-USER-ROLE-NAME"),
            "full role must yield to safety state: {row}"
        );
    }

    /// A posture token is never emitted as a prefix (`[INV-HEAD-08]`). `open`
    /// clipped to `op` would still be rendered in the warning tone, claiming a
    /// state the session does not have.
    #[test]
    fn row3_never_emits_a_truncated_posture_token() {
        for width in 8u16..=80 {
            let context = ThreadContext {
                workspace: "~/projects/nuo",
                role: Some("security-auditor"),
                unattended: true,
                confined: false,
            };
            let row = render_row3(width, context);
            for fragment in ["aut", "ope"] {
                let clipped =
                    row.contains(fragment) && !row.contains("auto") && !row.contains("open");
                assert!(
                    !clipped,
                    "width {width}: posture token clipped to a prefix: {row:?}"
                );
            }
        }
    }

    /// A non-Unicode terminal must not receive `…`. Both the badge elision and
    /// the path ladder take their marker from `theme.glyphs.ellipsis`, which is
    /// `...` under `ASCII_GLYPHS`.
    ///
    /// Pinned to a width where an elision actually happens: a wider row draws no
    /// marker at all, so asserting its absence there would pass even with the
    /// glyph override removed.
    #[test]
    fn row3_degrades_its_ellipsis_marker_under_ascii_glyphs() {
        let theme = Theme {
            glyphs: nuotc::ASCII_GLYPHS,
            ..Theme::default()
        };
        let context = ThreadContext {
            workspace: "~/projects/nuo/crates/nuo-tui",
            role: Some("security-auditor"),
            unattended: true,
            confined: false,
        };
        // Width 30 elides the role badge, so the marker must come from the theme.
        let row = render_row3_with_theme(30, context, &theme);
        assert!(
            !row.contains('…'),
            "ASCII profile must not draw the Unicode ellipsis: {row:?}"
        );
        assert!(row.contains("..."), "ASCII marker drawn instead: {row:?}");
        assert!(row.contains("auto  open"), "posture intact: {row:?}");
    }

    /// The path ladder's own marker degrades too, even though `PathView`
    /// hardcodes `…` internally: a single deep segment forces
    /// `middle_truncate_filename`, the only ladder stage that emits one.
    #[test]
    fn row3_degrades_the_path_ladder_marker_under_ascii_glyphs() {
        let theme = Theme {
            glyphs: nuotc::ASCII_GLYPHS,
            ..Theme::default()
        };
        let context = ThreadContext {
            workspace: "a-really-long-single-directory-name-with-no-nesting",
            role: None,
            unattended: false,
            confined: true,
        };
        let row = render_row3_with_theme(30, context, &theme);
        assert!(
            !row.contains('…'),
            "ASCII profile must not draw the Unicode ellipsis: {row:?}"
        );
    }

    /// `CONTEXT_BAR_WORKSPACE_MIN_COLS` must be the ladder's own last stop, not
    /// one rung above it. Asserted directly against the budget seam: at exactly
    /// the floor the path still names the project, one column under it there is
    /// nothing faithful left. An earlier floor of 8 silently discarded widths
    /// 4..=7 that the ladder serves correctly.
    ///
    /// This tests the constant itself. A render-width sweep would be a
    /// tautology — it can only rediscover whatever floor is in force.
    #[test]
    fn row3_workspace_floor_matches_the_ladder_capability() {
        let theme = Theme::default();
        let at_floor = workspace_spans(
            "~/projects/nuo",
            CONTEXT_BAR_WORKSPACE_MIN_COLS,
            Style::default(),
            &theme,
        );
        let text: String = at_floor.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            text, "nuo",
            "at the floor the ladder must still name the project"
        );

        let below = workspace_spans(
            "~/projects/nuo",
            CONTEXT_BAR_WORKSPACE_MIN_COLS - 1,
            Style::default(),
            &theme,
        );
        assert!(
            below.is_empty(),
            "below the floor nothing faithful can be drawn"
        );
    }

    #[test]
    fn row3_band_paints_the_full_row_width() {
        let theme = Theme::default();
        let context = ThreadContext {
            workspace: "~/projects/xx",
            role: Some("developer"),
            unattended: false,
            confined: true,
        };
        let mut terminal = nuotc::TestTerminal::new(60, 1);
        terminal.draw(|frame| {
            draw_thread_context_bar(frame, frame.area(), &context, &theme);
        });
        for cell in &terminal.buffer().content {
            assert_eq!(cell.bg, theme.contextbar_bg());
        }
        let row: String = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(row.starts_with("  "), "left pad: {row:?}");
        assert!(row.ends_with("  "), "right pad: {row:?}");
    }

    #[test]
    fn header_tiers_exhibit_distinct_micro_elevation_ladder() {
        let theme = Theme::default();
        assert_ne!(
            theme.tabbar_bg(),
            theme.scenebar_bg(),
            "tier 1 tabbar background must differ from tier 2 scenebar"
        );
        assert_ne!(
            theme.scenebar_bg(),
            theme.contextbar_bg(),
            "tier 2 scenebar background must differ from tier 3 contextbar"
        );
        assert_ne!(
            theme.tabbar_bg(),
            theme.contextbar_bg(),
            "tier 1 tabbar background must differ from tier 3 contextbar"
        );
    }
}
