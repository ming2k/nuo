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
//! Beneath the logo the hero carries a **capability carousel**: one
//! durable capability hint at a time (`/fork`, `Ctrl-r`, `@` mentions,
//! …) rotating on a wall-clock cadence, one line at a time (no position
//! indicator — the copy is self-explaining) and nothing else — the static
//! "type a message" tagline is retired, since the carousel's own first page
//! already answers "how do I start". It is the
//! same "teach quietly, without a manual" slot that can later host other
//! contextual tours. A missing provider replaces the carousel with a pinned
//! setup blocker (ADR-0057 → ADR-0104) — nothing rotates until the blocker clears.
//!
//! The carousel also teaches the Help dialog's dual chords (`F1` / `Alt-h`),
//! complementing the TabBar's elastic help portal (ADR-0245) with a full
//! sentence explaining what the dialog contains ("See every shortcut with
//! `F1` or `Alt-h`").
//!
//! The logo source is pluggable: a caller may pass user-supplied lines (loaded
//! from `$XDG_CONFIG_HOME/nuo/logo.txt`); when absent the built-in figlet
//! wordmark is used. Either way the art is clamped to a [`MIN_LOGO_COLS`] ×
//! [`MIN_LOGO_ROWS`] floor and a [`MAX_LOGO_COLS`] × [`MAX_LOGO_ROWS`]
//! ceiling, then only drawn when it fits the viewport without wrapping or
//! clipping ([`plan_hero`]). When the terminal is too short (or the art too
//! large) the wordmark is dropped and the guidance line is shown alone,
//! vertically centred — never a squashed or truncated logo.
use graticule_canvas::{
    Alignment, Frame, Paragraph, Rect, {Line, Span}, {Modifier, Style},
};

use nuo_ui_primitives::Theme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Hard width cap (in terminal columns) for any logo line. A wider line is
/// truncated at a character boundary. 60 keeps comfortable side margins inside
/// an 80-column terminal while leaving room for the guidance line beneath.
pub const MAX_LOGO_COLS: usize = 60;

/// Hard height cap (in rows) for the logo block. More lines than this are
/// dropped from the bottom. 20 leaves the welcome screen readable on a
/// 24-row terminal even before vertical centering.
pub const MAX_LOGO_ROWS: usize = 20;

/// Hard width floor (in terminal columns) for a logo that is allowed to
/// render. A mark narrower than this reads as noise rather than a wordmark,
/// so the hero drops the art entirely and shows the guidance line alone —
/// a small logo is worse for the first impression than no logo at all.
pub const MIN_LOGO_COLS: usize = 8;

/// Hard height floor (in rows) for a logo that is allowed to render. Below
/// this the art cannot carry a wordmark at a glance, so the hero drops it in
/// favour of the carousel line.
pub const MIN_LOGO_ROWS: usize = 3;

/// Blank rows inserted between the logo block and the guidance section (the
/// carousel page or the pinned blocker) when the logo is shown. Counted by
/// [`HeroLayout::total_rows`] so scroll accounting matches what is painted.
pub const HERO_LOGO_GAP_ROWS: usize = 1;

/// How long one carousel page stays on screen before rotating to the next
/// Long enough to read a one-line hint at a glance, short
/// enough that a user who lingers on the empty state sees several pages.
/// The index is derived from wall-clock elapsed time (`carousel_epoch`), so
/// the cadence stays constant regardless of how often the loop redraws.
pub const CAROUSEL_SLIDE_SECS: u64 = 8;

