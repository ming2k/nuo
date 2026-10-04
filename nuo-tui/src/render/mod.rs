//! Transcript-area renderer: draws the transcript (and footer chrome) into the
//! mutx-engine grid while recording semantic-to-screen layout
//! information. This is the entry point the app drives each frame
//! ([`draw_transcript`] / [`TranscriptProps`]); it also re-exports the drawing
//! surface (chrome, composer, overlays, theme, …) the shell consumes.

pub use crate::chrome::draw_persistence_health_bar;
pub use crate::chrome::{ActivityBarProps, draw_activity_bar};
pub use crate::chrome::{
    ModelBarProps, QueueBarProps, QueueItemProps, TasksBarProps, draw_completion_menu,
    draw_model_bar, draw_queue_bar, draw_tasks_bar,
};
pub use crate::composer::{
    ComposerDrawOptions, INPUT_MSG_IDX, draw_composer, draw_composer_highlighted,
    draw_composer_with_options,
};
// Design tokens are re-exported crate-visibility so the drawing leaves that
// used to reach them via the old `paint` parent's namespace still resolve.
pub(crate) use crate::design::{
    ACTIVITY_BAR_ROWS, BASH_FOLD_HEAD_ROWS, BASH_FOLD_TAIL_ROWS, CODE_BAND_GUTTER_GAP,
    CODE_BAND_GUTTER_MIN_WIDTH, COMPOSER_MAX_HEIGHT_DIVISOR, COMPOSER_MIN_HEIGHT,
    COMPOSER_PROMPT_PREFIX_COLS, COMPOSER_RIGHT_PAD_COLS, COMPOSER_VERTICAL_CHROME_ROWS,
    FOOTER_H_INSET, FOOTER_TOP_GAP_ROWS, MIN_TERMINAL_COLS, MIN_TERMINAL_ROWS, MODEL_BAR_ROWS,
    PAGE_HEADER_ROWS, QUEUE_BAR_ROWS, REASONING_TRACE_BLOCK_GAP_ROWS,
    REASONING_TRACE_BODY_TOP_GAP_ROWS, STEP_MIN_WIDTH, TOOL_STEP_BODY_INDENT_COLS,
    TOOL_STEP_BODY_TOP_GAP_ROWS, TOOL_STEP_CHILDREN_GAP_ROWS, TRANSCRIPT_BODY_LEADING_INDENT,
    TRANSCRIPT_H_INSET,
};
use crate::disclosure::{StickyStep, draw_sticky_summary_if_needed};
/// Which guidance copy the empty-state hero shows beneath the logo (ADR-0057).
/// Re-exported so the app shell selects the variant and the renderer paints it.
pub use crate::empty_state::EmptyStateGuidance;
/// Parse a raw logo file into clamped display lines for the empty-state hero.
/// Re-exported so the startup loader and the renderer share one clamp rule.
pub use crate::empty_state::parse_logo;
use crate::footer_stack;
pub(crate) use crate::footer_stack::{
    FooterRow, FooterRowId, PlacedFooter, rect_of as footer_rect,
};
/// Transcript arrangement strategy (`turn_band`).
pub(crate) use crate::layout;
#[allow(unused_imports)]
pub use crate::overlays::*;
pub use crate::primitives::recess_backdrop;
use crate::primitives::{VIEWPORT_BOTTOM_MARGIN, viewport_rect};
#[cfg(test)]
use crate::text_layout::WrappedLine;
#[cfg(test)]
use crate::text_layout::{block_selection_range, line_selection};
#[allow(unused_imports)]
pub use crate::theme::{COLOR_SCHEMES, Theme};
use crate::view_header;
#[allow(unused_imports)]
pub(crate) use crate::view_header::{
    AsidesChip, BtwHead, DashboardHead, SessionHead, ViewHeader, ViewHints, ViewKind,
    draw_view_header, draw_view_header_hints,
};
#[allow(unused_imports)]
pub use crate::views::settings::*;
#[cfg(test)]
use nuotc::text::{prohibited_line_end, prohibited_line_start};
// Re-export the drawing sub-trees so consumers that used to reach them through
// the old `paint` parent can drill in via this module (overlays/tools/…).
pub(crate) use crate::tools;
// Modules referenced by their bare name within this file (formerly declared
// here as submodules; now siblings at the crate root).
use crate::{composer, empty_state};
// Re-exported so layout-strategy code reaches them via the crate-root facade.
pub(crate) use crate::message_body::draw_message_body;
pub(crate) use crate::notice::draw_notice;

use nuotc::{
    Alignment, Block as RtBlock, Constraint, Direction, Frame, Layout, Line, Paragraph, Rect, Span,
    Style,
};

use crate::model::document::TranscriptMessage;
use crate::model::layout::{InteractiveTarget, LayoutMap};
use crate::model::selection::{CellDragInfo, SelectionState};
#[cfg(test)]
use nuo_wire::{PermissionRequest, UserQuestionRequest};

/// Inner rect of a transcript-area region after reserving the uniform
/// [`TRANSCRIPT_H_INSET`] left+right `app_bg` gutters. This is the **single
/// point** where the horizontal inset is applied for the content stream (the
/// `band` every downstream component receives). Individual components no
/// longer clip or hand-pad their own gutter; they trust the rect they
/// receive. The view header is *not* inset here — it spans the terminal's
/// full width and re-applies the inset as text padding inside
/// `draw_view_header`.
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
            "Please resize to at least {} × {}.",
            MIN_TERMINAL_COLS, MIN_TERMINAL_ROWS
        ),
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
    ];

    // Vertically center the block in whatever height is available.
    let slack = area.height.saturating_sub(lines.len() as u16) / 2;
    let para = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(
        para,
        Rect::new(area.x, area.y + slack, area.width, area.height - slack),
    );
}

