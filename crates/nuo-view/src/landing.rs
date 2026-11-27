//! Project-level landing hero: brand wordmark, capability carousel, and
//! layered readiness / setup guidance.
//!
//! This surface belongs to the **project/workspace stage**, not the `Thread`
//! transcript stream: it renders while connecting, while the server initializes
//! or degrades, while a thread is preparing/resuming, or when the user lands in
//! a fresh workspace with no dialogue yet. It is painted directly by the stage
//! orchestrator (`draw_project_landing`) rather than short-circuiting inside
//! `draw_transcript`, so it never participates in transcript scroll, focus
//! wash, selection, or attribution logic.
//!
//! Beneath the wordmark the hero carries a **capability carousel**: one
//! durable capability hint at a time (`/fork`, history recall, `@` mentions,
//! …) rotating on a wall-clock cadence, one line at a time (no position
//! indicator — the copy is self-explaining). Actionable chord hints are
//! projected from the client's live keymap ([`CarouselKeys`]) rather than
//! hardcoded defaults, so user remappings and unbound commands stay honest.
//! A missing provider replaces the carousel with a pinned setup blocker —
//! nothing rotates until the blocker clears.
//!
//! The wordmark source is pluggable: a caller may pass user-supplied lines
//! (loaded from `$XDG_CONFIG_HOME/nuo/logo.txt`); when absent the built-in
//! figlet wordmark is used. Either way the art is measured in **terminal
//! display columns** (`unicode_width`), clamped to [`MIN_LOGO_COLS`] ×
//! [`MIN_LOGO_ROWS`] ..= [`MAX_LOGO_COLS`] × [`MAX_LOGO_ROWS`], and only drawn
//! when it fits the viewport without wrapping or clipping ([`plan_hero`]).
use std::borrow::Cow;

use graticule_canvas::{
    Alignment, Frame, Paragraph, Rect, {Line, Span}, {Modifier, Style},
};
use nuo_ui_primitives::Theme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Hard width cap (in terminal display columns) for any logo line. A wider
/// line is truncated at a character boundary so it never exceeds this column
/// budget. 60 keeps comfortable side margins inside an 80-column terminal while
/// leaving room for the guidance line beneath.
pub const MAX_LOGO_COLS: usize = 60;

/// Hard height cap (in rows) for the logo block. More lines than this are
/// dropped from the bottom. 20 leaves the welcome screen readable on a
/// 24-row terminal even before vertical centering.
pub const MAX_LOGO_ROWS: usize = 20;

/// Hard width floor (in terminal display columns) for a logo that is allowed
/// to render. A mark narrower than this reads as noise rather than a wordmark,
/// so the hero drops the art entirely and shows the guidance line alone.
pub const MIN_LOGO_COLS: usize = 8;

/// Hard height floor (in rows) for a logo that is allowed to render. Below
/// this the art cannot carry a wordmark at a glance, so the hero drops it in
/// favour of the guidance line.
pub const MIN_LOGO_ROWS: usize = 3;

/// Blank rows inserted between the logo block and the guidance section when
/// both the logo and at least one guidance line are shown. Counted by
/// [`HeroLayout::total_rows`] so layout accounting always matches what is
/// painted.
pub const HERO_LOGO_GAP_ROWS: usize = 1;

/// How long one carousel page stays on screen before rotating to the next.
/// The index is derived from wall-clock elapsed time (`carousel_epoch`), so
/// the cadence stays constant regardless of how often the loop redraws.
pub const CAROUSEL_SLIDE_SECS: u64 = 8;

/// Live keybinding projection consumed by the landing capability carousel.
///
/// The shell builds this from its active `GlobalOverrides` and
/// `SurfaceOverrides` so remapped chords advertise accurately and unbound
/// commands never print dead keys (`[INV-HELP-04]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarouselKeys {
    /// Active chord(s) that open the Help dialog (default: `["F1", "Alt-h"]`).
    /// When empty, the help slide teaches `/help` instead.
    pub help: Vec<String>,
    /// Active chord for cross-session history recall (default: `Some("Ctrl-r")`).
    /// When `None`, the history recall slide is omitted.
    pub history_search: Option<String>,
    /// Active chord for the command palette (default: `Some("Ctrl-Shift-P")`).
    /// When `None`, the palette slide is omitted.
    pub command_palette: Option<String>,
}