/// One rotating capability page beneath the logo. Each page is one
/// centered line: a muted lead sentence followed by keycap/`command` tokens
/// picked out in the theme's info tone, so the actionable part of each hint
/// reads as an affordance the way the hint bar's `◆ effort` tag does.
///
/// The copy is intentionally **durable**: every page teaches a capability
/// that remains true for the life of the product (send, steer/queue, the help
/// dialog's `F1` / `Alt-h`, forking, history, models, mentions, the command
/// palette, sessions), never a transient state. The list is static — there is
/// nothing session-specific to compute — and new pages can be appended freely:
/// the modulo rotation is derived from the slice length, so no other code needs
/// to know the count.
///
/// Every page is kept short enough to fit the minimum terminal width on one
/// centered line (`carousel_pages_fit_the_minimum_terminal_width` asserts
/// it); a page that wrapped would break the wrap-independent height
/// accounting in [`HeroLayout`].
pub fn carousel_pages() -> Vec<CarouselPage> {
    use CarouselToken as Tok;
    vec![
        CarouselPage {
            lead: "Send a message, or type ",
            tokens: vec![Tok::Key("/"), Tok::Text(" for commands")],
        },
        // Help discovery lives here rather than in a standing TabBar keycap
        // (ADR-0219): the empty state can spend a whole line teaching *why* to
        // press the key, while a permanent keycap in the TabBar spent the row's
        // scarcest budget — tab titles — on a key pressed once. It sits second
        // so a first-time user meets it inside the first minute.
        CarouselPage {
            lead: "See every shortcut with ",
            tokens: vec![
                Tok::Key(nuo_ui_primitives::chord::Key::F1.display()),
                Tok::Text(" or "),
                Tok::Key(nuo_ui_primitives::chord::Key::ALT_H.display()),
            ],
        },
        CarouselPage {
            lead: "Mid-round, ",
            tokens: vec![
                Tok::Key("Enter"),
                Tok::Text(" steers, "),
                Tok::Key("Tab"),
                Tok::Text(" queues"),
            ],
        },
        CarouselPage {
            lead: "Branch thread into a new tab with ",
            tokens: vec![Tok::Key("/fork")],
        },
        CarouselPage {
            lead: "Recall past prompts with ",
            tokens: vec![Tok::Key(nuo_ui_primitives::chord::Key::CTRL_R.display())],
        },
        CarouselPage {
            lead: "Switch models with ",
            tokens: vec![Tok::Key("/models")],
        },
        CarouselPage {
            lead: "Mention files with ",
            tokens: vec![Tok::Key("@"), Tok::Text(" — Tab completes")],
        },
        CarouselPage {
            lead: "Open the command palette with ",
            tokens: vec![Tok::Key(
                nuo_ui_primitives::chord::Key::CTRL_SHIFT_P.display(),
            )],
        },
        CarouselPage {
            lead: "Jump between sessions with ",
            tokens: vec![Tok::Key("/sessions")],
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

/// How the empty-state hero lays itself out for a given viewport. Split out
/// so the renderer and any caller can derive the exact same decision and row
/// count — the app loop's `content_lines` accounting must match what is
/// painted, or the empty session would mis-pin the scroll position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeroLayout {
    /// Whether the wordmark is shown. `false` means the hero carries the
    /// guidance section alone, vertically centred.
    pub show_logo: bool,
    /// Logo rows that will be drawn (0 when [`Self::show_logo`] is false).
    pub logo_rows: usize,
    /// Guidance rows beneath the logo (carousel page or pinned blocker).
    pub guidance_rows: usize,
}

impl HeroLayout {
    /// Total rows the hero occupies, gap included.
    pub fn total_rows(&self) -> usize {
        if self.show_logo {
            self.logo_rows + HERO_LOGO_GAP_ROWS + self.guidance_rows
        } else {
            self.guidance_rows
        }
    }
}

/// Decide the hero layout for `area`.
///
/// The logo is shown only when it respects the [`MIN_LOGO_COLS`] ×
/// [`MIN_LOGO_ROWS`] floor **and** fits the available band without wrapping or
/// clipping: its width must not exceed `area.width` and its height plus the
/// gap plus the guidance must not exceed `area.height`. Otherwise the artwork
/// is dropped and the guidance section is shown alone — a wrapped or clipped
/// logo damages the first impression more than no logo at all.
pub fn plan_hero(logo: &[&str], area: Rect, guidance: &EmptyStateGuidance) -> HeroLayout {
    let guidance_rows = guidance_line_count(guidance);
    let logo_rows = logo.len();
    let logo_cols = logo.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let fits = !logo.is_empty()
        && logo_cols >= MIN_LOGO_COLS
        && logo_rows >= MIN_LOGO_ROWS
        && logo_cols <= area.width as usize
        && logo_rows + HERO_LOGO_GAP_ROWS + guidance_rows <= area.height as usize;
    HeroLayout {
        show_logo: fits,
        logo_rows: if fits { logo_rows } else { 0 },
        guidance_rows,
    }
}

/// Which guidance variant the empty-state hero shows beneath the logo.
///
/// The empty state is a **contextual** surface (ADR-0057 → ADR-0104): it adapts to
/// whether the user has a working LLM configured, rather than showing every
/// user the same static line. The variants are mutually exclusive and
/// ordered by priority — a missing provider beats everything, since nothing
/// else matters until the blocker clears.
///
/// - [`Self::NeedsProvider`] replaces the carousel with a static
///   setup blocker: a message the user must act on before anything else is
///   useful, so it stays pinned instead of rotating.
/// - [`Self::Tour`] (the default) shows the rotating capability carousel
///
/// The app shell selects the variant; the view layer owns the copy and
/// styling. This keeps the policy (when to nudge) in the shell and the
/// presentation (what the nudge looks like) in the renderer.
/// Guidance shown beneath the logo in the empty-state hero.
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
    /// Transient sub-120ms startup/prepare window (ADR-0260 `[INV-SRVPHASE-05]`):
    /// renders only the calm logo with no status subtitle so fast warm connects
    /// and thread switches never flash intermediate labels or stale prompts.
    QuietWait,
    /// Client transport layer is discovering, spawning, connecting, or
    /// reconnecting to the Nuo server (ADR-0260 `[INV-SRVPHASE-05]`).
    Connecting { label: String },
    /// The Nuo server itself is not yet `Ready` (initializing, draining, or
    /// failed) — rendered from the server-attested [`nuo_protocol::ServerPhase`]
    /// (ADR-0260 `[INV-SRVPHASE-01]`, `[INV-SRVPHASE-05]`).
    ServerPhase(nuo_protocol::ServerPhase),
    /// The user explicitly initiated switching to or resuming an existing
    /// thread (holds its short ID) alongside the server-attested phase so real
    /// assembly stages stay visible instead of being masked.
    ResumingThread {
        short_id: String,
        phase: nuo_protocol::ThreadPhase,
    },
    /// The foreground thread is not yet `Ready` — render the exact
    /// server-attested lifecycle phase or assembly failure (ADR-0133, ADR-0260).
    ThreadPhase(nuo_protocol::ThreadPhase),
}

fn prepare_stage_label(stage: nuo_protocol::PrepareStage) -> &'static str {
    match stage {
        nuo_protocol::PrepareStage::Thread => "thread store",
        nuo_protocol::PrepareStage::Provider => "provider",
        nuo_protocol::PrepareStage::Skills => "skills",
        nuo_protocol::PrepareStage::Hooks => "hooks",
    }
}

