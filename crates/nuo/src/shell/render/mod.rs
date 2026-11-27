//! Transcript-area renderer: draws the transcript (and footer chrome) into the
//! Graticule grid while recording semantic-to-screen layout
//! information. This is the entry point the app drives each frame
//! ([`draw_transcript`] / [`TranscriptProps`]); it also re-exports the drawing
//! surface (chrome, composer, overlays, theme, …) the shell consumes.
pub use crate::shell::chrome::draw_completion_menu;
pub use crate::shell::composer::{
    ComposerDrawOptions, INPUT_MSG_IDX, draw_composer, draw_composer_highlighted,
    draw_composer_with_options,
};
pub use nuo_view::chrome::{
    ActivityStripProps, ModelBarKeys, ModelBarProps, QueueBarProps, QueueItemProps, TasksBarProps,
    draw_activity_strip, draw_model_bar,
};
// Design tokens are re-exported crate-visibility so the drawing leaves that
// used to reach them via the old `paint` parent's namespace still resolve.
pub(crate) use crate::shell::overlays::draw_dialog_keys;
pub use crate::shell::overlays::{
    ContextUsageProps, CustomEditorProps, DiffSceneProps, LogSceneProps, QueueModalProps,
    draw_armed_toast, draw_command_log_scene, draw_connections_modal, draw_copy_toast,
    draw_custom_provider_editor, draw_dashboard, draw_diff_scene, draw_history_panel,
    draw_input_injection, draw_model_editor, draw_models_modal, draw_notice_toast,
    draw_oauth_pending, draw_permission_sheet, draw_preset_chooser, draw_provider_delete_confirm,
    draw_question_modal, draw_session_preview, draw_sessions_modal, draw_web_value_editor,
    telemetry_attempt_key, telemetry_round_count,
};
#[cfg(test)]
use graticule::{prohibited_line_end, prohibited_line_start};
pub(crate) use nuo_ui_primitives::design::{
    ACTIVITY_STRIP_ROWS, COMPOSER_MAX_HEIGHT_DIVISOR, COMPOSER_MIN_HEIGHT,
    COMPOSER_PROMPT_PREFIX_COLS, COMPOSER_RIGHT_PAD_COLS, COMPOSER_VERTICAL_CHROME_ROWS,
    FOOTER_H_INSET, FOOTER_TOP_GAP_ROWS, MIN_TERMINAL_COLS, MIN_TERMINAL_ROWS, MODEL_BAR_ROWS,
    PAGE_HEADER_ROWS, TRANSCRIPT_H_INSET,
};
pub use nuo_ui_primitives::primitives::recess_backdrop;
use nuo_ui_primitives::primitives::{VIEWPORT_BOTTOM_MARGIN, viewport_rect};
pub use nuo_ui_primitives::theme::Theme;
pub use nuo_view::disclosure::StickyInfo;
use nuo_view::disclosure::draw_sticky_anchor_if_needed;
/// Which peer plane owns the keyboard (ADR-0145). Re-exported so the shell
/// projects its own `Focus` onto the renderer's paint-time enum at the props
/// boundary instead of the presentation crate depending on the shell's
/// interaction state machine.
pub use nuo_view::focus_plane::FocusPlane;
use nuo_view::footer_stack;
pub(crate) use nuo_view::footer_stack::{
    FooterRow, FooterRowId, PlacedFooter, rect_of as footer_rect,
};
/// Parse a raw logo file into clamped display lines for the project landing hero.
/// Re-exported so the startup loader and the renderer share one clamp rule.
pub use nuo_view::landing::parse_logo;
/// Project-level landing guidance, live carousel keys, and lifecycle status
/// projections.
pub use nuo_view::landing::{
    CarouselKeys, LandingGuidance, resuming_thread_status, server_phase_status, thread_phase_status,
};
/// Transcript arrangement strategy (`turn_band`).
pub(crate) use nuo_view::layout;
#[cfg(test)]
use nuo_view::text_layout::WrappedLine;
#[cfg(test)]
use nuo_view::text_layout::{block_selection_range, line_selection};
use nuo_view::view_header;
#[cfg(test)]
pub(crate) use nuo_view::view_header::ViewKind;
pub(crate) use nuo_view::view_header::{
    SceneNavRects, TabBarItem, TabBarNavRects, TabBarProps, TabStatus, ThreadContext, ViewHints,
    draw_scene_bar, draw_tab_bar,
};
pub use nuo_view::views::settings::{
    ConfigCategory, ConfigFocus, SettingsProps, draw_settings_view,
};
// Modules referenced by their bare name within this file.
use crate::shell::composer_grid::TextGrid;
use nuo_view::landing;

use graticule::{
    Alignment, Block as RtBlock, Constraint, Direction, Frame, Layout, Line, Paragraph, Rect, Span,
    Style,
};

#[cfg(test)]
use nuo_tool::{PermissionRequest, UserQuestionRequest};

use nuo_view::model::document::TranscriptMessage;
use nuo_view::model::layout::{InteractiveTarget, LayoutMap};
use nuo_view::model::selection::{CellDragInfo, SelectionState};

/// Inner rect of a transcript-area region after reserving the uniform
/// [`TRANSCRIPT_H_INSET`] left+right `app_bg` gutters. This is the **single
/// point** where the horizontal inset is applied for the content stream (the
/// `band` every downstream component receives). Individual components no
/// longer clip or hand-pad their own gutter; they trust the rect they
/// receive. The view header is *not* inset here — it spans the terminal's
/// full width and re-applies the inset as text padding inside
/// `draw_tab_bar`.
pub(crate) fn transcript_band_rect(area: Rect) -> Rect {
    Rect::new(
        area.x + TRANSCRIPT_H_INSET,
        area.y,
        area.width.saturating_sub(2 * TRANSCRIPT_H_INSET).max(1),
        area.height,
    )
}

