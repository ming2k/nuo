//! Empty-state hero shown in place of the transcript when a session holds no
//! messages yet.
//!
//! This is a **replacement** for the transcript stream, not content rendered
//! *inside* it: `draw_transcript` short-circuits to this component when
//! `messages` is empty (and no subagent/side view is open), keeping the empty
//! state out of the message-rendering pipeline entirely. Responsibilities stay
//! clean — the empty state never participates in scroll, selection, or
//! attribution logic.
//!
//! The footer (input box, status bar, hint bar) renders exactly as in a live
//! session, so the user lands in a familiar composer immediately.
//!
//! Beneath the logo the hero carries a **capability carousel** (ADR-0104): one
//! durable capability hint at a time (`/btw`, `Ctrl-R`, `!` shell,
//! …) rotating on a wall-clock cadence, one line at a time (no position
//! indicator — the copy is self-explaining) and nothing else — the static
//! "type a message" tagline is retired, since the carousel's own first page
//! already answers "how do I start". It is the
//! same "teach quietly, without a manual" slot that can later host other
//! contextual tours. A missing provider replaces the carousel with a pinned
//! setup blocker (ADR-0057) — nothing rotates until the blocker clears.
//!
//! The logo source is pluggable: a caller may pass user-supplied lines (loaded
//! from `$XDG_CONFIG_HOME/muta/logo.txt`); when absent the built-in figlet
//! wordmark is used. Either way the art is clamped to a safe bounding box so a
//! giant paste can never blow out the welcome screen.

use nuotc::{
    Alignment, Frame, Paragraph, Rect, {Line, Span}, {Modifier, Style},
};

use super::theme::Theme;
#[cfg(test)]
use unicode_width::UnicodeWidthStr;

/// Hard width cap (in terminal columns) for any logo line. A wider line is
/// truncated at a character boundary. 60 keeps comfortable side margins inside
/// an 80-column terminal while leaving room for the guidance line beneath.
pub(crate) const MAX_LOGO_COLS: usize = 60;

/// Hard height cap (in rows) for the logo block. More lines than this are
/// dropped from the bottom. 20 leaves the welcome screen readable on a
/// 24-row terminal even before vertical centering.
pub(crate) const MAX_LOGO_ROWS: usize = 20;

/// How long one carousel page stays on screen before rotating to the next
/// (ADR-0104). Long enough to read a one-line hint at a glance, short
/// enough that a user who lingers on the empty state sees several pages.
/// The index is derived from wall-clock elapsed time (`carousel_epoch`), so
/// the cadence stays constant regardless of how often the loop redraws.
pub(crate) const CAROUSEL_SLIDE_SECS: u64 = 8;

/// One rotating capability page beneath the logo (ADR-0104). Each page is one
/// centered line: a muted lead sentence followed by keycap/`command` tokens
/// picked out in the theme's info tone, so the actionable part of each hint
/// reads as an affordance the way the hint bar's `◆ effort` tag does.
///
/// The copy is intentionally **durable**: every page teaches a capability
/// that remains true for the life of the product (send, queue, asides,
/// history, models, shell escape), never a transient state. The list
/// is static — there is nothing session-specific to compute — and new pages
/// can be appended freely: the modulo rotation is derived from the slice
/// length, so no other code needs to know the count.
pub(crate) fn carousel_pages() -> Vec<CarouselPage> {
    use CarouselToken as Tok;
    vec![
        CarouselPage {
            lead: "Send a message, or type ",
            tokens: vec![Tok::Key("/"), Tok::Text(" for commands")],
        },
        CarouselPage {
            lead: "Mid-round, Enter ",
            tokens: vec![Tok::Text("queues it")],
        },
        CarouselPage {
            lead: "Start a background side chat with ",
            tokens: vec![Tok::Key("/btw")],
        },
        CarouselPage {
            lead: "Recall what you typed with ",
            tokens: vec![Tok::Key(crate::keymap::Key::CTRL_R.display())],
        },
        CarouselPage {
            lead: "Switch models with ",
            tokens: vec![
                Tok::Key(crate::keymap::Key::CTRL_M.display()),
                Tok::Text(" or "),
                Tok::Key("/models"),
            ],
        },
        CarouselPage {
            lead: "Mention files with ",
            tokens: vec![Tok::Key("@"), Tok::Text(" — tab completes")],
        },
    ]
}