fn server_init_stage_label(stage: nuo_protocol::ServerInitStage) -> &'static str {
    match stage {
        nuo_protocol::ServerInitStage::Storage => "storage",
        nuo_protocol::ServerInitStage::Runtime => "runtime",
    }
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn format_elapsed_suffix(started_at_ms: Option<u64>, now_ms: u64) -> String {
    let Some(started) = started_at_ms else {
        return String::new();
    };
    let secs = now_ms.saturating_sub(started) / 1000;
    if secs < 2 {
        String::new()
    } else {
        format!(" ({secs}s)")
    }
}

/// Format an optional [`nuo_protocol::StorageInitDetail`] as a concise lowercase
/// phrase for status bars and CLI progress lines (ADR-0266).
pub fn storage_init_detail_label(detail: &nuo_protocol::StorageInitDetail) -> String {
    match detail {
        nuo_protocol::StorageInitDetail::BackingUp { target_version } => {
            format!("backing up database before v{target_version}")
        }
        nuo_protocol::StorageInitDetail::Migrating { step, total, name } => {
            format!("migrating database ({step}/{total} · {name})")
        }
        nuo_protocol::StorageInitDetail::Verifying => "verifying database integrity".to_string(),
    }
}

/// Concise, lowercase status-bar label for a non-`Ready` [`nuo_protocol::ServerPhase`]
/// (ADR-0260 `[INV-SRVPHASE-05]`, ADR-0266 `[INV-SRVPHASE-07]`).
///
/// Returns `None` once the server is [`nuo_protocol::ServerPhase::Ready`].
pub fn server_phase_status(phase: &nuo_protocol::ServerPhase) -> Option<String> {
    server_phase_status_at(phase, unix_now_ms())
}