/// Draw the "terminal too small" notice centered in `area`. Replaces the whole
/// UI when the terminal is resized below [`MIN_TERMINAL_COLS`] ×
/// [`MIN_TERMINAL_ROWS`]. Renders nothing but the notice so the user knows
/// exactly what to fix instead of seeing a broken/blank screen.
///
/// The message degrades gracefully on very narrow terminals: each line is
/// truncated to the available width so the notice never overflows or wraps
/// into the dimensions it is complaining about.
fn draw_too_small_notice(frame: &mut Frame, area: Rect, theme: &Theme) {
    let title = Span::styled("Terminal too small", Style::default().fg(theme.warn()));
    let detail = Span::styled(
        format!(
            "Resize to at least {} × {} to continue.",
            MIN_TERMINAL_COLS, MIN_TERMINAL_ROWS
        ),
        Style::default().fg(theme.muted()),
    );
    let paused = Span::styled(
        "Input and animation are paused while frozen.",
        Style::default().fg(theme.muted()),
    );

    // Truncate each visible line to the available width so the notice never
    // overflows the very geometry it is complaining about. A degenerate 0-width
    // terminal shows nothing; a 1–2 width terminal shows a sliver.
    let avail = (area.width as usize).max(1);
    let truncate = |span: Span<'_>| -> Span<'_> {
        let total: String = span.content.into_owned();
        if total.chars().count() <= avail {
            return Span::styled(total, span.style);
        }
        let kept: String = total.chars().take(avail).collect();
        Span::styled(kept, span.style)
    };
    let lines: Vec<Line> = vec![
        Line::from(vec![truncate(title)]),
        Line::raw(""),
        Line::from(vec![truncate(detail)]),
        Line::raw(""),
        Line::from(vec![truncate(paused)]),
    ];

    // Vertically center the block in whatever height is available.
    let slack = area.height.saturating_sub(lines.len() as u16) / 2;
    let mut para = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(
        &mut para,
        Rect::new(area.x, area.y + slack, area.width, area.height - slack),
    );
}

/// Paint the frozen "terminal too small" screen: the app background plus the
/// centered notice, and nothing else. The event loop calls this directly while
/// the terminal is below [`MIN_TERMINAL_COLS`] × [`MIN_TERMINAL_ROWS`] instead
/// of running the full chrome renderer, so a frozen frame cannot paint any
/// stale application surface over the notice. `draw_transcript` applies the
/// same guard internally for callers that bypass the loop (tests/showcase).
pub(crate) fn draw_too_small(frame: &mut Frame, theme: &Theme) {
    let full = frame.area();
    frame.render_widget(
        &mut RtBlock::default().style(Style::default().bg(theme.surface())),
        full,
    );
    draw_too_small_notice(frame, full, theme);
}

pub struct TranscriptProps<'a> {
    pub messages: &'a [TranscriptMessage],
    pub scroll: usize,
    pub selection: &'a SelectionState,
    pub cell_selection: Option<&'a CellDragInfo>,
    /// Transient running status shown in a thin bar above the input box.
    /// Empty / "idle" means the status bar is hidden; every other value
    /// (including "responding") keeps the bar up for the full round lifecycle.
    pub activity: &'a str,
    /// The live provider-retry setback rendered beside (never inside) the
    /// status label — the typed state, so the renderer can choose the full
    /// `retry 1 in 4s` or the compact `retry 1` form without re-parsing a
    /// sentence. First casualty under width pressure.
    pub backoff: Option<&'a nuo_view::retry::RetryProgress>,
    /// Whether a tool permission request is awaiting the user's decision. When
    /// true the activity bar is forced visible even if the loop has gone idle,
    /// and its label reads as a permission state so the live status surface
    /// above the input signals the pending decision (the permission sheet
    /// replaces the input + bars below it; this bar stays on as the one piece
    /// of live status that survives). footer chrome.
    pub awaiting_permission: bool,
    /// Animation phase for the breathing dot and status-text shimmer.
    pub spinner_phase: usize,
    /// The current input-box text (masked while the API-key modal is open). The
    /// transcript layout reads this so the input box can grow to fit its wrapped
    /// text — and *only* the text: the box's height is a function of the wrapped
    /// row count, so it cannot depend on where the caret sits.
    pub input: &'a str,
    /// When true, the hint bar and input box are hidden (overlay modal open).
    pub chrome_hidden: bool,
    /// The persistent one-row outbox summary pinned below the transcript gap.
    /// Its `items` slice is the viewed session's queued dispatches; an empty
    /// slice renders a muted empty state so the bar is always present (the
    /// permanent home for queue affordances).
    pub queue_bar: QueueBarProps<'a>,
    /// The background tasks status bar.
    pub tasks_bar: TasksBarProps<'a>,
    /// The chord the activity strip repeats for its armed interrupt, resolved by
    /// the caller from the live keymap. `None` suppresses the hint entirely: the
    /// strip must never advertise a key nobody bound.
    pub interrupt_key: Option<crate::shell::keymap::Key>,
    /// When set, the view is zoomed into a subagent task: `messages` is the
    /// focused task's child stream. The head band's scene row names the scene
    /// `subagent` and carries the task's typed `role` + `label (i/n)` context (ADR-0268).
    pub subagent_bar: Option<SubagentBarInfo>,
    /// The head band's **scene row** (ADR-0024): the scene the user stands in,
    /// its context, and the session's run-mode flags + the `C-x menu` namespace
    /// pair. `None` suppresses the row entirely (non-app contexts).
    pub page_hints: Option<view_header::ViewHints<'a>>,
    /// Client-global TabBar projection (ADR-0039, ADR-0060, ADR-0062).
    /// Tier 1 of the head band, carrying open tabs and active tab selection.
    pub tab_bar: Option<TabBarProps<'a>>,
    /// Thread execution governance context (ADR-0261 `[INV-HEAD-09]`, ADR-0268).
    /// Projected onto Tier 2 `SceneBar` on thread viewports (role, safety
    /// posture, and right-anchored workspace) and used by the transcript stream
    /// to resolve workspace-relative paths.
    pub thread_context: Option<ThreadContext<'a>>,
    /// Wall-clock instant the current round started, or `None` between rounds.
    /// Drives the muted `<elapsed>` segment in the activity bar.
    pub round_started_at: Option<std::time::Instant>,
    /// Message index of the step (tool step or reasoning trace) whose header
    /// currently rests under the mouse pointer (inline or sticky pinned), so
    /// the next draw lights it up to the intermediate hover tone as a click
    /// affordance. `None` whenever the pointer is elsewhere or an overlay
    /// modal is open.
    pub hovered_step: Option<usize>,
    /// Keyboard-focused activatable target. When `Some`, the matching entry is
    /// painted in the focus chrome — a full-width `Theme::entry_focus_bg` band
    /// behind its header row — so keyboard navigation via `Ctrl+P`/`Ctrl+N` has
    /// a clear, unambiguous indicator that speaks in the same channel (and at
    /// the same weight) as the composer's own focused panel. `None` means no
    /// entry is highlighted.
    pub focused_entry: Option<InteractiveTarget>,
    /// Which peer plane owns the keyboard (ADR-0145).
    ///
    /// The transcript's half of the crossing cue: while the transcript plane is
    /// live it paints a focus rail in its left gutter, so `Shift-Tab` — which
    /// lands on *browse*, with no entry highlighted — still visibly moves the
    /// keyboard. Without it, one direction of the crossing had no cue at all and
    /// the other had only the composer dimming.
    pub focus_plane: FocusPlane,
    pub theme: &'a Theme,
    /// Which layout strategy to arrange messages with. Selectable via
    /// `[tui] transcript_layout`; defaults to [`layout::Strategy::TurnBand`].
    pub layout: layout::Strategy,
    /// Per-message laid-out height cache (Stage 2). Lets the transcript pass
    /// skip the expensive text-wrapping of messages that are entirely outside
    /// the viewport, turning per-frame layout from O(transcript) into
    /// O(visible). The caller clears it whenever the transcript mutates, so an
    /// entry is only ever read while its message's content is unchanged.
    /// `None` outside the app loop (tests / showcase), where every lookup is a
    /// miss — correct, just unoptimized.
    ///
    /// Owns the scroll-anchoring state too: on a width change [`HeightCache::prepare`]
    /// promotes the last captured top-of-viewport anchor to the resolve pass,
    /// and the pass writes its captured/resolved outputs back here for the app
    /// loop to consume.
    pub height_cache: Option<&'a mut HeightCache>,
}