/// A styled token inside a carousel page: a keycap/command affordance or a
/// plain-text connective.
enum CarouselToken {
    /// Keycap / command / prefix affordance — rendered in the info tone
    /// (matching how the hint bar highlights actionable tokens).
    Key(&'static str),
    /// Plain connective text in the muted tone.
    Text(&'static str),
}

/// One page of the empty-state capability carousel.
pub struct CarouselPage {
    lead: &'static str,
    tokens: Vec<CarouselToken>,
}

impl CarouselPage {
    /// Render the page as one styled line: `lead tokens…` with the keycap
    /// tokens in the info tone and connectives muted.
    fn line(&self, theme: &Theme) -> Line<'static> {
        let muted = Style::default().fg(theme.muted());
        let key_style = theme.keycap_style();
        let mut spans = vec![Span::styled(self.lead, muted)];
        for token in &self.tokens {
            match token {
                CarouselToken::Key(key) => spans.push(Span::styled(*key, key_style)),
                CarouselToken::Text(text) => spans.push(Span::styled(*text, muted)),
            }
        }
        Line::from(spans)
    }

    /// Display width of the rendered line, used to keep every page inside
    /// the minimum terminal width (asserted by the tests; see
    /// `carousel_pages_fit_the_minimum_terminal_width`).
    #[cfg(test)]
    fn width(&self) -> usize {
        self.lead.width()
            + self
                .tokens
                .iter()
                .map(|token| match token {
                    CarouselToken::Key(key) => key.width(),
                    CarouselToken::Text(text) => text.width(),
                })
                .sum::<usize>()
    }
}

/// The built-in wordmark, rendered when no user logo is supplied (an 11-row
/// isometric 3D "nuo" banner).
/// 42 columns wide — compact enough to fit an 80-column terminal with room for
/// the guidance line beneath, while still reading as a logo at a glance
/// rather than competing with the transcript that will replace it.
const BUILTIN_LOGO: &[&str] = &[
    r#"      ___           ___           ___     "#,
    r#"     /\__\         /\__\         /\  \    "#,
    r#"    /::|  |       /:/  /        /::\  \   "#,
    r#"   /:|:|  |      /:/  /        /:/\:\  \  "#,
    r#"  /:/|:|  |__   /:/  /  ___   /:/  \:\  \ "#,
    r#" /:/ |:| /\__\ /:/__/  /\__\ /:/__/ \:\__\"#,
    r#" \/__|:|/:/  / \:\  \ /:/  / \:\  \ /:/  /"#,
    r#"     |:/:/  /   \:\  /:/  /   \:\  /:/  / "#,
    r#"     |::/  /     \:\/:/  /     \:\/:/  /  "#,
    r#"     /:/  /       \::/  /       \::/  /   "#,
    r#"     \/__/         \/__/         \/__/    "#,
];

/// Parse a raw logo file into a display-safe line vector, enforcing the
/// `MAX_LOGO_COLS` × `MAX_LOGO_ROWS` bounding box:
///
/// - Lines are split on `\n`; trailing `\r` (CRLF files) is stripped.
/// - Each line is truncated to `MAX_LOGO_COLS` chars.
/// - Leading/trailing blank lines are dropped (so a trailing newline doesn't
///   waste a centered row).
/// - At most `MAX_LOGO_ROWS` lines are kept (excess dropped from the bottom).
///
/// Returns `None` when the input yields no visible lines, so the caller falls
/// back to the built-in logo rather than rendering nothing.
pub fn parse_logo(raw: &str) -> Option<Vec<String>> {
    let mut lines: Vec<String> = raw
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .map(|l| truncate_chars(l, MAX_LOGO_COLS))
        .skip_while(|l| l.trim().is_empty())
        .collect();
    // Trim trailing blank lines.
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines.truncate(MAX_LOGO_ROWS);
    if lines.is_empty() { None } else { Some(lines) }
}

/// Truncate a string to at most `max` display chars (by Unicode scalar value,
/// not graphemes — terminals are cell-oriented and ASCII art is the target
/// use case). Does not split multi-byte chars.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Compute the height the empty state occupies for a given logo + guidance,
/// without drawing. Lets the transcript renderer keep its `content_lines`
/// accounting honest so the app loop does not treat an empty session as a
/// zero-height stream. `logo` is the effective lines (user-supplied or
/// built-in); `guidance` adds the lines beneath the gap (the carousel's
/// current page or the pinned blocker).
fn empty_state_height(logo: &[&str], guidance: EmptyStateGuidance) -> usize {
    logo.len() + 2 // logo rows + blank gap before the guidance section
        + guidance_line_count(guidance)
}

/// Resolve the effective logo lines: user-supplied lines when present,
/// otherwise the built-in wordmark.
fn effective_logo(user_logo: Option<&[String]>) -> Vec<&str> {
    if let Some(lines) = user_logo
        && !lines.is_empty()
    {
        return lines.iter().map(String::as_str).collect();
    }
    BUILTIN_LOGO.to_vec()
}

/// Which guidance variant the empty-state hero shows beneath the logo.
///
/// The empty state is a **contextual** surface (ADR-0057): it adapts to
/// whether the user has a working LLM configured, rather than showing every
/// user the same static line. The variants are mutually exclusive and
/// ordered by priority — a missing provider beats everything, since nothing
/// else matters until the blocker clears.
///
/// - [`Self::NeedsProvider`] replaces the carousel with a static
///   setup blocker: a message the user must act on before anything else is
///   useful, so it stays pinned instead of rotating.
/// - [`Self::Tour`] (the default) shows the rotating capability carousel
///   (ADR-0104).
///
/// The app shell selects the variant; the view layer owns the copy and
/// styling. This keeps the policy (when to nudge) in the shell and the
/// presentation (what the nudge looks like) in the renderer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EmptyStateGuidance {
    /// The rotating tour: the capability carousel alone. The default — a calm
    /// landing strip that teaches one durable capability at a time instead
    /// of a recurring billboard.
    #[default]
    Tour,
    /// No usable LLM provider is configured. Steers the user to `/connections`
    /// before they type, since a message sent against the mock provider goes
    /// nowhere. This is a real setup blocker, not onboarding — it clears the
    /// moment a keyed provider exists.
    NeedsProvider,
    /// A prior session is currently being loaded / resumed.
    LoadingSession(String),
}