/// Deterministic variant of [`server_phase_status`] taking an explicit `now_ms`.
pub fn server_phase_status_at(phase: &nuo_protocol::ServerPhase, now_ms: u64) -> Option<String> {
    match phase {
        nuo_protocol::ServerPhase::Initializing {
            stage,
            detail,
            started_at_ms,
        } => {
            let suffix = format_elapsed_suffix(*started_at_ms, now_ms);
            let label = match detail {
                Some(detail) => format!("{}…{suffix}", storage_init_detail_label(detail)),
                None => format!(
                    "initializing server · {}…{suffix}",
                    server_init_stage_label(*stage)
                ),
            };
            Some(label)
        }
        nuo_protocol::ServerPhase::Ready => None,
        nuo_protocol::ServerPhase::Reloading => Some("reloading server…".to_string()),
        nuo_protocol::ServerPhase::Draining { .. } => Some("server shutting down…".to_string()),
        nuo_protocol::ServerPhase::Failed { .. } => Some("server startup failed".to_string()),
    }
}

/// Concise, lowercase status-bar label for a non-`Ready` [`nuo_protocol::ThreadPhase`].
///
/// Returns `None` once the thread is [`nuo_protocol::ThreadPhase::Ready`].
/// Every non-ready state reports the exact local assembly step the server is
/// performing (ADR-0133, ADR-0260) — never a generic placeholder or internal
/// claim token.
pub fn thread_phase_status(phase: &nuo_protocol::ThreadPhase) -> Option<&'static str> {
    match phase {
        nuo_protocol::ThreadPhase::Preparing { stage } => Some(match stage {
            nuo_protocol::PrepareStage::Thread => "preparing · thread store…",
            nuo_protocol::PrepareStage::Provider => "preparing · provider…",
            nuo_protocol::PrepareStage::Skills => "preparing · skills…",
            nuo_protocol::PrepareStage::Hooks => "preparing · hooks…",
        }),
        nuo_protocol::ThreadPhase::Ready => None,
        nuo_protocol::ThreadPhase::Failed { .. } => Some("thread setup failed"),
    }
}

/// Concise, lowercase status-bar label when the user is explicitly resuming or
/// switching to `short_id`, composed with the server-attested [`nuo_protocol::ThreadPhase`]
/// so real preparation stages remain visible.
pub fn resuming_thread_status(short_id: &str, phase: &nuo_protocol::ThreadPhase) -> String {
    match phase {
        nuo_protocol::ThreadPhase::Preparing { stage } => {
            format!(
                "resuming {short_id} · preparing {}…",
                prepare_stage_label(*stage)
            )
        }
        nuo_protocol::ThreadPhase::Failed { .. } => "thread setup failed".to_string(),
        nuo_protocol::ThreadPhase::Ready => format!("resuming thread {short_id}…"),
    }
}

impl EmptyStateGuidance {
    /// Whether this variant rotates its capability pages. The setup
    /// blocker stays pinned — it is an action item, not a tour stop.
    #[cfg(test)]
    pub fn is_carousel(&self) -> bool {
        matches!(self, Self::Tour)
    }
}

/// Number of text lines the guidance section renders beneath the logo + gap.
/// Used by [`plan_hero`] to keep the hero's row count honest without needing a
/// `Theme` — the count is variant-only, never wrap-dependent (every guidance
/// line fits the minimum terminal width; the carousel contributes exactly its
/// current page line).
fn guidance_line_count(guidance: &EmptyStateGuidance) -> usize {
    match guidance {
        // the current carousel page (no static tagline above it — page 0
        // already teaches "send a message or /")
        EmptyStateGuidance::Tour => CAROUSEL_LINES,
        EmptyStateGuidance::NeedsProvider => 2, // blocker + action
        EmptyStateGuidance::QuietWait => 0,
        EmptyStateGuidance::Connecting { .. } => 1,
        EmptyStateGuidance::ServerPhase(nuo_protocol::ServerPhase::Failed { message, .. }) => {
            if message.trim().is_empty() {
                1
            } else {
                2
            }
        }
        EmptyStateGuidance::ServerPhase(_) => 1,
        EmptyStateGuidance::ResumingThread {
            phase: nuo_protocol::ThreadPhase::Failed { message, .. },
            ..
        }
        | EmptyStateGuidance::ThreadPhase(nuo_protocol::ThreadPhase::Failed { message, .. }) => {
            if message.trim().is_empty() {
                1
            } else {
                2
            }
        }
        EmptyStateGuidance::ResumingThread { .. } | EmptyStateGuidance::ThreadPhase(_) => 1,
    }
}