pub struct TranscriptProps<'a> {
    pub messages: &'a [TranscriptMessage],
    pub scroll: u16,
    pub selection: &'a SelectionState,
    pub cell_selection: Option<&'a CellDragInfo>,
    /// Transient running status shown in a thin bar above the input box.
    /// Empty / "idle" means the status bar is hidden; every other value
    /// (including "responding") keeps the bar up for the full round lifecycle.
    pub activity: &'a str,
    /// Transport-setback clause rendered beside (never inside) the status
    /// label — e.g. `retry 2/8 (next in 4s)` while a provider retry backs
    /// off. Muted styling; first casualty under width pressure.
    pub backoff_clause: Option<&'a str>,
    /// Whether a tool permission request is awaiting the user's decision. When
    /// true the activity bar is forced visible even if the loop has gone idle,
    /// and its label reads as a permission state so the live status surface
    /// above the input signals the pending decision (the permission sheet
    /// replaces the input + bars below it; this bar stays on as the one piece
    /// of live status that survives). See ADR-0089 footer chrome.
    pub awaiting_permission: bool,
    /// Animation phase for the breathing dot and status-text shimmer.
    pub spinner_phase: usize,
    /// The current input-box text (masked while the API-key modal is open). The
    /// transcript layout reads this so the input box can grow to fit its wrapped text.
    pub input: &'a str,
    /// Byte offset of the caret inside `input` (mirrors `App::byte_cursor`).
    /// The box grows one extra row when the caret rests past the last wrapped
    /// line (e.g. just after an inserted newline), so its height matches what
    /// [`composer::draw_composer`] actually renders.
    pub byte_cursor: usize,
    /// When true, the hint bar and input box are hidden (overlay modal open).
    pub chrome_hidden: bool,
    /// The persistent one-row outbox summary pinned below the transcript gap.
    /// Its `items` slice is the viewed session's queued dispatches; an empty
    /// slice renders a muted empty state so the bar is always present (the
    /// permanent home for queue affordances).
    pub queue_bar: QueueBarProps<'a>,
    /// The background tasks status bar (ADR-0212).
    pub tasks_bar: TasksBarProps<'a>,
    /// The daemon's persistence-writer degradation (ADR-0196 D4). While set
    /// (and not `Healthy`), the footer stack reserves a retained one-row
    /// banner between the transcript gap and the queue bar; `Healthy` /
    /// `None` place nothing.
    pub persistence_health: Option<&'a nuo_wire::monitor::PersistenceHealth>,
    /// When set, the view is zoomed into a subagent task: a contextual page
    /// header is rendered and `messages` is the focused task's child stream.
    pub subagent_bar: Option<SubagentBarInfo>,
    /// When set, the view is inside a `/btw` aside (ADR-0017/0103): the
    /// contextual view header carries the coarse primary-session status on
    /// row 1 and the aside's affordance legend on row 2.
    pub side_banner: Option<view_header::BtwHead>,
    /// Live-asides chip + interruptibility for the header band's row-2
    /// legend (ADR-0103 §3). `None` suppresses the legend entirely (non-app
    /// contexts).
    pub page_hints: Option<view_header::ViewHints<'a>>,
    /// Session identity for the Conversation scene's head row: the persistent-id tail
    /// plus the tilde-shortened workspace on the left, and the session mode
    /// (`DELEGATED`) on the right. `None` only in non-session contexts
    /// (tests/showcase) where no ambient session exists.
    pub session_head: Option<SessionHead<'a>>,
    /// Wall-clock instant the current round started, or `None` between rounds.
    /// Drives the muted `<elapsed>` segment in the activity bar.
    pub round_started_at: Option<std::time::Instant>,
    /// Message index of the step (tool step or reasoning trace) whose header
    /// currently rests under the mouse pointer (inline or sticky pinned), so
    /// the next draw lights it up to the intermediate hover tone as a click
    /// affordance. `None` whenever the pointer is elsewhere or an overlay
    /// modal is open.
    pub hovered_step: Option<usize>,
    /// Keyboard-focused activatable target. When `Some`, the matching step's
    /// summary line is painted with the focus-ring cue (a reversed fg/bg bar)
    /// so keyboard navigation via `Ctrl+↑`/`Ctrl+↓` has a clear, unambiguous
    /// visual indicator that does not compete with the hover/expand luminance
    /// channel. `None` means no step is focused.
    pub focused_target: Option<InteractiveTarget>,
    /// User-supplied ASCII logo lines (from `$XDG_CONFIG_HOME/muta/logo.txt`)
    /// that replace the built-in wordmark on the empty-state hero. `None` when
    /// no user logo is configured; the hero falls back to the built-in art.
    /// Ignored entirely when the transcript is non-empty.
    pub logo: Option<&'a [String]>,
    /// Which guidance variant the empty-state hero renders beneath the logo
    /// (ADR-0057). The app shell computes this from its onboarding + provider
    /// state; the view layer only paints what it is handed. Ignored entirely
    /// when the transcript is non-empty.
    pub guidance: EmptyStateGuidance,
    /// Carousel page index for the empty-state tour (ADR-0104). The caller
    /// derives it from wall-clock elapsed time (`carousel_page_for`), the
    /// same pattern the breathing indicator uses, so the slide cadence is
    /// independent of draw frequency. Ignored when the transcript is
    /// non-empty or the guidance variant does not rotate.
    pub carousel_index: usize,
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
    pub height_cache: Option<&'a mut HeightCache>,
}