/// Inputs for [`draw_project_landing`]: the project/workspace landing hero
/// framed by the shared stage head band and footer stack.
pub struct ProjectLandingProps<'a> {
    pub activity: &'a str,
    pub backoff: Option<&'a nuo_view::retry::RetryProgress>,
    pub awaiting_permission: bool,
    pub spinner_phase: usize,
    pub input: &'a str,
    pub queue_bar: QueueBarProps<'a>,
    pub tasks_bar: TasksBarProps<'a>,
    pub interrupt_key: Option<crate::shell::keymap::Key>,
    pub page_hints: Option<view_header::ViewHints<'a>>,
    pub tab_bar: Option<TabBarProps<'a>>,
    pub thread_context: Option<ThreadContext<'a>>,
    pub round_started_at: Option<std::time::Instant>,
    /// User-supplied logo lines loaded from `$XDG_CONFIG_HOME/nuo/logo.txt`.
    pub logo: Option<&'a [String]>,
    /// Layered readiness / setup / tour guidance projected by the shell.
    pub guidance: LandingGuidance,
    /// Current capability carousel page index (derived from wall-clock elapsed time).
    pub carousel_index: usize,
    /// Live resolved keybindings for the capability carousel.
    pub carousel_keys: &'a CarouselKeys,
    pub theme: &'a Theme,
}

/// Caches each transcript message's fully-laid-out height (in rows), keyed by
pub use nuo_view::layout::{HeightCache, StickyAnchor};

pub use nuo_view::text_layout::{BlockWrapCache, PreparedCode};

/// Page-header context for the TaskInspection scene (shown when zoomed into a task).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentBarInfo {
    /// The subagent's role (`explore` / `plan` / …), when the `Started` event
    /// has identified it. Rendered in Tier 2 `SceneBar` as bare `ROLE` on
    /// chromatic surfaces (`[ROLE]` on framed fallback — ADR-0268 `[INV-HEAD-11]`).
    /// identity and the title; omitted before the role is known.
    pub role: Option<String>,
    /// Title of the focused subagent (its task description).
    pub label: String,
    /// 1-based index of the focused subagent among its siblings.
    pub index: usize,
    /// Total number of sibling subagent tasks.
    pub total: usize,
}

/// Layout information returned by [`draw_transcript`].
/// The text-column budget the transcript layout gives the input box for a
/// full-width frame: the frame width minus the footer stack's horizontal
/// insets (applied by `footer_stack::place` to the composer's rect) and the
/// composer's own prompt prefix + right pad.
///
/// Single source of truth for the composer's frame-relative width budget.
pub(crate) fn composer_layout_text_width(frame_width: usize) -> usize {
    frame_width
        .saturating_sub(
            (2 * FOOTER_H_INSET) as usize + COMPOSER_PROMPT_PREFIX_COLS + COMPOSER_RIGHT_PAD_COLS,
        )
        .max(1)
}

/// Single source of truth for the composer's variable height given the current
/// viewport geometry and input text.
///
/// The height depends on the wrapped **row count only**: the caret projects
/// onto that grid rather than feeding it, so moving the caret can never resize
/// the box and an empty line created by Ctrl-J keeps its row whether or not the
/// caret is resting on it.
pub(crate) fn calculate_composer_height(viewport_size: Rect, input: &str) -> u16 {
    let input_text_width = composer_layout_text_width(viewport_size.width as usize);
    let input_wrapped_lines = TextGrid::new(input, input_text_width).row_count();
    let desired_input_height = input_wrapped_lines as u16 + COMPOSER_VERTICAL_CHROME_ROWS;
    let max_input_height =
        (viewport_size.height / COMPOSER_MAX_HEIGHT_DIVISOR).max(COMPOSER_MIN_HEIGHT);
    desired_input_height.min(max_input_height)
}