impl EmptyStateGuidance {
    /// Whether this variant rotates its capability pages (ADR-0104). The setup
    /// blocker stays pinned — it is an action item, not a tour stop.
    #[cfg(test)]
    pub fn is_carousel(&self) -> bool {
        matches!(self, Self::Tour)
    }
}

/// Number of text lines the guidance section renders beneath the logo + gap.
/// Used by [`empty_state_content_lines`] to keep the app loop's scroll
/// accounting honest without needing a `Theme` — the count is variant-only,
/// never wrap-dependent (every guidance line fits the minimum terminal width;
/// the carousel contributes exactly its current page line).
fn guidance_line_count(guidance: EmptyStateGuidance) -> usize {
    match guidance {
        // the current carousel page (no static tagline above it — page 0
        // already teaches "send a message or /")
        EmptyStateGuidance::Tour => CAROUSEL_LINES,
        EmptyStateGuidance::NeedsProvider => 2, // blocker + action
        EmptyStateGuidance::LoadingSession(_) => 1,
    }
}

/// Rows the carousel itself renders beneath the logo: the current page
/// line. No tagline above it (page 0 already teaches "send a message or /")
/// and no indicator row (ADR-0104) — the rotating copy is self-explaining.
const CAROUSEL_LINES: usize = 1;