/// Caches each transcript message's fully-laid-out height (in rows), keyed by
/// the message's stable [`TranscriptMessage::id`](crate::model::document::TranscriptMessage::id).
///
/// Correctness rests on one invariant, enforced by the caller: the cache is
/// invalidated for every changed message (or cleared for a structural change)
/// and whenever the wrap width changes ([`Self::prepare`]). So a cached height
/// is only ever consulted while the message's content **and** the
/// layout width are identical to when it was measured — making the cached row
/// count exactly reproduce a fresh layout.
#[derive(Default)]
pub struct HeightCache {
    width: u16,
    heights: std::collections::HashMap<u64, (u64, u16)>,
    virtual_index: Option<layout::VirtualLayoutIndex>,
    /// Width-independent, render-only rows derived from completed edit
    /// patches. Unlike height entries, these survive structural transcript
    /// invalidation and resize; their own source identity controls reuse.
    pub(crate) diff_cache: tools::DiffCache,
    /// Content-addressed wrapped-line cache (ADR-0184). Keyed by (content,
    /// width), so it survives transcript invalidation and resizes without an
    /// invalidation pass; only capacity eviction retires entries.
    pub wrap: BlockWrapCache,
}

impl HeightCache {
    /// Reset the cache if the wrap width changed since the last frame; heights
    /// are width-dependent, so a resize invalidates every entry. Call once at
    /// the start of a transcript pass before any [`Self::get`]/[`Self::set`].
    pub fn prepare(&mut self, width: u16) {
        if self.width != width {
            self.heights.clear();
            self.virtual_index = None;
            self.width = width;
        }
    }

    /// The cached height for message `id` matching revision `rev`, or `None` if it must be measured.
    pub fn get_with_rev(&self, id: u64, rev: u64) -> Option<u16> {
        self.heights.get(&id).and_then(
            |&(cached_rev, h)| {
                if cached_rev == rev { Some(h) } else { None }
            },
        )
    }

    /// The cached height for message `id`, regardless of revision, or `None`.
    pub fn get(&self, id: u64) -> Option<u16> {
        self.heights.get(&id).map(|&(_, h)| h)
    }

    /// Record freshly-measured height for message `id` with revision `rev`.
    pub fn set_with_rev(&mut self, id: u64, rev: u64, height: u16) {
        if self.heights.insert(id, (rev, height)) != Some((rev, height)) {
            self.virtual_index = None;
        }
    }

    /// Record the freshly-measured height for message `id`.
    pub fn set(&mut self, id: u64, height: u16) {
        self.set_with_rev(id, 0, height);
    }

    /// Drop every entry. Called when the transcript mutates (the version moved).
    pub fn clear(&mut self) {
        self.heights.clear();
        self.virtual_index = None;
    }

    /// Drop only the messages whose wrapped height changed. Streaming modifies
    /// the live tail one message at a time; retaining the frozen history is
    /// what lets a long transcript keep taking the off-screen fast path while
    /// that tail grows.
    pub fn invalidate_messages(&mut self, ids: impl IntoIterator<Item = u64>) {
        let mut changed = false;
        for id in ids {
            changed |= self.heights.remove(&id).is_some();
        }
        if changed {
            self.virtual_index = None;
        }
    }

    /// Return the exact message chunks that intersect the viewport when every
    /// settled message height is known. The index is built once after a stable
    /// transcript is measured; later frames use binary search and only draw
    /// the selected chunks.
    pub fn virtual_window(
        &mut self,
        messages: &[TranscriptMessage],
        strategy: layout::Strategy,
        scroll: usize,
        view_height: u16,
    ) -> Option<layout::VirtualWindow> {
        if self
            .virtual_index
            .as_ref()
            .is_none_or(|index| !index.matches(messages, strategy))
        {
            self.virtual_index = layout::build_virtual_index(messages, self, strategy);
        }
        self.virtual_index
            .as_ref()
            .and_then(|index| index.window(scroll, view_height))
    }
}

/// Content-addressed cache of wrapped lines (ADR-0184).
///
/// Streaming re-draws the live message every frame; without this cache every
/// frame re-wrapped *all* of the message's blocks, making per-frame cost
/// O(message length) and the whole stream O(n²). Entries are keyed by
/// (content hash, content length, wrap width) and verified by full content
/// equality on hit, so a cached wrap is bit-for-bit what a fresh
/// [`crate::text_layout::wrap_text`] would produce — frozen blocks (the
/// majority of a long transcript) wrap exactly once per width for the
/// session's lifetime. Width is part of the key, so a resize needs no
/// invalidation pass.
#[derive(Default)]
pub struct BlockWrapCache {
    lines: std::collections::HashMap<WrapKey, std::sync::Arc<CachedWrap>>,
    /// Prepared code-band geometry: per logical line, the wrapped rows (with
    /// byte offsets local to the logical line) plus the gutter width — the
    /// full per-frame re-derivation this replaces for `Block::Code`.
    code: std::collections::HashMap<WrapKey, std::sync::Arc<PreparedCode>>,
    /// FIFO eviction order; the cache is a perf aid, not a semantic store.
    order: std::collections::VecDeque<(WrapKind, WrapKey)>,
}