/// Layout information returned by [`draw_project_landing`] and [`draw_transcript`].
pub struct StageRender {
    /// The input box area.
    pub input_rect: Rect,
    /// The hint-bar area pinned directly below the input box (zero-sized when
    /// hidden).
    pub hint_rect: Rect,
    /// The placed footer stack for this frame — every visible row's rect in
    /// stack order. Hit-test consumers resolve bar rects through
    /// `footer_stack::rect_of` on this registry rather than one bespoke
    /// `Option<Rect>` field per bar (todo/queue/activity click routing,
    /// history-panel height reservation).
    pub footer: PlacedFooter,
    /// Total height (in lines) of the rendered body (landing hero or message
    /// stream), ignoring the viewport clip.
    pub content_lines: usize,
    /// Height of the body viewport.
    pub view_height: u16,
    /// The expanded step whose body is currently scrolled into view, so the app
    /// can render/click a sticky header pinned under the HUD bar. `None` when no
    /// expanded step body covers the top of the viewport (always `None` on
    /// project landing).
    pub sticky: Option<StickyInfo>,
    /// Bounding rects for each rendered tab in the top tab bar: `(tab_index, rect)`.
    pub tab_rects: Vec<(usize, Rect)>,
    /// The hovered pill's close mark, when one was painted (ADR-0219).
    pub tab_close_rects: Vec<(usize, Rect)>,
    /// The trailing add-tab affordance, when it fit on the row (ADR-0219).
    pub tab_add_rect: Option<Rect>,
    /// Whether a TabBar hover title reveal is mid-pass (ADR-0152).
    pub tab_reveal_active: bool,
    /// Whether any visible TabBar pill painted a live `Running` text shimmer.
    pub tab_shimmer_active: bool,
    /// Full bounding rect for the top tab bar row.
    pub tab_bar_rect: Option<Rect>,
    /// Bounding rect for left overflow indicator (`‹N`), if present.
    pub tab_prev_rect: Option<Rect>,
    /// Bounding rect for right overflow indicator (`N›`), if present.
    pub tab_next_rect: Option<Rect>,
    /// Bounding rect for overflow overview / switcher button (`[▾]`), if present.
    pub tab_overflow_rect: Option<Rect>,
    /// Bounding rect for unread fault notice badge (`[!N]`), if present (ADR-0131).
    pub tab_notices_rect: Option<Rect>,
    /// Bounding rect for the elastic help portal, if painted (ADR-0245 `[INV-TAB-20]`).
    pub tab_help_rect: Option<Rect>,
    /// Full bounding rect for the Tier 2 SceneBar row.
    pub scene_bar_rect: Option<Rect>,
    /// Interactive navigation bounding rects on Tier 2 SceneBar (ADR-0092 [INV-INTERACT-01]).
    pub scene_nav_rects: SceneNavRects,
}

impl StageRender {
    fn empty() -> Self {
        Self {
            input_rect: Rect::default(),
            hint_rect: Rect::default(),
            footer: PlacedFooter::default(),
            content_lines: 0,
            view_height: 0,
            sticky: None,
            tab_rects: Vec::new(),
            tab_close_rects: Vec::new(),
            tab_add_rect: None,
            tab_reveal_active: false,
            tab_shimmer_active: false,
            tab_bar_rect: None,
            tab_prev_rect: None,
            tab_next_rect: None,
            tab_overflow_rect: None,
            tab_notices_rect: None,
            tab_help_rect: None,
            scene_bar_rect: None,
            scene_nav_rects: SceneNavRects::default(),
        }
    }
}

/// The vertical slice occupied by the uniform two-tier head band (ADR-0261)
/// and the transcript viewport left beneath it.
#[derive(Clone, Copy)]
struct HeadBandRects {
    /// Tier 1: client-global tab bar.
    tab: Option<Rect>,
    /// Tier 2: polymorphic scene bar (includes thread governance + workspace on Thread).
    scene: Option<Rect>,
    /// Remaining viewport after the head band.
    viewport: Rect,
}

/// Hit-test rects produced while painting the head band.
struct HeadBandNavRects {
    tab: TabBarNavRects,
    scene: SceneNavRects,
}

/// The footer rows the current frame needs. Visibility is decided before the
/// row heights, so the two stay in lockstep.
struct FooterVisibility {
    queue_row_needed: bool,
    tasks_row_needed: bool,
    activity_strip_needed: bool,
}

/// A measured footer stack: the declared rows, their total height, and the
/// visibility flags the activity row projects into its props.
struct FooterPlan {
    rows: Vec<FooterRow>,
    height: u16,
    visibility: FooterVisibility,
    input_box_height: u16,
    hint_height: u16,
}

/// Height-cache output for one non-empty transcript pass. `skip_rows` and
/// `content_lines` are the stream's starting scroll accounting.
struct TranscriptWindow {
    message_start: usize,
    message_end: usize,
    content_lines: usize,
    skip_rows: usize,
    virtual_total_lines: Option<usize>,
    anchor_lookup: Option<nuo_view::model::layout::ScrollAnchor>,
}

/// Inputs for the height-cached, layout-strategy-driven transcript pass.
struct TranscriptStreamDraw<'a, 'f> {
    frame: &'a mut Frame<'f>,
    layout_map: &'a mut LayoutMap,
    band: Rect,
    messages: &'a [TranscriptMessage],
    strategy: layout::Strategy,
    scroll: usize,
    height_cache: &'a mut HeightCache,
    selection: &'a SelectionState,
    cell_selection: Option<&'a CellDragInfo>,
    hovered_step: Option<usize>,
    focused_entry: Option<InteractiveTarget>,
    workspace_root: Option<&'a std::path::Path>,
    theme: &'a Theme,
}

/// State recovered from a transcript layout pass for the post-processing
/// stages (sticky anchors and the interactive content rect).
struct TranscriptStreamResult {
    content_lines: usize,
    sticky_anchors: Vec<StickyAnchor>,
}

/// The activity-strip projection of the footer's live state. Bundled so the
/// row painter takes one contract instead of a long positional argument list.
struct ActivityStripRow<'a, 'p> {
    plan: &'a FooterPlan,
    activity: &'a str,
    backoff: Option<&'a nuo_view::retry::RetryProgress>,
    awaiting_permission: bool,
    queue_bar: &'a QueueBarProps<'p>,
    tasks_bar: &'a TasksBarProps<'p>,
    interrupt_key: Option<crate::shell::keymap::Key>,
    spinner_phase: usize,
    round_started_at: Option<std::time::Instant>,
}