/// Resolve the carousel page to show for a wall-clock `elapsed` since the
/// carousel epoch (ADR-0104). The index is `(elapsed / slide) % pages`, so
/// the cadence is steady regardless of draw frequency (the same property the
/// breathing indicator relies on via `spinner_epoch`), and any terminal
/// size sees the full set over time. Falls back to the first page for an
/// empty set, which `carousel_pages()` never produces.
pub(crate) fn carousel_page_for(elapsed_ms: u128) -> usize {
    let pages = carousel_pages().len().max(1);
    ((elapsed_ms / (CAROUSEL_SLIDE_SECS as u128 * 1000)) as usize) % pages
}

/// Build the carousel block for page `index`: the page line only. No dot
/// indicator — the rotating copy is self-explaining (each page carries its
/// own affordance), and an indicator row would spend a second row of chrome
/// restating "this line changes", information the user cannot act on. The
/// hero stays a minimal landing strip (ADR-0104).
fn carousel_lines(index: usize, theme: &Theme) -> Vec<Line<'static>> {
    let pages = carousel_pages();
    let index = index.min(pages.len().saturating_sub(1));
    vec![pages[index].line(theme)]
}

/// Build the styled text lines for the guidance section. The logo + blank gap
/// are rendered by [`draw_empty_state`]; this owns only the copy beneath. The
/// `info` tone highlights actionable tokens (`/connections`, keycaps, …) so the
/// next step reads as an affordance, not ambient text — mirroring how the hint
/// bar uses `info` for the `◆ effort` tag.
///
/// `carousel_index` selects the carousel page for the [`EmptyStateGuidance::Tour`]
/// variant (ADR-0104); it is ignored by the pinned variants.
fn guidance_section(
    guidance: EmptyStateGuidance,
    carousel_index: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let muted = Style::default().fg(theme.muted());
    let info = Style::default().fg(theme.info());
    let warn = Style::default().fg(theme.warn());
    match guidance {
        // The tour is the carousel alone: page 0 already answers "how do I
        // start" ("Send a message, or type / for commands"), so the old
        // static tagline beneath the logo duplicated it and is retired.
        EmptyStateGuidance::Tour => carousel_lines(carousel_index, theme),
        EmptyStateGuidance::NeedsProvider => vec![
            Line::from(vec![Span::styled(
                "No LLM provider is configured yet.",
                warn,
            )]),
            Line::from(vec![
                Span::styled("Run ", muted),
                Span::styled("/connections", info),
                Span::styled(" to set one up.", muted),
            ]),
        ],
        EmptyStateGuidance::LoadingSession(ref id) => vec![Line::from(vec![
            Span::styled("Loading session ", muted),
            Span::styled(id.clone(), info),
            Span::styled("…", muted),
        ])],
    }
}

