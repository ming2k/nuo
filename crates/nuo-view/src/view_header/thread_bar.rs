//! Single-row Thread SceneBar renderer: governance, authentic title, workspace
//! (ADR-0261 `[INV-HEAD-09]`, `[INV-HEAD-10]`; revised by ADR-0268
//! `[INV-HEAD-11]`, `[INV-HEAD-12]`).

use std::borrow::Cow;

use super::*;

/// One persistent safety posture token of the Thread SceneBar.
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
/// `/auto-approve on|off` and `/confinement on|off` explain themselves. Adding a
/// second explanation surface here would fork that text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PostureToken {
    /// Tool calls are approved without asking (`--auto-approve` / `/auto-approve on`).
    Auto,
    /// File tools may leave the workspace (`/confinement off`).
    Open,
}

impl PostureToken {
    /// The token's display text. Deliberately lowercase: the uppercase form
    /// competes with the uppercase role token (`DEVELOPER`), and `[N queued]`-style
    /// bracketed forms belong to counts and framed-surface chrome.
    pub(super) const fn label(self) -> &'static str {
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

/// Admit whole posture tokens that fit inside `budget` without ever clipping a
/// token into a misleading prefix (`aut`, `ope`, `op`).
fn fit_posture_tokens(candidates: Vec<PostureToken>, budget: usize) -> (Vec<PostureToken>, usize) {
    let mut admitted = Vec::with_capacity(candidates.len());
    let mut used = 0usize;
    for token in candidates {
        let gap = if admitted.is_empty() {
            0
        } else {
            JOIN_ENUMERATE_COLS
        };
        let needed = gap + token.label().width();
        if used + needed <= budget {
            admitted.push(token);
            used += needed;
        }
    }
    (admitted, used)
}

/// Truncate a role token to fit `budget` (ADR-0268 `[INV-HEAD-11]`).
///
/// On chromatic surfaces, roles render bare (`DEVELOPER` → `SECURITY-AU…`) with
/// a 4-column legible floor (`DEV…`). On framed surfaces, intact brackets
/// preserve structural demarcation (`[DEVELOPER]` → `[SECURITY-AU…]`) with a
/// 5-column floor (`[DEV…]`). Below the legible floor the token is dropped
/// completely rather than emitting a single-letter fragment that names nobody.
pub(super) fn truncate_role_token(
    role: &str,
    budget: usize,
    chromatic: bool,
    ellipsis: &str,
) -> String {
    /// Minimum legible inner prefix before the ellipsis (`DEV…` / `[DEV…]`).
    const INNER_MIN_COLS: usize = 3;
    const BRACKETS: usize = 2;

    let upper = role.to_uppercase();
    if chromatic {
        let min_cols = INNER_MIN_COLS + 1;
        if upper.width() <= budget {
            return upper;
        }
        if budget < min_cols {
            return String::new();
        }
        truncate_with(&upper, budget, ellipsis)
    } else {
        let min_cols = BRACKETS + INNER_MIN_COLS;
        let full = format!("[{upper}]");
        if full.width() <= budget {
            return full;
        }
        if budget < min_cols {
            return String::new();
        }
        format!("[{}]", truncate_with(&upper, budget - BRACKETS, ellipsis))
    }
}

/// Draw the single-row Tier 2 SceneBar for `SceneKind::Thread`
/// (ADR-0261 `[INV-HEAD-09]`, `[INV-HEAD-10]`; revised by ADR-0268
/// `[INV-HEAD-11]`, `[INV-HEAD-12]`).
///
/// Row grammar, left to right:
/// - Staffed role token: bare uppercase brand bold (`DEVELOPER`) on chromatic
///   surfaces, bracketed (`[DEVELOPER]`) on non-chromatic framed surfaces
///   (absent when no role is staffed).
/// - Silent-by-default posture tokens (`auto`, `open`), warning bold, joined by
///   `JOIN_ENUMERATE_COLS`.
/// - Silent-when-untitled authentic task title slot: empty until an authentic
///   title exists, then painted bold heading in place with zero displacement of
///   the left governance cluster or right-anchored workspace.
/// - Right-anchored bound workspace path (`PathView` ancestor-first shortening).
///
/// Priority on the row is **posture (whole tokens only) > role (legible floor
/// or omitted) > fair-share title/workspace with slack redistribution**.
pub(super) fn draw_thread_scene_bar(
    frame: &mut Frame,
    rect: Rect,
    title: Option<&str>,
    context: Option<&ThreadContext<'_>>,
    theme: &Theme,
) {
    let full_width = rect.width as usize;
    if full_width < STEP_MIN_WIDTH || rect.height == 0 {
        return;
    }

    let bg = theme.scenebar_bg();
    let fill = Style::default().bg(bg);
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = full_width.saturating_sub(2 * pad);
    let chromatic = theme.elevation.is_chromatic();

    let role_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);
    let posture_style = fill.fg(theme.warn()).add_modifier(Modifier::BOLD);
    let title_style = fill.fg(theme.heading()).add_modifier(Modifier::BOLD);