/// Carve the top rows of the terminal into the uniform two-tier head band
/// (ADR-0261 `[INV-HEAD-09]`):
/// - row 1 is the Client Global TabBar (`draw_tab_bar`)
/// - row 2 is the Polymorphic SceneBar (`draw_scene_bar`)
///
/// The band is a sibling of the transcript, not content inside it, so it
/// replaces the viewport's top margin rather than nesting under it.
fn compute_head_band(
    frame: &Frame<'_>,
    tab_bar: Option<&TabBarProps<'_>>,
    page_hints: Option<&ViewHints<'_>>,
) -> HeadBandRects {
    let tab_rows = u16::from(tab_bar.is_some());
    let scene_rows = u16::from(page_hints.is_some());
    if tab_rows + scene_rows > 0 {
        debug_assert!(
            tab_rows + scene_rows <= PAGE_HEADER_ROWS,
            "the head band is at most {PAGE_HEADER_ROWS} rows"
        );
        let full = frame.area();
        let sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(tab_rows),
                Constraint::Length(scene_rows),
                Constraint::Min(0),
            ])
            .split(full);
        HeadBandRects {
            tab: (tab_rows > 0).then_some(sub[0]),
            scene: (scene_rows > 0).then_some(sub[1]),
            // The remaining area keeps the bottom viewport margin (0) but
            // drops the top one (the head owns those rows now).
            viewport: Rect::new(
                sub[2].x,
                sub[2].y,
                sub[2].width,
                sub[2].height.saturating_sub(VIEWPORT_BOTTOM_MARGIN),
            ),
        }
    } else {
        HeadBandRects {
            tab: None,
            scene: None,
            viewport: viewport_rect(frame),
        }
    }
}

/// Paint the head band's two tiers and return their interactive rects.
fn draw_head_band(
    frame: &mut Frame<'_>,
    rects: &HeadBandRects,
    tab_bar: Option<&TabBarProps<'_>>,
    page_hints: Option<&ViewHints<'_>>,
    thread_context: Option<&ThreadContext<'_>>,
    theme: &Theme,
) -> HeadBandNavRects {
    let tab = if let (Some(bar), Some(rect)) = (tab_bar, rects.tab) {
        draw_tab_bar(frame, rect, bar, theme)
    } else {
        TabBarNavRects::default()
    };
    let scene = if let (Some(hints), Some(rect)) = (page_hints, rects.scene) {
        draw_scene_bar(frame, rect, hints, thread_context, theme)
    } else {
        SceneNavRects::default()
    };
    HeadBandNavRects { tab, scene }
}

/// Decide which footer rows are visible.
///
/// The activity bar (animated spinner + activity text) is shown for every
/// active phase — including streaming ("responding"), the longest phase and
/// the one where the breathing dot's liveness signal matters most — and hidden
/// only when the harness is idle, so the row returns to the transcript.
///
/// A pending permission request is an exception: the permission sheet replaces
/// the input box and the bars beneath it, so the activity bar is the only live
/// status surface left. Force it on so the user always has a visible "awaiting
/// permission" anchor above the sheet — even if the loop has nominally gone
/// idle (e.g. right after an interrupt that rejects permissions but before the
/// stale round's terminal snapshot lands).
fn footer_visibility(
    chrome_hidden: bool,
    in_subagent: bool,
    activity: &str,
    awaiting_permission: bool,
    queue_bar: &QueueBarProps<'_>,
    tasks_bar: &TasksBarProps<'_>,
) -> FooterVisibility {
    let activity_active = !chrome_hidden
        && !in_subagent
        && (awaiting_permission || (!activity.is_empty() && activity != "idle"));
    // Consolidated single-row activity strip (ADR-0062 [INV-FOOTER-02]):
    // strictly bounded to at most 1 row above Composer. Appears if any
    // activity, pending queue items, or active background tasks exist.
    let queue_row_needed = !chrome_hidden && !in_subagent && !queue_bar.items.is_empty();
    let tasks_row_needed =
        !chrome_hidden && !in_subagent && tasks_bar.tasks.iter().any(|t| !t.dismissed);
    FooterVisibility {
        queue_row_needed,
        tasks_row_needed,
        activity_strip_needed: activity_active || queue_row_needed || tasks_row_needed,
    }
}

/// Measure the declared footer stack. The input box grows with its content,
/// up to roughly half the terminal, so the transcript history always stays
/// visible; the hint bar stays a single row pinned flush below it.
fn plan_footer(
    viewport: Rect,
    input: &str,
    chrome_hidden: bool,
    in_subagent: bool,
    visibility: FooterVisibility,
) -> FooterPlan {
    let activity_strip_height: u16 = if visibility.activity_strip_needed {
        ACTIVITY_STRIP_ROWS
    } else {
        0
    };
    let input_box_height = if in_subagent {
        0
    } else {
        calculate_composer_height(viewport, input)
    };
    let hint_height: u16 = if chrome_hidden || in_subagent {
        0
    } else {
        MODEL_BAR_ROWS
    };
    // The footer stack is declared once, in draw order (ADR-0062).
    // Order, top → bottom: top gap, notice dock, consolidated activity strip,
    // input box, hint bar.
    let rows: Vec<FooterRow> = if chrome_hidden || in_subagent {
        Vec::new()
    } else {
        vec![
            FooterRow {
                id: FooterRowId::TopGap,
                height: FOOTER_TOP_GAP_ROWS,
            },
            FooterRow {
                id: FooterRowId::ActivityStrip,
                height: activity_strip_height,
            },
            FooterRow {
                id: FooterRowId::Composer,
                height: input_box_height,
            },
            FooterRow {
                id: FooterRowId::ModelBar,
                height: hint_height,
            },
        ]
    };
    let height = footer_stack::measure(&rows);
    FooterPlan {
        rows,
        height,
        visibility,
        input_box_height,
        hint_height,
    }
}

/// Stage 2 height-cache bookkeeping. Heights are wrap-width-dependent, so
/// drop the cache on a resize; within a stable width + unchanged transcript
/// every entry reproduces a fresh layout exactly, so off-screen messages can
/// be advanced from their cached height instead of being re-wrapped.
///
/// A `prepare` that saw a width change armed a resolve pass: the anchor
/// captured at the previous width must be mapped back onto this layout so the
/// reading position survives the resize (scroll anchoring). A resolve pass
/// walks the **full** transcript (virtualization is skipped): a large reflow
/// can move the anchored content far from the stale raw offset, well outside
/// the virtual window that offset would select, and a windowed walk would then
/// never measure it.
fn begin_transcript_window(
    height_cache: &mut HeightCache,
    messages: &[TranscriptMessage],
    strategy: layout::Strategy,
    band: Rect,
    scroll: usize,
) -> TranscriptWindow {
    height_cache.prepare(band.width);
    let anchor_lookup = height_cache.pending_anchor();
    let mut window = TranscriptWindow {
        message_start: 0,
        message_end: messages.len(),
        content_lines: 0,
        skip_rows: scroll,
        virtual_total_lines: None,
        anchor_lookup,
    };
    if anchor_lookup.is_none()
        && let Some(virtual_window) =
            height_cache.virtual_window(messages, strategy, scroll, band.height)
    {
        window.message_start = virtual_window.message_start;
        window.message_end = virtual_window.message_end;
        window.content_lines = virtual_window.prefix_lines;
        window.skip_rows = virtual_window.skip_rows;
        if virtual_window.is_full {
            window.virtual_total_lines = Some(virtual_window.total_lines);
        }
    }
    window
}