/// Rows the carousel itself renders beneath the logo: the current page
/// line. No tagline above it (page 0 already teaches "send a message or /")
/// and no indicator row — the rotating copy is self-explaining.
const CAROUSEL_LINES: usize = 1;

/// Resolve the carousel page to show for a wall-clock `elapsed` since the
/// carousel epoch. The index is `(elapsed / slide) % pages`, so
/// the cadence is steady regardless of draw frequency (the same property the
/// breathing indicator relies on via `spinner_epoch`), and any terminal
/// size sees the full set over time. Falls back to the first page for an
/// empty set, which `carousel_pages()` never produces.
pub fn carousel_page_for(elapsed_ms: u128) -> usize {
    let pages = carousel_pages().len().max(1);
    ((elapsed_ms / (CAROUSEL_SLIDE_SECS as u128 * 1000)) as usize) % pages
}

/// Build the carousel block for page `index`: the page line only. No dot
/// indicator — the rotating copy is self-explaining (each page carries its
/// own affordance), and an indicator row would spend a second row of chrome
/// restating "this line changes", information the user cannot act on. The
/// hero stays a minimal landing strip.
fn carousel_lines(index: usize, theme: &Theme) -> Vec<Line<'static>> {
    let pages = carousel_pages();
    let index = index.min(pages.len().saturating_sub(1));
    vec![pages[index].line(theme)]
}

/// Truncate `text` to at most `max_cols` terminal display columns, appending
/// `…` when characters are dropped so long server diagnostics never silently
/// clip at the viewport edge.
fn truncate_display_width(text: &str, max_cols: usize) -> String {
    if max_cols == 0 || text.width() <= max_cols {
        return text.to_string();
    }
    if max_cols == 1 {
        return "…".to_string();
    }
    let budget = max_cols - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    let trimmed = out.trim_end();
    format!("{trimmed}…")
}

fn failed_phase_lines(
    message: &str,
    max_width: usize,
    err: Style,
    muted: Style,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![Span::styled("Thread setup failed.", err)])];
    let detail = message.lines().next().unwrap_or("").trim();
    if !detail.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            truncate_display_width(detail, max_width),
            muted,
        )]));
    }
    lines
}

