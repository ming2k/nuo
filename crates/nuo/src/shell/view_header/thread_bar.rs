//! Tier 3: the thread-context bar (role, posture, workspace).

use super::*;

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

pub(crate) enum PostureToken {
    /// Tool calls are approved without asking (`--unattended` / `/unattended on`).
    Auto,
    /// File tools may leave the workspace (`/confinement off`).
    Open,
}

impl PostureToken {
    /// The token's display text. Deliberately lowercase: the uppercase form
    /// reads as a second badge competing with `[ROLE]`, and `[N queued]`-style
    /// bracketed caps already mean *count* in the footer stack.
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

    frame.render_widget(&mut Paragraph::new(Line::from(spans)), rect);
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
pub(crate) fn workspace_spans(
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

/// Disambiguate thread ids by taking their last four characters.
pub(super) fn id_tail(id: &str) -> &str {
    let len = id.len();
    if len <= 4 { id } else { &id[len - 4..] }
}

/// Truncate `s` to at most `max_width` display columns, keeping its head.
pub(super) fn truncate_to_width(s: &str, max_width: usize) -> String {
    truncate_with(s, max_width, "…")
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