/// Run the selected layout strategy over the cached transcript window and
/// publish its scroll-anchor outputs back onto the cache the app loop owns.
fn draw_transcript_stream(draw: TranscriptStreamDraw<'_, '_>) -> TranscriptStreamResult {
    let TranscriptStreamDraw {
        frame,
        layout_map,
        band,
        messages,
        strategy,
        scroll,
        height_cache,
        selection,
        cell_selection,
        hovered_step,
        focused_entry,
        workspace_root,
        theme,
    } = draw;
    let window = begin_transcript_window(height_cache, messages, strategy, band, scroll);
    // The `Stream` carries every piece of shared render state (scroll/Y
    // accounting, layout map, height cache, theme, hover/focus) and exposes
    // `badge` / `dispatch` / `gap` as the sanctioned mutations, so every
    // layout agrees on scroll semantics.
    let mut stream = layout::Stream {
        frame,
        band,
        messages,
        theme,
        layout_map,
        // Reborrow so `height_cache` is still usable after the Stream is
        // dropped, to publish the anchor outputs below.
        height_cache: &mut *height_cache,
        selection,
        cell_selection,
        hovered_step,
        focused_entry,
        workspace_root,
        message_start: window.message_start,
        message_end: window.message_end,
        virtual_total_lines: window.virtual_total_lines,
        viewport_top: scroll,
        anchor_lookup: window.anchor_lookup,
        anchor_capture: None,
        anchor_resolved_line: None,
        current_y: band.y,
        skip_rows: window.skip_rows,
        content_lines: window.content_lines,
        sticky_anchors: Vec::new(),
    };
    strategy.build().run(&mut stream);
    // Recover the loop state the post-processing below needs.
    // (`skip_rows` and `current_y` are loop-internal and are not read after
    // the layout returns, so they are not recovered.)
    let result = TranscriptStreamResult {
        content_lines: stream.content_lines,
        sticky_anchors: stream.sticky_anchors,
    };
    // Publish the anchor outputs: the captured top-of-viewport identity is
    // stored for the next resize; a resolved lookup is handed to the app loop,
    // which re-anchors `App::scroll` before the frame commits.
    let anchor_capture = stream.anchor_capture;
    let anchor_resolved = stream.anchor_resolved_line;
    height_cache.set_anchor(anchor_capture);
    height_cache.set_resolved(anchor_resolved);
    result
}

/// Place and paint the single-row activity strip. Its geometry still comes
/// from the footer stack; the row only supplies the live projection.
fn draw_activity_row(
    frame: &mut Frame<'_>,
    placed_footer: &PlacedFooter,
    row: ActivityStripRow<'_, '_>,
    theme: &Theme,
) {
    let ActivityStripRow {
        plan,
        activity,
        backoff,
        awaiting_permission,
        queue_bar,
        tasks_bar,
        interrupt_key,
        spinner_phase,
        round_started_at,
    } = row;
    footer_stack::rect_of(placed_footer, FooterRowId::ActivityStrip)
        .filter(|_| plan.visibility.activity_strip_needed)
        .and_then(|rect| {
            draw_activity_strip(
                frame,
                rect,
                round_started_at,
                ActivityStripProps {
                    status: activity,
                    backoff,
                    awaiting_permission,
                    queued_count: if plan.visibility.queue_row_needed {
                        queue_bar.items.len()
                    } else {
                        0
                    },
                    queued_preview: queue_bar.items.first().map(|i| i.text.as_str()),
                    queued_first_is_steer: queue_bar
                        .items
                        .first()
                        .map(|i| i.is_steer)
                        .unwrap_or(false),
                    queue_blocked: queue_bar.blocked,
                    tasks_count: if plan.visibility.tasks_row_needed {
                        tasks_bar.tasks.iter().filter(|t| !t.dismissed).count()
                    } else {
                        0
                    },
                    tasks_preview: tasks_bar
                        .tasks
                        .iter()
                        .find(|t| !t.dismissed)
                        .map(|t| t.label.as_str()),
                    queue_key: queue_bar.expand_key,
                },
                interrupt_key,
                spinner_phase,
                theme,
            )
        });
}

/// Resolve the composer and model-bar rects from the placed footer stack.
fn footer_content_rects(placed_footer: &PlacedFooter, plan: &FooterPlan) -> (Rect, Rect) {
    let input_rect = footer_stack::rect_of(placed_footer, FooterRowId::Composer)
        .filter(|_| plan.input_box_height > 0)
        .unwrap_or_default();
    let hint_rect = footer_stack::rect_of(placed_footer, FooterRowId::ModelBar)
        .filter(|_| plan.hint_height > 0)
        .unwrap_or_default();
    (input_rect, hint_rect)
}

/// Paint the transcript plane's own focus cue (ADR-0145, amended), in two
/// scales, last so nothing can punch a hole in it:
///
/// 1. The **pane wash** — the whole viewport lifted one quiet step. It is a
///    restyle pass rather than a paint pass because the transcript is composed
///    of dozens of renderers that each pad their own tails with the page tone;
///    a wash drawn first would be covered by the very content it sits under.
/// 2. The **entry marker** — one cell in the highlighted entry's gutter, read
///    back from the layout map (the sentinel row each entry kind already
///    records) rather than from any renderer's cursor, so all five entry kinds
///    answer identically and an off-screen entry simply has no marker.
///
/// Both are zero-layout: no row added, no column moved. The marker is painted
/// from the **base** theme: it is a fixed absolute mark, not a surface, so it
/// must not be lifted (and on a lifted theme its own derivation would report
/// the pane as wash-less).
fn draw_pane_focus_cues(
    frame: &mut Frame<'_>,
    layout_map: &LayoutMap,
    transcript_area: Rect,
    band: Rect,
    focus_plane: FocusPlane,
    focused_entry: Option<InteractiveTarget>,
    base_theme: &Theme,
) {
    nuo_view::focus_plane::draw_pane_focus(frame, transcript_area, band, focus_plane, base_theme);
    if let Some(header) = focused_entry.and_then(|target| layout_map.entry_header_rect(target)) {
        nuo_view::focus_plane::draw_entry_marker(
            frame,
            transcript_area,
            band,
            header.y,
            base_theme,
        );
    }
}