impl Default for CarouselKeys {
    fn default() -> Self {
        Self {
            help: vec![
                nuo_ui_primitives::chord::Key::F1.display().to_string(),
                nuo_ui_primitives::chord::Key::ALT_H.display().to_string(),
            ],
            history_search: Some(nuo_ui_primitives::chord::Key::CTRL_R.display().to_string()),
            command_palette: Some(
                nuo_ui_primitives::chord::Key::CTRL_SHIFT_P
                    .display()
                    .to_string(),
            ),
        }
    }
}

/// Build the rotating capability pages using the caller's live keybindings.
///
/// Every page teaches one durable capability in a single centered line. Pages
/// tied to a remappable chord read from `keys` so user keymap overrides are
/// reflected verbatim.
pub fn carousel_pages_with_keys(keys: &CarouselKeys) -> Vec<CarouselPage> {
    use CarouselToken as Tok;
    let mut pages = Vec::with_capacity(9);

    pages.push(CarouselPage {
        lead: "Send a message, or type ",
        tokens: vec![Tok::Key(Cow::Borrowed("/")), Tok::Text(" for commands")],
    });

    // Help discovery sits second so a first-time user meets it inside the
    // first minute. Honors user remaps and falls back to `/help` when unbound.
    let help_tokens = match keys.help.as_slice() {
        [] => vec![Tok::Key(Cow::Borrowed("/help"))],
        [one] => vec![Tok::Key(Cow::Owned(one.clone()))],
        [first, second, ..] => vec![
            Tok::Key(Cow::Owned(first.clone())),
            Tok::Text(" or "),
            Tok::Key(Cow::Owned(second.clone())),
        ],
    };
    pages.push(CarouselPage {
        lead: "See every shortcut with ",
        tokens: help_tokens,
    });

    pages.push(CarouselPage {
        lead: "Mid-round, ",
        tokens: vec![
            Tok::Key(Cow::Borrowed("Enter")),
            Tok::Text(" steers, "),
            Tok::Key(Cow::Borrowed("Tab")),
            Tok::Text(" queues"),
        ],
    });

    pages.push(CarouselPage {
        lead: "Branch thread into a new tab with ",
        tokens: vec![Tok::Key(Cow::Borrowed("/fork"))],
    });

    if let Some(ref chord) = keys.history_search
        && !chord.is_empty()
    {
        pages.push(CarouselPage {
            lead: "Recall past prompts with ",
            tokens: vec![Tok::Key(Cow::Owned(chord.clone()))],
        });
    }

    pages.push(CarouselPage {
        lead: "Switch models with ",
        tokens: vec![Tok::Key(Cow::Borrowed("/models"))],
    });

    pages.push(CarouselPage {
        lead: "Mention files with ",
        tokens: vec![Tok::Key(Cow::Borrowed("@")), Tok::Text(" — Tab completes")],
    });

    if let Some(ref chord) = keys.command_palette
        && !chord.is_empty()
    {
        pages.push(CarouselPage {
            lead: "Open the command palette with ",
            tokens: vec![Tok::Key(Cow::Owned(chord.clone()))],
        });
    }

    pages.push(CarouselPage {
        lead: "Jump between sessions with ",
        tokens: vec![Tok::Key(Cow::Borrowed("/sessions"))],
    });

    pages
}

/// Convenience wrapper returning carousel pages for the default keymap.
pub fn carousel_pages() -> Vec<CarouselPage> {
    carousel_pages_with_keys(&CarouselKeys::default())
}