const WRAP_CACHE_LINES_CAP: usize = 2048;
const WRAP_CACHE_CODE_CAP: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum WrapKind {
    Lines,
    Code,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct WrapKey {
    hash: u64,
    len: u32,
    width: u32,
    /// Distinguishes plain [`crate::text_layout::wrap_text`] from
    /// markup-aware [`crate::text_layout::wrap_text_markup`] at the same
    /// content + width, so the two never alias one cache slot.
    markup: bool,
}

struct CachedWrap {
    content: String,
    lines: std::sync::Arc<Vec<crate::text_layout::WrappedLine>>,
}

pub(crate) struct PreparedCode {
    content: String,
    /// (byte offset of the logical line in `content`, wrapped rows with
    /// offsets local to that logical line).
    pub logical: Vec<(usize, Vec<crate::text_layout::WrappedLine>)>,
}

fn wrap_key(content: &str, width: usize) -> WrapKey {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(content.as_bytes());
    WrapKey {
        hash: hasher.finish(),
        len: content.len() as u32,
        width: width as u32,
        markup: false,
    }
}

/// Like [`wrap_key`] but tagged as the markup-aware variant so the two wrap
/// modes never share a cache slot at the same content + width.
fn wrap_key_markup(content: &str, width: usize) -> WrapKey {
    WrapKey {
        markup: true,
        ..wrap_key(content, width)
    }
}

impl BlockWrapCache {
    /// Cached [`crate::text_layout::wrap_text`]. Hits are verified by full
    /// content equality, so a (hash, length) collision can never return wrong
    /// geometry.
    pub(crate) fn wrap_text(
        &mut self,
        content: &str,
        width: usize,
    ) -> std::sync::Arc<Vec<crate::text_layout::WrappedLine>> {
        let key = wrap_key(content, width);
        if let Some(entry) = self.lines.get(&key)
            && entry.content == content
        {
            return std::sync::Arc::clone(&entry.lines);
        }
        let lines = crate::text_layout::wrap_text(content, width);
        let shared = std::sync::Arc::new(lines);
        self.lines.insert(
            key,
            std::sync::Arc::new(CachedWrap {
                content: content.to_string(),
                lines: std::sync::Arc::clone(&shared),
            }),
        );
        self.touch(WrapKind::Lines, key);
        shared
    }

    /// Cached [`crate::text_layout::wrap_text_markup`]: like [`Self::wrap_text`]
    /// but inline-markup delimiters count as zero display width. The hidden
    /// ranges are derived deterministically from `content`'s inline scan, so
    /// keying the cache on content + width is sufficient.
    pub(crate) fn wrap_text_markup(
        &mut self,
        content: &str,
        width: usize,
        hidden_ranges: &[(usize, usize)],
    ) -> std::sync::Arc<Vec<crate::text_layout::WrappedLine>> {
        let key = wrap_key_markup(content, width);
        if let Some(entry) = self.lines.get(&key)
            && entry.content == content
        {
            return std::sync::Arc::clone(&entry.lines);
        }
        let lines = crate::text_layout::wrap_text_markup(content, width, hidden_ranges);
        let shared = std::sync::Arc::new(lines);
        self.lines.insert(
            key,
            std::sync::Arc::new(CachedWrap {
                content: content.to_string(),
                lines: std::sync::Arc::clone(&shared),
            }),
        );
        self.touch(WrapKind::Lines, key);
        shared
    }

    /// Cached code-band preparation: logical-line split, gutter width, and
    /// per-logical-line wrapping. Byte offsets are local to each logical
    /// line, exactly as a fresh wrap would produce.
    pub(crate) fn prepare_code(
        &mut self,
        content: &str,
        width: usize,
    ) -> std::sync::Arc<PreparedCode> {
        let key = wrap_key(content, width);
        if let Some(entry) = self.code.get(&key)
            && entry.content == content
        {
            return std::sync::Arc::clone(entry);
        }
        let mut logical: Vec<(usize, Vec<crate::text_layout::WrappedLine>)> = Vec::new();
        let mut offset = 0usize;
        for line in content.split('\n') {
            let wrapped = crate::text_layout::wrap_text(line, width);
            let wrapped = if wrapped.is_empty() {
                vec![crate::text_layout::WrappedLine {
                    text: String::new(),
                    start_byte: 0,
                    end_byte: 0,
                }]
            } else {
                wrapped
            };
            logical.push((offset, wrapped));
            offset += line.len() + 1; // +1 for the '\n'
        }
        let prepared = std::sync::Arc::new(PreparedCode {
            content: content.to_string(),
            logical,
        });
        self.code.insert(key, std::sync::Arc::clone(&prepared));
        self.touch(WrapKind::Code, key);
        prepared
    }

    fn touch(&mut self, kind: WrapKind, key: WrapKey) {
        self.order.push_back((kind, key));
        while self.lines.len() > WRAP_CACHE_LINES_CAP || self.code.len() > WRAP_CACHE_CODE_CAP {
            let Some((kind, key)) = self.order.pop_front() else {
                break;
            };
            match kind {
                WrapKind::Lines => {
                    self.lines.remove(&key);
                }
                WrapKind::Code => {
                    self.code.remove(&key);
                }
            }
        }
    }
}

/// Page-header context for the TaskInspection scene (shown when zoomed into a task).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentBarInfo {
    /// The subagent's role (`explore` / `plan` / …), when the `Started` event
    /// has identified it. Rendered as the `[ROLE]` tag between the `SUBAGENT`
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

pub struct TranscriptRender {
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
    /// Total height (in lines) of the rendered message stream, ignoring the
    /// viewport clip. Used by the app loop to pin the view to the bottom.
    pub content_lines: usize,
    /// Height of the transcript viewport.
    pub view_height: u16,
    /// The expanded step whose body is currently scrolled into view, so the app
    /// can render/click a sticky header pinned under the HUD bar. `None` when no
    /// expanded step body covers the top of the viewport.
    pub sticky: Option<StickyInfo>,
}

/// A sticky pinned step summary (returned to the app for click handling).
pub struct StickyInfo {
    pub message_idx: usize,
    pub rect: Rect,
    /// The content-line index of the real summary inside the stream. The app
    /// uses this to re-anchor the scroll offset when the user collapses the
    /// pinned step, so the real summary takes the sticky's place at the top of
    /// the viewport instead of jumping to unrelated content.
    pub summary_line: usize,
}

/// Draw the main transcript area, recording layout info.
pub fn draw_transcript(
    frame: &mut Frame,
    layout_map: &mut LayoutMap,
    props: TranscriptProps<'_>,
) -> TranscriptRender {
    let TranscriptProps {
        messages,
        scroll,
        selection,
        cell_selection,
        activity,
        backoff_clause,
        awaiting_permission,
        spinner_phase,
        input,
        byte_cursor,
        chrome_hidden,
        queue_bar,
        tasks_bar,
        persistence_health,
        subagent_bar,
        side_banner,
        page_hints,
        session_head,
        round_started_at,
        hovered_step,
        focused_target,
        logo,
        guidance,
        carousel_index,
        theme,
        layout,
        height_cache,
    } = props;
    // Outside the app loop (tests/showcase) no persistent cache is supplied;
    // fall back to a throwaway so every lookup simply misses and the renderer
    // behaves exactly as before the cache existed.
    let mut fallback_height_cache = HeightCache::default();
    let height_cache = height_cache.unwrap_or(&mut fallback_height_cache);
    let full = frame.area();

    // Paint the entire frame with the app background so the TUI owns every
    // pixel rather than leaving gaps at the terminal emulator's default color.
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(theme.surface())),
        full,
    );

    // Too-small terminal guard
    // When the terminal is resized below the usable minimum, the layout math
    // (footer split, composer height, gutter columns) would underflow or
    // produce an unusable UI — and a degenerate 0×0 / 1×1 geometry risks an
    // integer-underflow panic deep in a subtraction chain. Instead of drawing
    // garbage, hide the entire UI and show a single centered notice telling
    // the user how large the terminal must be. The footer chrome is suppressed
    // by returning zero-sized rects, so the app loop renders nothing else.
    if full.width < MIN_TERMINAL_COLS || full.height < MIN_TERMINAL_ROWS {
        draw_too_small_notice(frame, full, theme);
        return TranscriptRender {
            input_rect: Rect::default(),
            hint_rect: Rect::default(),
            footer: PlacedFooter::default(),
            content_lines: 0,
            view_height: 0,
            sticky: None,
        };
    }

    // Resolve every transcript page to one page-header model. The Main
    // session view always carries a head (its ambient session state — id
    // tail, workspace, mode — replaces the old bottom status bar). Subagent
    // and `/btw` keep their contextual headers. Subagent and `/btw` are
    // mutually exclusive in the app; preferring Subagent here is a defensive
    // fallback that keeps rendering deterministic if a malformed caller
    // supplies both.
    let view_header = subagent_bar
        .as_ref()
        .map(ViewHeader::Subagent)
        .or_else(|| side_banner.map(ViewHeader::Btw))
        .or_else(|| session_head.as_ref().map(ViewHeader::Session));
    // The row-2 affordance legend (ADR-0103 §3, demand-gated by ADR-0104).
    // The destructured `page_hints` is pre-resolved by the caller (it needs
    // app-level state — the aside chip and interruptibility — that the view
    // struct carries precisely so this stays allocation-free here). Row 2 is
    // reserved only while the view has page-specific affordances that no
    // other surface already carries; otherwise the band collapses to the
    // single identity row and the transcript reclaims the line.
    let page_hints_view = page_hints.filter(|hints: &ViewHints<'_>| hints.has_content());

    // When a head band is present it occupies the top rows of the terminal
    // directly — the head is a sibling of the transcript, not content inside
    // it, so it replaces the viewport's top margin rather than nesting under
    // it. The band is identity/status on row 1 plus — only while the view
    // has page-specific affordances to announce (ADR-0104; see
    // `ViewHints::has_content`) — the view-affordance legend on row 2, both
    // carved off with one layout split. Without a head, the standard
    // viewport margins apply. TaskInspection reserves no second row and no
    // bottom band: the scene's whole keyboard surface is one `Esc` and a pair
    // of remappable sibling walks, which the head cannot render faithfully
    // under a remap (ADR-0205), so the transcript keeps those rows.
    let (head_rect, hints_rect, viewport) = if view_header.is_some() {
        let full = frame.area();
        // The head band's height is demand-driven (ADR-0104): row 2 is
        // reserved only while the view has page-specific affordances.
        // `PAGE_HEADER_ROWS` stays the recorded ceiling.
        let band_rows = (1 + u16::from(page_hints_view.is_some())).min(PAGE_HEADER_ROWS);
        let sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(band_rows.saturating_sub(1)),
                Constraint::Min(0),
            ])
            .split(full);
        (
            // The head band spans the terminal's full width — it is top-level
            // chrome pinned to the top edge, not a transcript-area
            // component. Its *text* keeps the shared horizontal inset (applied
            // inside `draw_view_header` as pad spans) so it stays aligned with
            // the transcript band below.
            Some(sub[0]),
            (band_rows > 1).then_some(sub[1]),
            // The remaining area keeps the bottom viewport margin (0) but
            // drops the top one (the head owns that row now).
            Rect::new(
                sub[2].x,
                sub[2].y,
                sub[2].width,
                sub[2].height.saturating_sub(VIEWPORT_BOTTOM_MARGIN),
            ),
        )
    } else {
        (None, None, viewport_rect(frame))
    };

    let size = viewport;

    // When zoomed into a subagent task, the footer (status bar, plan panel,
    // input box, hint bar) is hidden: the task detail page is a read-only view
    // whose only chrome is its page header.
    let in_subagent = subagent_bar.is_some();

    // The activity bar (animated spinner + activity text) sits directly above the
    // input box, below the ambient todo/queue meta bars. It is shown for every
    // active phase — including streaming ("responding"), which is the longest
    // phase and the one where the breathing dot's liveness signal matters most
    // — and hidden only when the harness is idle, so the row returns to the
    // transcript.
    //
    // A pending permission request is an exception: the permission sheet
    // replaces the input box and the bars beneath it, so the activity bar is
    // the only live status surface left. Force it on so the user always has a
    // visible "awaiting permission" anchor above the sheet — even if the loop
    // has nominally gone idle (e.g. right after an interrupt that rejects
    // permissions but before the stale round's terminal snapshot lands).
    let activity_active = !chrome_hidden
        && !in_subagent
        && (awaiting_permission || (!activity.is_empty() && activity != "idle"));
    // The activity bar is purely transient now: it shows only while a round is
    // active and hides when idle, so the row returns to the transcript (the
    // persistent task-list summary has its own bar above).
    let activity_row_needed = activity_active;
    let activity_height: u16 = if activity_row_needed {
        ACTIVITY_BAR_ROWS
    } else {
        0
    };

    // The todo bar leads the footer stack and surfaces the live task list —
    // The queue bar surfaces pending outbox messages. It is hidden while the
    // viewed session's queue is empty (the common idle case) so an ordinary
    // session reclaims the row; it appears the moment a message is staged
    // and stays up until the outbox drains, so the user always has a glanceable
    // surface while there is pending work.
    let queue_row_needed = !chrome_hidden && !in_subagent && !queue_bar.items.is_empty();
    let queue_height: u16 = if queue_row_needed { QUEUE_BAR_ROWS } else { 0 };
    // ADR-0212: Background tasks status bar reserves a row while tasks are active or recently settled.
    let tasks_row_needed =
        !chrome_hidden && !in_subagent && tasks_bar.tasks.iter().any(|t| !t.dismissed);
    let tasks_height: u16 = if tasks_row_needed { 1 } else { 0 };
    // The durability-health banner (ADR-0196 D4) reserves a row only while
    // the writer is actually degraded; a healthy writer costs nothing.
    let persistence_health_row_needed = !chrome_hidden
        && !in_subagent
        && persistence_health.is_some_and(|health| !health.is_healthy());
    let persistence_health_height: u16 = if persistence_health_row_needed { 1 } else { 0 };

    // The input box grows with its content: the typed text wraps onto new
    // lines and the box expands to fit, up to roughly half the terminal so the
    // transcript history always stays visible. The inner text width reserves the
    // footer insets, the `> ` prompt prefix, and the matching right pad so the
    // height calculation wraps at the same width the composer renders.
    let input_text_width = composer_layout_text_width(size.width as usize);
    let input_wrapped_lines = composer::input_row_count(input, input_text_width, byte_cursor);
    let desired_input_height = input_wrapped_lines as u16 + COMPOSER_VERTICAL_CHROME_ROWS;
    let max_input_height = (size.height / COMPOSER_MAX_HEIGHT_DIVISOR).max(COMPOSER_MIN_HEIGHT);
    let input_box_height = if in_subagent {
        0
    } else {
        desired_input_height.min(max_input_height)
    };
    // The hint bar is a single-line status strip pinned flush below the input
    // box — the composer's own bottom panel-bg padding row is all the
    // separation it needs (COMPOSER_HINT_GAP_ROWS = 0). It carries the
    // next Enter action plus ambient model/context info. Hidden alongside the
    // rest of the chrome while an overlay is open.
    let hint_height: u16 = if chrome_hidden || in_subagent {
        0
    } else {
        MODEL_BAR_ROWS
    };
    // The composer/hint gap is 0 and the activity/composer gap is 0: the
    // hint bar sits flush against the composer's bottom edge and the activity
    // bar flush against the composer's top edge (each side's panel-bg padding
    // row is the separation). These zero-gap tokens are therefore structural:
    // adjacent rows in the footer stack are flush by construction, and no gap
    // row is ever placed. The tokens stay in `design.rs` as the recorded
    // decision, asserted by the layout tests.
    // The footer stack is declared once, in draw order — the single-pass
    // placer derives both the band's total height (for the layout split) and
    // each row's rect, so the height arithmetic can no longer exist in two
    // copies that drift. Order, top → bottom: gap, queue bar, tasks bar,
    // activity bar, input box, hint bar.
    let footer_rows: Vec<FooterRow> = if chrome_hidden || in_subagent {
        Vec::new()
    } else {
        vec![
            FooterRow {
                id: FooterRowId::TopGap,
                height: FOOTER_TOP_GAP_ROWS,
            },
            FooterRow {
                id: FooterRowId::PersistenceHealth,
                height: persistence_health_height,
            },
            FooterRow {
                id: FooterRowId::Queue,
                height: queue_height,
            },
            FooterRow {
                id: FooterRowId::Tasks,
                height: tasks_height,
            },
            FooterRow {
                id: FooterRowId::Activity,
                height: activity_height,
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
    let footer_height: u16 = footer_stack::measure(&footer_rows);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),                // Transcript page
            Constraint::Length(footer_height), // Activity? + input box + hint bar + status bar
        ])
        .split(size);

    // 1. Head band — drawn at the very top of the terminal, before the
    // transcript. The band is a sibling of the transcript, not content inside
    // it, so it was already split from `full` above; just paint it here.
    // Row 1 carries identity/status; row 2 the view-affordance legend.
    if let (Some(header), Some(rect)) = (view_header.as_ref(), head_rect) {
        draw_view_header(frame, rect, header, theme);
    }
    if let (Some(hints), Some(rect)) = (page_hints_view.as_ref(), hints_rect) {
        draw_view_header_hints(frame, rect, hints, theme);
    }

    // 2. Transcript History — the transcript area is the whole `chunks[0]`
    // when a head is present (it was already accounted for above), or the
    // entire viewport-minus-footer when there is no head.
    let transcript_area = chunks[0];
    // Apply the uniform horizontal inset (`TRANSCRIPT_H_INSET` on each side)
    // exactly once, here at the transcript-stream entry point. Every
    // downstream component receives `band` — an already-inset rect — so none
    // of them re-clips or hand-pads a leading gutter. The empty-state hero is
    // the sole exception: it centers across the full viewport, so it keeps
    // `transcript_area` (un-inset). The page header is rendered from its own
    // layout-split rect before this point, so it is unaffected.
    let band = transcript_band_rect(transcript_area);
    // Account for scroll. Owned by the layout `Stream` once the loop runs; not
    // mutated locally here unless a virtual index selects a later chunk.
    let mut skip_rows = scroll as usize;
    // Total stream height, counted independently of the viewport clip so the
    // app loop can follow the bottom.
    let mut content_lines: usize = 0;
    let mut message_start = 0usize;
    let mut message_end = messages.len();
    let mut virtual_total_lines = None;
    // Expanded steps collected during the pass, for the sticky pinned header.
    let mut sticky_steps: Vec<StickyStep> = Vec::new();

    // Empty-state replacement (ADR-0033): when the session has no messages and
    // no subagent/side view is open, the transcript is replaced by a centered
    // logo hero rather than rendering an empty stream. This is a component
    // substitution, not transcript content — the hero never participates in
    // scroll, selection, or attribution, so the whole message-rendering
    // pipeline (loop, badges, sticky pinning) is skipped. The footer below
    // renders exactly as in a live session.
    let show_empty_state = messages.is_empty() && subagent_bar.is_none() && side_banner.is_none();

    if show_empty_state {
        empty_state::draw_empty_state(
            frame,
            transcript_area,
            logo,
            guidance.clone(),
            carousel_index,
            theme,
        );
        // Account for the hero so the app loop does not treat the session as a
        // zero-height stream (which would mis-pin the scroll position).
        content_lines = empty_state::empty_state_content_lines(logo, guidance);
    } else {
        // Stage 2: heights are wrap-width-dependent, so drop the cache on a
        // resize. Within a stable width + unchanged transcript every entry
        // reproduces a fresh layout exactly, so off-screen messages can be
        // advanced from their cached height instead of being re-wrapped.
        height_cache.prepare(band.width);

        if let Some(window) =
            height_cache.virtual_window(messages, layout, scroll as usize, band.height)
        {
            message_start = window.message_start;
            message_end = window.message_end;
            content_lines = window.prefix_lines;
            skip_rows = window.skip_rows;
            if window.is_full {
                virtual_total_lines = Some(window.total_lines);
            }
        }

        // Delegate message arrangement to the selected layout strategy. The
        // `Stream` carries every piece of shared render state (scroll/Y
        // accounting, layout map, height cache, theme, hover/focus) and
        // exposes `badge` / `dispatch` / `gap` as the sanctioned mutations, so
        // every layout agrees on scroll semantics. The layout leaves
        // `content_lines` and `sticky_steps` populated for the
        // post-processing below.
        let workspace_root = session_head
            .as_ref()
            .map(|h| h.workspace)
            .filter(|w| !w.is_empty())
            .map(std::path::Path::new);

        let mut stream = layout::Stream {
            frame,
            band,
            messages,
            theme,
            layout_map,
            height_cache,
            selection,
            cell_selection,
            hovered_step,
            focused_target,
            workspace_root,
            message_start,
            message_end,
            virtual_total_lines,
            current_y: band.y,
            skip_rows,
            content_lines,
            sticky_steps,
        };
        layout.build().run(&mut stream);
        // Recover the loop state the post-processing below needs.
        // (`skip_rows` and `current_y` are loop-internal and are not read
        // after the layout returns, so they are not recovered.)
        content_lines = stream.content_lines;
        sticky_steps = stream.sticky_steps;
    } // end else (non-empty transcript branch)

    // Record the interactive transcript rect so clicks on any blank
    // transcript space — gap rows between messages, the empty region below
    // the last message, and the outer gutters — park keyboard attention on
    // the transcript (ADR-0174 browse focus). The rect spans the full
    // transcript viewport: the user's mental model is "the whole transcript
    // area is the browse surface", so no part of it is a dead click.
    // Skipped for the empty-state hero, which owns its own rect and is not
    // part of the interactive transcript surface.
    if !show_empty_state {
        layout_map.set_transcript_content_rect(Rect::new(
            transcript_area.x,
            transcript_area.y,
            transcript_area.width,
            transcript_area.height,
        ));
    }

    // The footer stacks, from top to bottom: a permanent blank separator,
    // the persistent todo bar (when the task list is non-empty), the
    // persistent queue bar (when the outbox is non-empty), the transient
    // activity bar (when active), the input box, and the hint bar. The
    // activity bar sits flush above the input box so the live "what the
    // agent is doing right now" status reads as part of the composer cluster;
    // the todo and queue bars are ambient meta-info and float above it. The
    // separator keeps the latest response visually distinct from the controls
    // even when the activity row appears or disappears.
    // One `place` pass walks the declared stack and yields every row's rect
    // plus the hit-test registry; each draw call below just looks its own
    // rect up. The height sum (which feeds the layout split) and the per-row
    // offsets can no longer drift apart — they are the same traversal now.
    // `place` applies the shared `FOOTER_H_INSET` extent itself, so no
    // hand-derived `footer_x`/`footer_w` remains.
    let placed_footer = footer_stack::place(chunks[1], &footer_rows);

    // The durability-health banner (ADR-0196 D4) leads the visible chrome:
    // a degraded writer must be seen before anything else in the footer.
    if let Some((rect, state)) =
        footer_stack::rect_of(&placed_footer, FooterRowId::PersistenceHealth)
            .filter(|_| persistence_health_row_needed)
            .zip(persistence_health)
    {
        draw_persistence_health_bar(frame, rect, state, theme);
    }

    // The persistent queue bar leads the footer stack below the top gap. It is a
    // stable one-row outbox summary so pending messages never have to be
    // inferred from the hint bar. The whole bar is the click target that
    // expands the full Queue modal.
    footer_stack::rect_of(&placed_footer, FooterRowId::Queue)
        .filter(|_| queue_row_needed)
        .map(|rect| draw_queue_bar(frame, rect, queue_bar, theme));

    // ADR-0212: Draw the background tasks status bar.
    footer_stack::rect_of(&placed_footer, FooterRowId::Tasks)
        .filter(|_| tasks_row_needed)
        .map(|rect| draw_tasks_bar(frame, rect, tasks_bar, theme));

    // The transient activity bar sits directly above the input box so the live
    // "what the agent is doing right now" status reads as part of the composer
    // cluster rather than floating above the ambient meta bars. It stays up for
    // the entire active round lifecycle (queued → responding → tool work →
    // finalizing), including the streaming phase, and hides only when idle.
    // Keeping it up during "responding" avoids a layout shift at the stream
    // boundary and sustains the breathing-dot liveness anchor (ADR-0008)
    // through the longest phase.
    footer_stack::rect_of(&placed_footer, FooterRowId::Activity)
        .filter(|_| activity_row_needed)
        .and_then(|rect| {
            draw_activity_bar(
                frame,
                rect,
                round_started_at,
                ActivityBarProps {
                    status: activity,
                    backoff_clause,
                    awaiting_permission,
                },
                spinner_phase,
                theme,
            )
        });

    // The input box sits flush directly below the activity bar — the
    // composer's top panel-bg padding row already separates its text from
    // the live status line, so no `surface` gap row is reserved between them.
    let input_rect = footer_stack::rect_of(&placed_footer, FooterRowId::Composer)
        .filter(|_| input_box_height > 0)
        .unwrap_or_default();

    // The model bar sits directly below the input box, carrying the ambient
    // gauges (model/context/rate). Its rect is computed even though its draw
    // call is delegated to the app loop (which owns the context-token
    // source); composer-owned rows live inside `input_rect` itself.
    let hint_rect = footer_stack::rect_of(&placed_footer, FooterRowId::ModelBar)
        .filter(|_| hint_height > 0)
        .unwrap_or_default();

    // Sticky pinned summary: if an expanded step's body covers the top of the
    // viewport (its summary is scrolled out of view), pin its summary to the
    // line directly under the HUD bar so the user can always collapse it.
    let sticky_info = draw_sticky_summary_if_needed(frame, band, &sticky_steps, scroll, theme);

    TranscriptRender {
        input_rect,
        hint_rect,
        footer: placed_footer,
        content_lines,
        view_height: transcript_area.height,
        sticky: sticky_info,
    }
}

#[cfg(test)]
mod tests;