struct StageFrameGeometry {
    frame_surface: graticule::Color,
    head_band: HeadBandRects,
    head_nav: HeadBandNavRects,
    body_area: Rect,
    footer_area: Rect,
    footer_plan: FooterPlan,
}

#[allow(clippy::too_many_arguments)]
fn begin_stage_frame(
    frame: &mut Frame,
    input: &str,
    chrome_hidden: bool,
    in_subagent: bool,
    activity: &str,
    awaiting_permission: bool,
    queue_bar: &QueueBarProps<'_>,
    tasks_bar: &TasksBarProps<'_>,
    tab_bar: Option<&TabBarProps<'_>>,
    page_hints: Option<&view_header::ViewHints<'_>>,
    thread_context: Option<&ThreadContext<'_>>,
    theme: &Theme,
) -> Option<StageFrameGeometry> {
    let full = frame.area();
    let frame_surface = theme.surface();
    frame.render_widget(
        &mut RtBlock::default().style(Style::default().bg(frame_surface)),
        full,
    );

    if full.width < MIN_TERMINAL_COLS || full.height < MIN_TERMINAL_ROWS {
        draw_too_small_notice(frame, full, theme);
        return None;
    }

    let head_band = compute_head_band(frame, tab_bar, page_hints);
    let size = head_band.viewport;
    let visibility = footer_visibility(
        chrome_hidden,
        in_subagent,
        activity,
        awaiting_permission,
        queue_bar,
        tasks_bar,
    );
    let footer_plan = plan_footer(size, input, chrome_hidden, in_subagent, visibility);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(footer_plan.height)])
        .split(size);

    let head_nav = draw_head_band(
        frame,
        &head_band,
        tab_bar,
        page_hints,
        thread_context,
        theme,
    );

    Some(StageFrameGeometry {
        frame_surface,
        head_band,
        head_nav,
        body_area: chunks[0],
        footer_area: chunks[1],
        footer_plan,
    })
}

#[allow(clippy::too_many_arguments)]
fn finish_stage_frame(
    frame: &mut Frame,
    stage: StageFrameGeometry,
    activity: &str,
    backoff: Option<&nuo_view::retry::RetryProgress>,
    awaiting_permission: bool,
    queue_bar: &QueueBarProps<'_>,
    tasks_bar: &TasksBarProps<'_>,
    interrupt_key: Option<crate::shell::keymap::Key>,
    spinner_phase: usize,
    round_started_at: Option<std::time::Instant>,
    content_lines: usize,
    sticky: Option<StickyInfo>,
    theme: &Theme,
) -> StageRender {
    let placed_footer = footer_stack::place(stage.footer_area, &stage.footer_plan.rows);
    draw_activity_row(
        frame,
        &placed_footer,
        ActivityStripRow {
            plan: &stage.footer_plan,
            activity,
            backoff,
            awaiting_permission,
            queue_bar,
            tasks_bar,
            interrupt_key,
            spinner_phase,
            round_started_at,
        },
        theme,
    );
    let (input_rect, hint_rect) = footer_content_rects(&placed_footer, &stage.footer_plan);
    let HeadBandNavRects { tab, scene } = stage.head_nav;
    StageRender {
        input_rect,
        hint_rect,
        footer: placed_footer,
        content_lines,
        view_height: stage.body_area.height,
        sticky,
        tab_rects: tab.tab_rects,
        tab_close_rects: tab.close_rects,
        tab_add_rect: tab.add_rect,
        tab_reveal_active: tab.reveal_active,
        tab_shimmer_active: tab.shimmer_active,
        tab_bar_rect: stage.head_band.tab,
        tab_prev_rect: tab.prev_rect,
        tab_next_rect: tab.next_rect,
        tab_overflow_rect: tab.overview_rect,
        tab_notices_rect: tab.notices_rect,
        tab_help_rect: tab.help_rect,
        scene_bar_rect: stage.head_band.scene,
        scene_nav_rects: scene,
    }
}

/// Draw the **project-level landing surface** (wordmark, capability carousel,
/// and layered readiness / setup guidance) inside the shared stage chrome.
///
/// Elevated out of `draw_transcript` so the project banner and carousel never
/// participate in transcript scroll, focus wash, selection, or attribution.
pub fn draw_project_landing(frame: &mut Frame, props: ProjectLandingProps<'_>) -> StageRender {
    let ProjectLandingProps {
        activity,
        backoff,
        awaiting_permission,
        spinner_phase,
        input,
        queue_bar,
        tasks_bar,
        interrupt_key,
        page_hints,
        tab_bar,
        thread_context,
        round_started_at,
        logo,
        guidance,
        carousel_index,
        carousel_keys,
        theme,
    } = props;

    let Some(stage) = begin_stage_frame(
        frame,
        input,
        false,
        false,
        activity,
        awaiting_permission,
        &queue_bar,
        &tasks_bar,
        tab_bar.as_ref(),
        page_hints.as_ref(),
        thread_context.as_ref(),
        theme,
    ) else {
        return StageRender::empty();
    };

    // Always paint with the base `theme` (never the lifted transcript focus
    // wash): project landing is not an interactive transcript plane.
    let content_lines = landing::draw_landing_hero(
        frame,
        stage.body_area,
        logo,
        guidance,
        carousel_index,
        carousel_keys,
        theme,
    );

    finish_stage_frame(
        frame,
        stage,
        activity,
        backoff,
        awaiting_permission,
        &queue_bar,
        &tasks_bar,
        interrupt_key,
        spinner_phase,
        round_started_at,
        content_lines,
        None,
        theme,
    )
}