/// Build the styled text lines for the guidance section. The logo + blank gap
/// are rendered by [`draw_empty_state`]; this owns only the copy beneath. The
/// `info` tone highlights actionable tokens (`/connections`, keycaps, …) so the
/// next step reads as an affordance, not ambient text — mirroring how the hint
/// bar uses `info` for the `◆ effort` tag.
///
/// `carousel_index` selects the carousel page for the [`EmptyStateGuidance::Tour`]
/// variant; it is ignored by the pinned variants.
fn guidance_section(
    guidance: EmptyStateGuidance,
    carousel_index: usize,
    max_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let muted = Style::default().fg(theme.muted());
    let info = Style::default().fg(theme.info());
    let warn = Style::default().fg(theme.warn());
    let err = Style::default().fg(theme.err());
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
        EmptyStateGuidance::QuietWait => Vec::new(),
        EmptyStateGuidance::Connecting { ref label } => vec![Line::from(vec![Span::styled(
            truncate_display_width(label, max_width),
            muted,
        )])],
        EmptyStateGuidance::ServerPhase(ref phase) => match phase {
            nuo_protocol::ServerPhase::Initializing {
                stage,
                detail,
                started_at_ms,
            } => {
                let elapsed = format_elapsed_suffix(*started_at_ms, unix_now_ms());
                match detail {
                    Some(detail) => {
                        let text = format!(
                            "Initializing Nuo server · {}…{elapsed}",
                            storage_init_detail_label(detail)
                        );
                        vec![Line::from(vec![Span::styled(
                            truncate_display_width(&text, max_width),
                            info,
                        )])]
                    }
                    None => {
                        let tail = format!("…{elapsed}");
                        vec![Line::from(vec![
                            Span::styled("Initializing Nuo server · ", muted),
                            Span::styled(server_init_stage_label(*stage), info),
                            Span::styled(tail, muted),
                        ])]
                    }
                }
            }
            nuo_protocol::ServerPhase::Reloading => vec![Line::from(vec![Span::styled(
                "Reloading Nuo server…",
                muted,
            )])],
            nuo_protocol::ServerPhase::Draining { reason } => {
                let text = match reason.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
                    Some(r) => format!("Nuo server is shutting down ({r})…"),
                    None => "Nuo server is shutting down…".to_string(),
                };
                vec![Line::from(vec![Span::styled(
                    truncate_display_width(&text, max_width),
                    warn,
                )])]
            }
            nuo_protocol::ServerPhase::Failed { message, .. } => {
                let mut lines = vec![Line::from(vec![Span::styled(
                    "Nuo server startup failed.",
                    err,
                )])];
                let detail = message.lines().next().unwrap_or("").trim();
                if !detail.is_empty() {
                    lines.push(Line::from(vec![Span::styled(
                        truncate_display_width(detail, max_width),
                        muted,
                    )]));
                }
                lines
            }
            nuo_protocol::ServerPhase::Ready => carousel_lines(carousel_index, theme),
        },
        EmptyStateGuidance::ResumingThread {
            ref short_id,
            ref phase,
        } => match phase {
            nuo_protocol::ThreadPhase::Preparing { stage } => vec![Line::from(vec![
                Span::styled("Resuming ", muted),
                Span::styled(short_id.clone(), info),
                Span::styled(" · preparing ", muted),
                Span::styled(prepare_stage_label(*stage), info),
                Span::styled("…", muted),
            ])],
            nuo_protocol::ThreadPhase::Failed { message, .. } => {
                failed_phase_lines(message, max_width, err, muted)
            }
            nuo_protocol::ThreadPhase::Ready => {
                vec![Line::from(vec![
                    Span::styled("Resuming thread ", muted),
                    Span::styled(short_id.clone(), info),
                    Span::styled("…", muted),
                ])]
            }
        },
        EmptyStateGuidance::ThreadPhase(ref phase) => match phase {
            nuo_protocol::ThreadPhase::Preparing { stage } => vec![Line::from(vec![
                Span::styled("Preparing ", muted),
                Span::styled(prepare_stage_label(*stage), info),
                Span::styled("…", muted),
            ])],
            nuo_protocol::ThreadPhase::Failed { message, .. } => {
                failed_phase_lines(message, max_width, err, muted)
            }
            nuo_protocol::ThreadPhase::Ready => carousel_lines(carousel_index, theme),
        },
    }
}

/// Draw the empty-state hero centered in `area`, returning the number of rows
/// it painted (for `content_lines` accounting). Paints nothing outside the
/// given rect.
///
/// `user_logo` — when `Some` and non-empty, replaces the built-in wordmark.
/// The caller is responsible for having loaded + parsed it (clamped here as a
/// safety net regardless).
///
/// `guidance` selects the copy shown beneath the logo+gap (ADR-0057 → ADR-0104): the
/// rotating tour or the pinned provider blocker. It is the app
/// shell's policy decision — the view layer only renders what it is given.
///
/// `carousel_index` picks the tour page (see [`carousel_page_for`]); the
/// caller derives it from wall-clock elapsed time.
///
/// When the viewport cannot show the wordmark without wrapping or clipping,
/// [`plan_hero`] drops it and only the guidance section is drawn, vertically
/// centred.
pub fn draw_empty_state(
    frame: &mut Frame,
    area: Rect,
    user_logo: Option<&[String]>,
    guidance: EmptyStateGuidance,
    carousel_index: usize,
    theme: &Theme,
) -> usize {
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

    let plan = plan_hero(&logo_refs, area, &guidance);
    let max_guidance_width = area.width.saturating_sub(2) as usize;
    let section = guidance_section(guidance, carousel_index, max_guidance_width, theme);

    let mut lines: Vec<Line> = Vec::with_capacity(plan.total_rows());
    if plan.show_logo {
        for row in &logo_refs[..plan.logo_rows] {
            lines.push(Line::from(vec![Span::styled(*row, logo_style)]));
        }
        // Blank gap, then the guidance section (carousel page or blocker).
        lines.push(Line::raw(""));
    }
    lines.extend(section);

    let rendered = lines.len();
    // Center vertically: push the whole block down by half the slack so it sits
    // roughly in the middle of the viewport rather than pinned to the top.
    let slack = area.height.saturating_sub(rendered as u16) / 2;
    let top = area.y + slack;

    let mut para = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(
        &mut para,
        Rect::new(area.x, top, area.width, area.height - slack),
    );
    rendered
}

#[cfg(test)]
mod tests;