/// Draw the empty-state hero centered in `area`. Paints nothing outside the
/// given rect.
///
/// `user_logo` — when `Some` and non-empty, replaces the built-in wordmark.
/// The caller is responsible for having loaded + parsed it (clamped here as a
/// safety net regardless).
///
/// `guidance` selects the copy shown beneath the logo+gap (ADR-0057): the
/// rotating tour (ADR-0104) or the pinned provider blocker. It is the app
/// shell's policy decision — the view layer only renders what it is given.
///
/// `carousel_index` picks the tour page (see [`carousel_page_for`]); the
/// caller derives it from wall-clock elapsed time.
pub(crate) fn draw_empty_state(
    frame: &mut Frame,
    area: Rect,
    user_logo: Option<&[String]>,
    guidance: EmptyStateGuidance,
    carousel_index: usize,
    theme: &Theme,
) {
    // If the user logo somehow slipped through un-clamped, clamp it here too so
    // rendering stays within bounds even if a caller bypassed `parse_logo`.
    let user_clamped: Option<Vec<String>> =
        user_logo.map(|lines| parse_logo(&lines.join("\n")).unwrap_or_default());
    let logo_refs: Vec<&str> = if let Some(ref clamped) = user_clamped {
        if !clamped.is_empty() {
            clamped.iter().map(String::as_str).collect()
        } else {
            BUILTIN_LOGO.to_vec()
        }
    } else {
        BUILTIN_LOGO.to_vec()
    };

    let logo_fg = theme.brand();
    let logo_style = Style::default().fg(logo_fg).add_modifier(Modifier::BOLD);

    let section = guidance_section(guidance, carousel_index, theme);

    let mut lines: Vec<Line> = Vec::with_capacity(logo_refs.len() + 2 + section.len());
    for row in &logo_refs {
        lines.push(Line::from(vec![Span::styled(*row, logo_style)]));
    }
    // Blank gap, then the guidance section (carousel page or blocker).
    lines.push(Line::raw(""));
    lines.extend(section);

    // Center vertically: push the whole block down by half the slack so it sits
    // roughly in the middle of the viewport rather than pinned to the top.
    let slack = area.height.saturating_sub(lines.len() as u16) / 2;
    let top = area.y + slack;

    let para = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(
        para,
        Rect::new(area.x, top, area.width, area.height - slack),
    );
}