/// Draw the main transcript stream area, recording layout info.
pub fn draw_transcript(
    frame: &mut Frame,
    layout_map: &mut LayoutMap,
    props: TranscriptProps<'_>,
) -> StageRender {
    let TranscriptProps {
        messages,
        scroll,
        selection,
        cell_selection,
        activity,
        backoff,
        awaiting_permission,
        spinner_phase,
        input,
        chrome_hidden,
        queue_bar,
        tasks_bar,
        interrupt_key,
        subagent_bar,
        page_hints,
        tab_bar,
        thread_context,
        round_started_at,
        hovered_step,
        focused_entry,
        focus_plane,
        theme,
        layout,
        height_cache,
    } = props;
    let mut fallback_height_cache = HeightCache::default();
    let height_cache: &mut HeightCache = height_cache.unwrap_or(&mut fallback_height_cache);
    let in_subagent = subagent_bar.is_some();

    let Some(stage) = begin_stage_frame(
        frame,
        input,
        chrome_hidden,
        in_subagent,
        activity,
        awaiting_permission,
        &queue_bar,
        &tasks_bar,
        tab_bar.as_ref(),
        page_hints.as_ref(),
        thread_context.as_ref(),
        theme,
    ) else {
        return StageRender::empty();
    };

    let transcript_area = stage.body_area;
    let frame_surface = stage.frame_surface;
    // One value, one truth (ADR-0145 `[INV-FOCUS-03]`): an entry can only be
    // highlighted while the transcript plane owns the keyboard. The two props
    // arrive separately (the shell projects them from one `Focus`), so this is
    // where they are welded back together — a `focused_entry` on the composer
    // plane would be a state the model cannot otherwise express, and painting a
    // focus band for it is exactly the kind of "an entry is lit while the
    // composer is live" artifact the ADR removes.
    let focused_entry = if focus_plane.is_transcript() {
        focused_entry
    } else {
        None
    };
    // Apply the uniform horizontal inset (`TRANSCRIPT_H_INSET` on each side)
    // exactly once, here at the transcript-stream entry point. Every
    // downstream component receives `band` — an already-inset rect — so none
    // of them re-clips or hand-pads a leading gutter. The empty-state hero is
    // the sole exception: it centers across the full viewport, so it keeps
    // `transcript_area` (un-inset). The page header is rendered from its own
    // layout-split rect before this point, so it is unaffected.
    let band = transcript_band_rect(transcript_area);
    // The transcript paints with the **pane-lifted theme** while it owns the
    // keyboard (ADR-0145, amended). Doing it here, at the source, is what makes the
    // cue cost one struct copy instead of a per-cell restyle over the viewport: every
    // renderer below already paints its backgrounds through this struct, so they
    // paint the lifted pane without knowing the cue exists. The wash only moves
    // backgrounds; foregrounds stay semantic.
    //
    // `Cow` rather than a clone: on the composer plane (the common case) this borrows
    // and costs nothing.
    //
    // The *base* theme is kept beside the lifted one on purpose: "which cue does this
    // pane get" is a question about the palette the user loaded, and asking the lifted
    // copy would ask a different question — on that copy `app_bg` already *is* the wash,
    // so the derivation would report no headroom and the pane would fall back to the
    // gutter cue on top of an already-lifted page. Two values, two distinct meanings.
    let base_theme: &Theme = theme;
    let transcript_theme = nuo_view::focus_plane::transcript_theme(theme, focus_plane);
    let theme: &Theme = &transcript_theme;
    // The frame-wide background above was painted with the *unlifted* page, so the
    // pane's own rect has to be repainted at the lifted tone before anything draws
    // into it. One rect fill — not a per-cell restyle — and it is what makes the wash
    // cover the whole pane including the blank rows no renderer touches.
    if theme.surface() != frame_surface {
        frame.render_widget(
            &mut RtBlock::default().style(Style::default().bg(theme.surface())),
            transcript_area,
        );
    }
    let workspace_root = thread_context
        .as_ref()
        .map(|c| c.workspace)
        .filter(|w| !w.is_empty())
        .map(std::path::Path::new);
    let outcome = draw_transcript_stream(TranscriptStreamDraw {
        frame,
        layout_map,
        band,
        messages,
        strategy: layout,
        scroll,
        height_cache,
        selection,
        cell_selection,
        hovered_step,
        focused_entry,
        workspace_root,
        theme,
    });
    let content_lines = outcome.content_lines;
    let sticky_anchors = outcome.sticky_anchors;

    layout_map.set_transcript_content_rect(Rect::new(
        transcript_area.x,
        transcript_area.y,
        transcript_area.width,
        transcript_area.height,
    ));
    draw_transcript_scrollbar(
        frame,
        transcript_area,
        content_lines,
        scroll,
        focus_plane,
        theme,
    );

    let sticky_info = draw_sticky_anchor_if_needed(frame, band, &sticky_anchors, scroll, theme);
    draw_pane_focus_cues(
        frame,
        layout_map,
        transcript_area,
        band,
        focus_plane,
        focused_entry,
        base_theme,
    );

    finish_stage_frame(
        frame,
        stage,
        activity,
        backoff,
        awaiting_permission,
        &queue_bar,
        &tasks_bar,
        interrupt_key,
        spinner_phase,
        round_started_at,
        content_lines,
        sticky_info,
        base_theme,
    )
}

fn draw_transcript_scrollbar(
    frame: &mut Frame,
    area: Rect,
    content_lines: usize,
    scroll: usize,
    focus_plane: FocusPlane,
    theme: &Theme,
) {
    let track_h = area.height as usize;
    if content_lines <= track_h || area.width < 3 || track_h < 2 {
        return;
    }
    let max_scroll = content_lines.saturating_sub(track_h);
    let effective_scroll = scroll.min(max_scroll);
    let thumb_h = ((track_h * track_h) / content_lines).clamp(1, track_h);
    let thumb_top = effective_scroll
        .saturating_mul(track_h - thumb_h)
        .checked_div(max_scroll)
        .unwrap_or(0);
    let x = area.x + area.width - 1;
    let thumb_bg = if focus_plane.is_transcript() {
        theme.pane_marker_fg
    } else {
        theme.muted()
    };
    for row in thumb_top..(thumb_top + thumb_h) {
        let y = area.y + row as u16;
        let cell_rect = Rect::new(x, y, 1, 1);
        frame.render_widget(
            &mut Paragraph::new(Line::from(Span::styled(" ", Style::default().bg(thumb_bg)))),
            cell_rect,
        );
    }
}

#[cfg(test)]
mod tests;