/// A styled token inside a carousel page: a keycap/command affordance or a
/// plain-text connective.
enum CarouselToken {
    /// Keycap / command / prefix affordance — rendered in the keycap style.
    Key(Cow<'static, str>),
    /// Plain connective text in the muted tone.
    Text(&'static str),
}

/// One page of the landing capability carousel.
pub struct CarouselPage {
    lead: &'static str,
    tokens: Vec<CarouselToken>,
}

impl CarouselPage {
    /// Render the page as one styled line: `lead tokens…` with the keycap
    /// tokens highlighted and connectives muted.
    fn line(&self, theme: &Theme) -> Line<'static> {
        let muted = Style::default().fg(theme.muted());
        let key_style = theme.keycap_style();
        let mut spans = vec![Span::styled(self.lead, muted)];
        for token in &self.tokens {
            match token {
                CarouselToken::Key(key) => spans.push(Span::styled(key.clone(), key_style)),
                CarouselToken::Text(text) => spans.push(Span::styled(*text, muted)),
            }
        }
        Line::from(spans)
    }

    /// Display width of the rendered line, used to keep default pages inside
    /// the minimum terminal width.
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
/// the guidance line beneath, while still reading as a logo at a glance.
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
/// `MAX_LOGO_COLS` × `MAX_LOGO_ROWS` bounding box in terminal display columns:
///
/// - Lines are split on `\n`; trailing `\r` (CRLF files) is stripped.
/// - Tabs are expanded to 4 spaces so column width matches terminal cells.
/// - Each line is truncated to at most `MAX_LOGO_COLS` terminal display
///   columns (`unicode_width`), never splitting wide characters.
/// - Leading/trailing blank lines are dropped.
/// - At most `MAX_LOGO_ROWS` lines are kept (excess dropped from the bottom).
///
/// Returns `None` when the input yields no visible lines, so the caller falls
/// back to the built-in logo rather than rendering nothing.
pub fn parse_logo(raw: &str) -> Option<Vec<String>> {
    let mut lines: Vec<String> = raw
        .split('\n')
        .map(|l| l.trim_end_matches('\r').replace('\t', "    "))
        .map(|l| truncate_to_display_cols(&l, MAX_LOGO_COLS))
        .skip_while(|l| l.trim().is_empty())
        .collect();
    // Trim trailing blank lines.
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines.truncate(MAX_LOGO_ROWS);
    if lines.is_empty() { None } else { Some(lines) }
}

/// Truncate a string to at most `max_cols` terminal display columns without
/// appending an ellipsis (ASCII/Unicode logo art must clip cleanly at the
/// bounding box edge).
fn truncate_to_display_cols(s: &str, max_cols: usize) -> String {
    if s.width() <= max_cols {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > max_cols {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

/// How the landing hero lays itself out for a given viewport. Split out so the
/// renderer and any caller derive the exact same decision and row count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeroLayout {
    /// Whether the wordmark is shown. `false` means the hero carries the
    /// guidance section alone, vertically centred.
    pub show_logo: bool,
    /// Logo rows that will be drawn (0 when [`Self::show_logo`] is false).
    pub logo_rows: usize,
    /// Guidance rows beneath the logo (carousel page, blocker, or phase line).
    pub guidance_rows: usize,
}

impl HeroLayout {
    /// Total rows the hero occupies, including the gap only when both logo and
    /// guidance are present.
    pub fn total_rows(&self) -> usize {
        if self.show_logo {
            if self.guidance_rows > 0 {
                self.logo_rows + HERO_LOGO_GAP_ROWS + self.guidance_rows
            } else {
                self.logo_rows
            }
        } else {
            self.guidance_rows
        }
    }
}

/// Decide the hero layout for `area`.
///
/// The logo is shown only when it respects the [`MIN_LOGO_COLS`] ×
/// [`MIN_LOGO_ROWS`] floor **and** fits the available band without wrapping or
/// clipping: its display column width (`unicode_width`) must not exceed
/// `area.width`, and its height plus any gap plus guidance must not exceed
/// `area.height`. Otherwise the artwork is dropped and the guidance section is
/// shown alone.
pub fn plan_hero(logo: &[&str], area: Rect, guidance: &LandingGuidance) -> HeroLayout {
    let guidance_rows = guidance_line_count(guidance);
    let logo_rows = logo.len();
    let logo_cols = logo.iter().map(|l| l.width()).max().unwrap_or(0);
    let gap_rows = if guidance_rows > 0 {
        HERO_LOGO_GAP_ROWS
    } else {
        0
    };
    let fits = !logo.is_empty()
        && logo_cols >= MIN_LOGO_COLS
        && logo_rows >= MIN_LOGO_ROWS
        && logo_cols <= area.width as usize
        && logo_rows + gap_rows + guidance_rows <= area.height as usize;
    HeroLayout {
        show_logo: fits,
        logo_rows: if fits { logo_rows } else { 0 },
        guidance_rows,
    }
}

/// Which guidance variant the project-level landing hero shows beneath the
/// wordmark.
///
/// Priority (highest wins):
/// 1. Client transport link readiness (`Connecting` / `Reconnecting` / `QuietWait`)
/// 2. Server lifecycle readiness (`ServerPhase::Initializing` / `Draining` / `Failed`)
/// 3. Thread attachment readiness (`ResumingThread` / `ThreadPhase`)
/// 4. Project-level setup blocker (`NeedsProvider`)
/// 5. Project-level rotating capability tour (`Tour`)
///
/// The app shell selects the variant; the view layer owns the copy and styling.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LandingGuidance {
    /// The rotating tour: the capability carousel alone.
    #[default]
    Tour,
    /// No usable LLM provider is configured. Steers the user to `/connections`
    /// before they type. Stays pinned instead of rotating.
    NeedsProvider,
    /// Transient sub-120ms startup/prepare window (ADR-0260 `[INV-SRVPHASE-05]`):
    /// renders only the calm logo with no status subtitle so fast warm connects
    /// and thread switches never flash intermediate labels.
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

impl LandingGuidance {
    /// Whether this variant actively rotates capability carousel pages.
    ///
    /// Used by the event loop to gate the 8-second carousel wake timer so
    /// static blockers and readiness phases never trigger redundant redraws.
    pub fn is_carousel(&self) -> bool {
        matches!(self, Self::Tour)
    }
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

/// Number of text lines the guidance section renders beneath the logo + gap.
/// Every rendered line is clamped to `max_width` display columns by
/// [`truncate_line_to_width`], so this count is guaranteed to match the painted
/// row count even at `MIN_TERMINAL_COLS`.
fn guidance_line_count(guidance: &LandingGuidance) -> usize {
    match guidance {
        LandingGuidance::Tour => CAROUSEL_LINES,
        LandingGuidance::NeedsProvider => 2,
        LandingGuidance::QuietWait => 0,
        LandingGuidance::Connecting { .. } => 1,
        LandingGuidance::ServerPhase(nuo_protocol::ServerPhase::Failed { message, .. }) => {
            if message.trim().is_empty() {
                1
            } else {
                2
            }
        }
        LandingGuidance::ServerPhase(_) => 1,
        LandingGuidance::ResumingThread {
            phase: nuo_protocol::ThreadPhase::Failed { message, .. },
            ..
        }
        | LandingGuidance::ThreadPhase(nuo_protocol::ThreadPhase::Failed { message, .. }) => {
            if message.trim().is_empty() {
                1
            } else {
                2
            }
        }
        LandingGuidance::ResumingThread { .. } | LandingGuidance::ThreadPhase(_) => 1,
    }
}

/// Rows the carousel itself renders beneath the logo: the current page line.
const CAROUSEL_LINES: usize = 1;

/// Resolve the carousel page index for a wall-clock `elapsed_ms` given the
/// number of active pages.
pub fn carousel_page_for_count(elapsed_ms: u128, page_count: usize) -> usize {
    let pages = page_count.max(1);
    ((elapsed_ms / (CAROUSEL_SLIDE_SECS as u128 * 1000)) as usize) % pages
}

/// Resolve the carousel page index for a wall-clock `elapsed_ms` and the
/// caller's live [`CarouselKeys`].
pub fn carousel_page_for(elapsed_ms: u128, keys: &CarouselKeys) -> usize {
    carousel_page_for_count(elapsed_ms, carousel_pages_with_keys(keys).len())
}

/// Build the carousel block for page `index` using `keys`.
fn carousel_lines(index: usize, keys: &CarouselKeys, theme: &Theme) -> Vec<Line<'static>> {
    let pages = carousel_pages_with_keys(keys);
    let index = index.min(pages.len().saturating_sub(1));
    vec![pages[index].line(theme)]
}

/// Truncate a styled [`Line`] to at most `max_cols` terminal display columns,
/// preserving span styles and appending `…` when characters are dropped so
/// guidance lines never wrap or clip silently at the viewport edge.
fn truncate_line_to_width(line: Line<'static>, max_cols: usize) -> Line<'static> {
    if max_cols == 0 {
        return Line::raw("");
    }
    let total_width: usize = line.spans.iter().map(|s| s.content.width()).sum();
    if total_width <= max_cols {
        return line;
    }
    if max_cols == 1 {
        let style = line.spans.first().map(|s| s.style).unwrap_or_default();
        return Line::from(vec![Span::styled("…", style)]);
    }
    let budget = max_cols - 1;
    let mut out_spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 1);
    let mut used = 0usize;
    let mut last_style = Style::default();
    for span in line.spans {
        last_style = span.style;
        let span_width = span.content.width();
        if used + span_width <= budget {
            out_spans.push(span);
            used += span_width;
            continue;
        }
        let mut partial = String::new();
        for ch in span.content.chars() {
            let w = ch.width().unwrap_or(0);
            if used + w > budget {
                break;
            }
            partial.push(ch);
            used += w;
        }
        let trimmed = partial.trim_end();
        if !trimmed.is_empty() {
            out_spans.push(Span::styled(trimmed.to_string(), span.style));
        }
        break;
    }
    // Trim trailing whitespace on the last kept span before appending `…`.
    while let Some(last) = out_spans.last_mut() {
        let trimmed = last.content.trim_end();
        if trimmed.is_empty() {
            out_spans.pop();
        } else {
            if trimmed.len() != last.content.len() {
                last.content = Cow::Owned(trimmed.to_string());
            }
            last_style = last.style;
            break;
        }
    }
    out_spans.push(Span::styled("…", last_style));
    Line::from(out_spans)
}

fn failed_phase_lines(message: &str, err: Style, muted: Style) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![Span::styled("Thread setup failed.", err)])];
    let detail = message.lines().next().unwrap_or("").trim();
    if !detail.is_empty() {
        lines.push(Line::from(vec![Span::styled(detail.to_string(), muted)]));
    }
    lines
}

/// Build the styled, width-clamped text lines for the guidance section.
fn guidance_section(
    guidance: LandingGuidance,
    carousel_index: usize,
    carousel_keys: &CarouselKeys,
    max_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let muted = Style::default().fg(theme.muted());
    let info = Style::default().fg(theme.info());
    let warn = Style::default().fg(theme.warn());
    let err = Style::default().fg(theme.err());
    let raw_lines = match guidance {
        LandingGuidance::Tour => carousel_lines(carousel_index, carousel_keys, theme),
        LandingGuidance::NeedsProvider => vec![
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
        LandingGuidance::QuietWait => Vec::new(),
        LandingGuidance::Connecting { label } => vec![Line::from(vec![Span::styled(label, muted)])],
        LandingGuidance::ServerPhase(phase) => match phase {
            nuo_protocol::ServerPhase::Initializing {
                stage,
                detail,
                started_at_ms,
            } => {
                let elapsed = format_elapsed_suffix(started_at_ms, unix_now_ms());
                match detail {
                    Some(detail) => {
                        let text = format!(
                            "Initializing Nuo server · {}…{elapsed}",
                            storage_init_detail_label(&detail)
                        );
                        vec![Line::from(vec![Span::styled(text, info)])]
                    }
                    None => {
                        let tail = format!("…{elapsed}");
                        vec![Line::from(vec![
                            Span::styled("Initializing Nuo server · ", muted),
                            Span::styled(server_init_stage_label(stage), info),
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
                vec![Line::from(vec![Span::styled(text, warn)])]
            }
            nuo_protocol::ServerPhase::Failed { message, .. } => {
                let mut lines = vec![Line::from(vec![Span::styled(
                    "Nuo server startup failed.",
                    err,
                )])];
                let detail = message.lines().next().unwrap_or("").trim();
                if !detail.is_empty() {
                    lines.push(Line::from(vec![Span::styled(detail.to_string(), muted)]));
                }
                lines
            }
            nuo_protocol::ServerPhase::Ready => {
                carousel_lines(carousel_index, carousel_keys, theme)
            }
        },
        LandingGuidance::ResumingThread { short_id, phase } => match phase {
            nuo_protocol::ThreadPhase::Preparing { stage } => vec![Line::from(vec![
                Span::styled("Resuming ", muted),
                Span::styled(short_id, info),
                Span::styled(" · preparing ", muted),
                Span::styled(prepare_stage_label(stage), info),
                Span::styled("…", muted),
            ])],
            nuo_protocol::ThreadPhase::Failed { message, .. } => {
                failed_phase_lines(&message, err, muted)
            }
            nuo_protocol::ThreadPhase::Ready => vec![Line::from(vec![
                Span::styled("Resuming thread ", muted),
                Span::styled(short_id, info),
                Span::styled("…", muted),
            ])],
        },
        LandingGuidance::ThreadPhase(phase) => match phase {
            nuo_protocol::ThreadPhase::Preparing { stage } => vec![Line::from(vec![
                Span::styled("Preparing ", muted),
                Span::styled(prepare_stage_label(stage), info),
                Span::styled("…", muted),
            ])],
            nuo_protocol::ThreadPhase::Failed { message, .. } => {
                failed_phase_lines(&message, err, muted)
            }
            nuo_protocol::ThreadPhase::Ready => {
                carousel_lines(carousel_index, carousel_keys, theme)
            }
        },
    };
    raw_lines
        .into_iter()
        .map(|line| truncate_line_to_width(line, max_width))
        .collect()
}

/// Draw the project-level landing hero centered in `area`, returning the
/// exact number of rows painted. Paints nothing outside `area`.
///
/// `user_logo` — when `Some` and non-empty, replaces the built-in wordmark
/// (clamped here as a safety net even if a caller bypassed [`parse_logo`]).
///
/// `guidance` selects the copy beneath the logo (or alone on short viewports).
///
/// `carousel_index` and `carousel_keys` drive the rotating capability tour
/// when `guidance` is [`LandingGuidance::Tour`].
pub fn draw_landing_hero(
    frame: &mut Frame,
    area: Rect,
    user_logo: Option<&[String]>,
    guidance: LandingGuidance,
    carousel_index: usize,
    carousel_keys: &CarouselKeys,
    theme: &Theme,
) -> usize {
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
    let section = guidance_section(
        guidance,
        carousel_index,
        carousel_keys,
        max_guidance_width,
        theme,
    );

    let mut lines: Vec<Line> = Vec::with_capacity(plan.total_rows());
    if plan.show_logo {
        for row in &logo_refs[..plan.logo_rows] {
            lines.push(Line::from(vec![Span::styled(*row, logo_style)]));
        }
        if !section.is_empty() {
            lines.push(Line::raw(""));
        }
    }
    lines.extend(section);

    let rendered = lines.len();
    if rendered == 0 {
        return 0;
    }
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