/// Height the empty state reports for `content_lines` accounting. Uses the
/// user logo's line count when supplied (clamped), else the built-in height,
/// plus the guidance section's line count.
pub(crate) fn empty_state_content_lines(
    user_logo: Option<&[String]>,
    guidance: EmptyStateGuidance,
) -> usize {
    let refs = effective_logo(user_logo);
    empty_state_height(&refs, guidance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_renders_builtin_without_panicking() {
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        let theme = Theme::default();
        terminal.draw(|f| {
            draw_empty_state(f, f.area(), None, EmptyStateGuidance::Tour, 0, &theme);
        });
    }

    #[test]
    fn empty_state_renders_user_logo_without_panicking() {
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        let theme = Theme::default();
        let logo = vec!["  X X  ".to_string(), " X X X ".to_string()];
        terminal.draw(|f| {
            draw_empty_state(
                f,
                f.area(),
                Some(&logo),
                EmptyStateGuidance::Tour,
                3,
                &theme,
            );
        });
    }

    #[test]
    fn empty_state_renders_needs_provider_guidance_without_panicking() {
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        let theme = Theme::default();
        terminal.draw(|f| {
            draw_empty_state(
                f,
                f.area(),
                None,
                EmptyStateGuidance::NeedsProvider,
                0,
                &theme,
            );
        });
    }

    #[test]
    fn empty_state_renders_loading_session_guidance() {
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        let theme = Theme::default();
        terminal.draw(|f| {
            draw_empty_state(
                f,
                f.area(),
                None,
                EmptyStateGuidance::LoadingSession("7c405d7e".to_string()),
                0,
                &theme,
            );
        });
        let output: String = terminal
            .buffer()
            .rows()
            .iter()
            .map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            output.contains("Loading session 7c405d7e…"),
            "must render loading session indicator: {output}"
        );
    }

    #[test]
    fn carousel_rotates_on_a_wall_clock_cadence() {
        let pages = carousel_pages().len();
        assert!(pages >= 4, "the tour must teach several capabilities");
        // Slide boundaries: within one slide the page holds, then advances.
        assert_eq!(carousel_page_for(0), 0);
        assert_eq!(carousel_page_for(CAROUSEL_SLIDE_SECS as u128 * 1000 - 1), 0);
        assert_eq!(carousel_page_for(CAROUSEL_SLIDE_SECS as u128 * 1000), 1);
        // Full-cycle wrap.
        let cycle = CAROUSEL_SLIDE_SECS as u128 * 1000 * pages as u128;
        assert_eq!(carousel_page_for(cycle), 0);
        assert_eq!(carousel_page_for(cycle + 1), 0);
    }

    #[test]
    fn carousel_pages_fit_the_minimum_terminal_width() {
        // MIN_TERMINAL_COLS = 40: every page must stay on one centered line
        // even there, or the copy would wrap and break the height accounting
        // (`guidance_line_count` is wrap-independent by contract).
        for page in carousel_pages() {
            assert!(
                page.width() <= 40,
                "carousel page too wide ({} cols): {:?}",
                page.width(),
                page.lead
            );
        }
    }

    #[test]
    fn tour_carousel_renders_only_the_current_page_line() {
        let theme = Theme::default();
        let lines = guidance_section(EmptyStateGuidance::Tour, 1, &theme);
        // Just the page line — no static tagline above it, no indicator row.
        assert_eq!(lines.len(), CAROUSEL_LINES);
        let page_line: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.clone().into_owned())
            .collect();
        assert!(
            page_line.contains(carousel_pages()[1].lead),
            "page 1 shows its lead: {page_line}"
        );
        // No dot indicator anywhere (ADR-0104): the rotation is
        // self-explaining, so no chrome restates it.
        let joined = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.clone().into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!joined.contains('●'), "no dot indicator: {joined}");
    }

    #[test]
    fn needs_provider_does_not_rotate() {
        assert!(!EmptyStateGuidance::NeedsProvider.is_carousel());
        let theme = Theme::default();
        let lines = guidance_section(EmptyStateGuidance::NeedsProvider, 5, &theme);
        assert_eq!(lines.len(), 2, "blocker + action only: {lines:?}");
    }

    #[test]
    fn parse_logo_truncates_wide_lines() {
        let wide = "a".repeat(200);
        let out = parse_logo(&wide).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].chars().count(), MAX_LOGO_COLS);
    }

    #[test]
    fn parse_logo_truncates_tall_blocks() {
        let tall = (0..MAX_LOGO_ROWS + 50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = parse_logo(&tall).unwrap();
        assert_eq!(out.len(), MAX_LOGO_ROWS);
    }

    #[test]
    fn parse_logo_strips_crlf_and_trailing_blanks() {
        let raw = "\r\nhello\r\nworld\r\n\r\n";
        let out = parse_logo(raw).unwrap();
        assert_eq!(out, vec!["hello", "world"]);
    }

    #[test]
    fn parse_logo_returns_none_for_empty_input() {
        assert!(parse_logo("").is_none());
        assert!(parse_logo("\n\n\n").is_none());
        assert!(parse_logo("   \n  \n").is_none());
    }

    #[test]
    fn builtin_height_matches_logo_plus_gap() {
        let refs = effective_logo(None);
        assert_eq!(
            empty_state_height(&refs, EmptyStateGuidance::Tour),
            BUILTIN_LOGO.len() + 2 + CAROUSEL_LINES
        );
    }

    #[test]
    fn content_lines_reflects_user_logo_size() {
        let user = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        // Tour guidance: 3 logo + 2 (blank gap) + 1 carousel page = 6.
        assert_eq!(
            empty_state_content_lines(Some(&user), EmptyStateGuidance::Tour),
            6
        );
        assert_eq!(
            empty_state_content_lines(None, EmptyStateGuidance::Tour),
            BUILTIN_LOGO.len() + 2 + CAROUSEL_LINES
        );
    }

    #[test]
    fn content_lines_reflects_guidance_variant() {
        // Same logo, two guidance tiers — tour (one page line) and blocker
        // (blocker + action). The blocker is one row taller.
        let base_logo = None::<&[String]>;
        let tour = empty_state_content_lines(base_logo, EmptyStateGuidance::Tour);
        let needs = empty_state_content_lines(base_logo, EmptyStateGuidance::NeedsProvider);
        assert_eq!(
            guidance_line_count(EmptyStateGuidance::Tour),
            CAROUSEL_LINES
        );
        assert_eq!(guidance_line_count(EmptyStateGuidance::NeedsProvider), 2);
        assert_eq!(needs, tour + 1, "blocker adds its action row");
    }
}