    // ---- 1. Governance cluster: posture (safety) > role (identity) ----
    let raw_tokens = context.map(posture_tokens).unwrap_or_default();
    let (tokens, posture_w) = fit_posture_tokens(raw_tokens, text_width);
    let role_gap = if tokens.is_empty() {
        0
    } else {
        JOIN_ENUMERATE_COLS
    };
    let role_token = context
        .and_then(|c| c.role)
        .map_or_else(String::new, |role| {
            truncate_role_token(
                role,
                text_width.saturating_sub(posture_w + role_gap),
                chromatic,
                theme.glyphs.ellipsis,
            )
        });

    let mut left_spans: Vec<Span<'static>> = Vec::new();
    let mut left_width = 0usize;
    if !role_token.is_empty() {
        left_width += role_token.width();
        left_spans.push(Span::styled(role_token, role_style));
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

    // ---- 2. Silent-until-titled task title + right-anchored workspace ----
    let authentic_title = title.map(str::trim).filter(|t| !t.is_empty());

    let workspace = context.map_or("", |c| c.workspace);
    let has_ws = !workspace.is_empty();
    let ws_full_w = workspace.width();

    let gov_sep = if left_width > 0 {
        JOIN_ENUMERATE_COLS
    } else {
        0
    };
    let shared = text_width.saturating_sub(left_width + gov_sep);

    let (drawn_title, ws_spans) = allocate_title_and_workspace(
        authentic_title,
        workspace,
        has_ws,
        ws_full_w,
        shared,
        fill,
        theme,
    );

    if let Some(title_str) = drawn_title {
        if left_width > 0 {
            left_spans.push(Span::styled(" ".repeat(JOIN_ENUMERATE_COLS), fill));
            left_width += JOIN_ENUMERATE_COLS;
        }
        left_width += title_str.width();
        left_spans.push(Span::styled(title_str, title_style));
    }

    let ws_width: usize = ws_spans.iter().map(Span::width).sum();
    let gap = text_width.saturating_sub(left_width + ws_width);

    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
    spans.extend(left_spans);
    spans.push(Span::styled(" ".repeat(gap), fill));
    spans.extend(ws_spans);
    spans.push(Span::styled(" ".repeat(pad), fill));

    frame.render_widget(&mut Paragraph::new(Line::from(spans)), rect);
}

/// Distribute `shared` columns between the optional authentic title and the
/// right-anchored workspace path using fair-share with slack redistribution
/// (ADR-0261 `[INV-HEAD-10]`, ADR-0268 `[INV-HEAD-12]`).
///
/// When `title` is `None` (untitled thread), the title slot spends zero columns
/// and the entire `shared` budget flows directly to `workspace`.
fn allocate_title_and_workspace(
    title: Option<&str>,
    workspace: &str,
    has_ws: bool,
    ws_full_w: usize,
    shared: usize,
    fill: Style,
    theme: &Theme,
) -> (Option<String>, Vec<Span<'static>>) {
    if shared == 0 {
        return (None, Vec::new());
    }

    let Some(title_text) = title else {
        let ws_spans = if has_ws {
            workspace_spans(workspace, shared, fill, theme)
        } else {
            Vec::new()
        };
        return (None, ws_spans);
    };

    let title_full_w = title_text.width();

    if !has_ws {
        let drawn = fit_title(title_text, title_full_w, shared, theme.glyphs.ellipsis);
        return (drawn, Vec::new());
    }

    let mid_sep = JOIN_ENUMERATE_COLS;

    // Relaxed fit: both elements fit completely with room for the inter-cluster gap.
    // Workspace stays pinned to the right edge so posture toggles never shift it.
    if title_full_w + mid_sep + ws_full_w <= shared {
        let ws_budget = shared - title_full_w - mid_sep;
        let ws_spans = workspace_spans(workspace, ws_budget, fill, theme);
        return (Some(title_text.to_string()), ws_spans);
    }

    // Shared pressure: both can coexist above their honesty floors.
    let usable = shared.saturating_sub(mid_sep);
    if usable >= SCENE_BAR_WORKSPACE_MIN_COLS + SCENE_BAR_IDENTITY_MIN_COLS {
        let ws_share = (usable / 3).max(SCENE_BAR_WORKSPACE_MIN_COLS);
        let title_share = usable - ws_share;
        let ws_budget = if title_full_w < title_share {
            // Title does not use its full share: flow slack to workspace.
            usable - title_full_w
        } else if ws_full_w < ws_share {
            // Workspace fits inside its share; flow its remaining slack to title.
            ws_full_w
        } else {
            ws_share
        };

        let ws_spans = workspace_spans(workspace, ws_budget, fill, theme);
        let ws_actual_w: usize = ws_spans.iter().map(Span::width).sum();
        let title_budget = if ws_actual_w > 0 {
            usable.saturating_sub(ws_actual_w)
        } else {
            shared
        };
        let drawn = fit_title(
            title_text,
            title_full_w,
            title_budget,
            theme.glyphs.ellipsis,
        );
        return (drawn, ws_spans);
    }

    // Extreme pressure: only one element can fit honestly. Because Tier 1 TabBar
    // already projects the active thread tab title, prefer the workspace leaf.
    let ws_spans = workspace_spans(workspace, shared, fill, theme);
    if !ws_spans.is_empty() {
        return (None, ws_spans);
    }
    let drawn = fit_title(title_text, title_full_w, shared, theme.glyphs.ellipsis);
    (drawn, Vec::new())
}

fn fit_title(
    title_text: &str,
    title_full_w: usize,
    budget: usize,
    ellipsis: &str,
) -> Option<String> {
    if budget == 0 || (title_full_w > budget && budget < SCENE_BAR_IDENTITY_MIN_COLS) {
        return None;
    }
    let truncated = truncate_with(title_text, budget, ellipsis);
    (!truncated.is_empty()).then_some(truncated)
}

/// Render the workspace path for the right edge of the Thread SceneBar.
///
/// The path goes through the shared [`PathView`] shortening ladder
/// (`Full` → `.../parent/leaf` → fish → basename → middle-truncate) instead of
/// a plain right-side `truncate_to_width`, for two reasons:
///
/// 1. **Information order.** A tail-truncated path (`~/projects…`) drops the
///    leaf — the part that names the project — while keeping ancestors the
///    user already knows. The ladder keeps the leaf and elides ancestors.
/// 2. **Honesty floor.** [`SCENE_BAR_WORKSPACE_MIN_COLS`] is the ladder's own
///    last stop (the bare leaf), so at that width the path still names the
///    project. Below it there is nothing faithful left to draw, and the path is
///    dropped rather than replaced by a fragment that asserts a location it
///    cannot show.
///
/// Ancestor directories are styled dimmed (`theme.dim()`), and the project
/// stem is styled crisp (`theme.fg()`), matching the semantic path language.
pub(crate) fn workspace_spans(
    workspace: &str,
    budget: usize,
    fill: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    if workspace.is_empty()
        || budget == 0
        || (workspace.width() > budget && budget < SCENE_BAR_WORKSPACE_MIN_COLS)
    {
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
                Cow::Owned(span.content.replace('…', marker))
            };
            Span::styled(content, span.style.bg(fill.bg))
        })
        .collect()
}

/// Truncate `s` to `max_width` columns, appending `ellipsis` when cut.
///
/// The marker comes from the caller rather than being hardcoded so a
/// non-Unicode terminal degrades with the rest of the chrome
/// (`theme.glyphs.ellipsis` is `...` under `ASCII_GLYPHS`, `…` under
/// `UNICODE_GLYPHS`). The result never exceeds `max_width`, marker included.
///
/// The mechanics are the substrate's (ADR-0152 `[INV-TEXT-OVERFLOW-01]`): this
/// alias exists only to thread the caller's marker through.
pub(super) fn truncate_with(s: &str, max_width: usize, ellipsis: &str) -> String {
    nuo_ui_primitives::components::overflow::fit_with(
        s,
        max_width,
        nuo_ui_primitives::components::overflow::Overflow::End,
        ellipsis,
    )
    .text
}
