//! Polymorphic spatial header topology for every scene (ADR-0062, ADR-0261).
//!
//! The head band is the sovereign chrome pinned to the terminal's top edge.
//! Under ADR-0261 (`[INV-HEAD-09]`) every scene shares a uniform two-tier
//! hierarchy (`PAGE_HEADER_ROWS = 2`):
//!
//! - **Tier 1 — Client Global TabBar (`draw_tab_bar`).**
//!   Fixed height: 1 row, with `TABBAR_H_INSET` (2 columns) horizontal margin
//!   matching Tier 2 and the transcript stream while `tabbar_bg` bleeds
//!   edge-to-edge. Strictly inverted (DIP): the TabBar knows nothing about
//!   concrete tab or scene models (`ClientTab`, `TabKind`, thread IDs); it
//!   renders an abstract slice of [`TabBarItem`]s projected each frame by the
//!   active scene inside each tab container, strictly omitting ordinal prefixes
//!   (`1:`) [INV-TAB-09]. Live execution and unseen outcomes occupy **zero**
//!   extra columns: `Running` sweeps a calm left-to-right text shimmer across
//!   stationary title glyphs, while `Attention`, `Succeeded`, and `Failed`
//!   recolor the title text in their semantic tones. Its right edge carries the
//!   optional `[!N]` fault notice badge and a yield-first elastic help portal
//!   resolved from the live keymap (`{key} help` in idle slack → compact `{key}`
//!   under pressure → omitted when starved or unbound, ADR-0245 `[INV-TAB-19]`).
//!   The row also carries its own lifecycle (ADR-0235): overflowing titles fade
//!   smoothly into the pill background instead of appending `…`, a hovered pill
//!   replaces its trailing title columns in place with a close mark (no resting
//!   reserve, no reflow), an overflowing title slides through one bounded
//!   one-way reveal while the pointer rests on it, and the strip ends with an
//!   add affordance whose columns are budgeted before any title.
//!
//! - **Tier 2 — Polymorphic SceneBar (`draw_scene_bar`).**
//!   Fixed height: 1 row. Owned by the active scene view [INV-SCENE-02].
//!   - For `Thread` (ADR-0261 `[INV-HEAD-09]`, `[INV-HEAD-10]`; ADR-0268
//!     `[INV-HEAD-11]`, `[INV-HEAD-12]`): consolidates thread governance,
//!     authentic task title, and workspace into a single row on
//!     `theme.scenebar_bg()`. Left: staffed role token (`ROLE` brand bold on
//!     chromatic surfaces; `[ROLE]` on framed fallback), silent-by-default
//!     safety posture tokens (`auto`, `open`; warning tone), and an
//!     anchor-stable title slot (silent while untitled; bold heading once an
//!     authentic title exists). Right: bound workspace path pinned to the right
//!     edge (`PathView` ancestor-first shortening). Peer elements separate by
//!     `JOIN_ENUMERATE_COLS` pure whitespace.
//!   - For Non-Thread (`Subagent`, `Dashboard`, `Settings`, `CommandLog`, `Diff`):
//!     scene label + domain summary context on the left; right-aligned drill-in
//!     navigation stack / breadcrumbs (`‹  thread › subagent  ›`) with
//!     interactive navigation targets (ADR-0092 [INV-SCENE-03, INV-INTERACT-01]).
//!     The affordances are pure chevrons: chord text lives only in the Help
//!     dialog, resolved from the live keymap (ADR-0131 [INV-HELP-04]).
use graticule_canvas::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use nuo_ui_primitives::Theme;
use nuo_ui_primitives::components::path::{PathFormatStrategy, PathStyle, PathView};
use nuo_ui_primitives::design::{
    JOIN_ENUMERATE_COLS, SCENE_BAR_IDENTITY_MIN_COLS, SCENE_BAR_WORKSPACE_MIN_COLS, STEP_MIN_WIDTH,
    TRANSCRIPT_H_INSET,
};

pub mod types;
pub use types::{ClientTab, SceneKind, TabKind};

/// Canonical scene identifier alias for header presentation (ADR-0042).
pub type ViewKind = SceneKind;

/// Maximum display width allocated to individual tab titles before truncation.
pub(crate) const MAX_TAB_TITLE_WIDTH: usize = 20;

/// Scene context for header presentation (ADR-0060, ADR-0062).
///
/// Focuses purely on viewport context: authentic task title or domain summary
/// on the left, and navigation stack / drill-in breadcrumbs on the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewHints<'a> {
    /// Which scene the row describes — decides the leading scene label.
    pub kind: ViewKind,
    /// Optional staffed role token for non-thread scenes that carry an agent
    /// role (e.g. `SceneKind::Subagent`), painted in brand bold via the same
    /// capability-adaptive rule as `Thread` (`EXPLORE` chromatic / `[EXPLORE]`
    /// framed — ADR-0268 `[INV-HEAD-11]`).
    pub role: Option<&'a str>,
    /// Scene context shown: the thread's authentic chat title, a subagent's
    /// task label, the dashboard's fleet summary, or the settings breadcrumb.
    pub context: Option<&'a str>,
    /// `true` while the context is an attention state (e.g. the dashboard has
    /// a session needing approval), escalating it to the warning tone.
    pub context_warn: bool,
    /// Optional breadcrumbs from the active tab's history stack (ADR-0040, ADR-0042, ADR-0060).
    pub breadcrumbs: Option<&'a [crate::view_header::SceneKind]>,
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
            role: None,
            context: None,
            context_warn: false,
            breadcrumbs: None,
            can_back: false,
            can_forward: false,
        }
    }
}

/// Transient live activity or unseen terminal outcome projected onto a single
/// TabBar pill.
///
/// Rendered with **zero** extra horizontal columns: `Running` sweeps a calm
/// left-to-right text shimmer over stationary title characters, while
/// `Attention`, `Succeeded`, and `Failed` recolor the title text using the
/// theme's semantic tokens. Pill widths and title offsets never shift across
/// status transitions (`[INV-TAB-16]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TabStatus {
    /// Resting / no live activity, or terminal outcome already viewed.
    #[default]
    Idle,
    /// Work is actively executing inside this tab container (text shimmer).
    Running,
    /// Blocked waiting on human input (approval, question, or stdin).
    Attention,
    /// Completed successfully in the background (unseen outcome).
    Succeeded,
    /// Terminated with a failure or error in the background (unseen outcome).
    Failed,
}

/// Pure, domain-agnostic display projection for one slot in the Tier 1 TabBar.
///
/// Implements the Dependency Inversion boundary between the TabBar container
/// architecture and concrete scene views: the TabBar renders `&[TabBarItem]`
/// without knowing anything about `ClientTab`, `TabKind`, or `SceneKind`. Each
/// tab container's active scene produces its own `TabBarItem` every frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabBarItem<'a> {
    /// Semantic title decided by the active scene inside this tab container.
    pub title: std::borrow::Cow<'a, str>,
    /// Transient execution or unseen-outcome status decided by the scene.
    pub status: TabStatus,
}

impl<'a> TabBarItem<'a> {
    /// Create an idle tab bar item with the given scene-projected title.
    pub fn new(title: impl Into<std::borrow::Cow<'a, str>>) -> Self {
        Self {
            title: title.into(),
            status: TabStatus::Idle,
        }
    }

    /// Create a tab bar item with an explicit scene-projected status.
    pub fn with_status(title: impl Into<std::borrow::Cow<'a, str>>, status: TabStatus) -> Self {
        Self {
            title: title.into(),
            status,
        }
    }
}

/// Canonical client tabbar projection props (ADR-0039, ADR-0060, ADR-0062 [INV-TAB-09]).
///
/// Tier 1: Client Global TabBar is a pure container architecture (DIP). It
/// renders scene-projected [`TabBarItem`] slots, tracks active/hover geometry,
/// and paints client-global right-edge affordances — never inspecting concrete
/// tab kinds or thread governance metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TabBarProps<'a> {
    /// Ordered tab slots projected by each tab container's active scene.
    /// Empty falls back to the client brand mark (`nuo`).
    pub items: &'a [TabBarItem<'a>],
    pub active_tab: usize,
    /// Monotonic animation clock in milliseconds, driving the smooth
    /// left-to-right text shimmer on [`TabStatus::Running`] pills independently
    /// of frame rate.
    pub animation_ms: u64,
    /// Unread fault notices requiring persistent attention (ADR-0131).
    pub unread_faults: usize,
    /// The resolved chord for `CommandId::OpenHelp`, supplied by the caller
    /// from the live keymap (ADR-0131 `[INV-AFFORDANCE-08]`, ADR-0245
    /// `[INV-TAB-19]`). `None` suppresses the help portal cleanly — the bar
    /// never fabricates a literal chord.
    pub help_key: Option<nuo_ui_primitives::chord::Key>,
    /// What the pointer rests on inside the rail, if anything (ADR-0223, ADR-0235).
    ///
    /// The TabBar is the first chrome row whose *hover* changes what it
    /// paints: a hovered pill reveals its close glyph and may scroll its title
    /// through a bounded reveal pass. The projection therefore carries the
    /// pointer fact rather than re-deriving it from a second source, so the
    /// pill that paints the `✕` and the pill that answers the click are
    /// provably the same one.
    pub hover: Option<TabBarHover>,
    /// Milliseconds since the pointer arrived on [`Self::hover`]'s anchor.
    ///
    /// Time-based, never frame-count-based, so the reveal cadence is identical
    /// on any frame rate (`marquee_offset`).
    pub hover_elapsed_ms: u64,
}

/// What the pointer rests on inside the Client Global TabBar (ADR-0223, ADR-0235).
///
/// [`TabBarHover::Close`] is deliberately a distinct variant from
/// [`TabBarHover::Tab`]: the two paint differently (the close glyph lights up,
/// the pill highlight does not move), yet they share one *anchor* — moving from
/// a pill onto its own `✕` must not restart the title reveal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabBarHover {
    /// The rail itself: background, margins, or a gap between pills.
    Rail,
    /// A tab pill.
    Tab(usize),
    /// The close glyph inside a tab pill.
    Close(usize),
    /// The trailing add-tab affordance.
    Add,
    /// The right-edge elastic help portal (ADR-0245).
    Help,
}

impl TabBarHover {
    /// The pill whose anchor owns the pointer. The close glyph belongs to its
    /// own tab, so both variants name the same anchor.
    pub fn tab(self) -> Option<usize> {
        match self {
            Self::Tab(index) | Self::Close(index) => Some(index),
            Self::Rail | Self::Add | Self::Help => None,
        }
    }

    /// Whether the pointer is on a pill's close glyph rather than its body.
    pub fn is_close(self) -> bool {
        matches!(self, Self::Close(_))
    }

    /// The *anchor* this hover belongs to: the thing whose clock is running.
    ///
    /// Moving from a pill onto its own `✕` keeps the same anchor, so the title
    /// reveal does not rewind under the pointer; moving to another pill starts
    /// a fresh visit (the anchor rule is stated here; ADR-0223 supplies the hover input).
    pub fn anchor(self) -> Self {
        match self {
            Self::Close(index) => Self::Tab(index),
            other => other,
        }
    }
}

/// Canonical thread execution governance context projected onto the single-row
/// Thread SceneBar (ADR-0261 `[INV-HEAD-09]`, `[INV-HEAD-10]`).
///
/// Displays staffing role, silent-by-default safety posture tokens, and bound
/// workspace on `SceneKind::Thread` viewports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ThreadContext<'a> {
    /// Active staffing role for this thread (e.g. "developer").
    pub role: Option<&'a str>,
    /// Tilde-shortened workspace path (e.g. `~/projects/xx`), or empty when unbound.
    pub workspace: &'a str,
    /// `true` while the thread runs unattended (`--unattended` / `/unattended on`).
    pub unattended: bool,
    /// `false` while workspace filesystem confinement is off (`/confinement off`).
    pub confined: bool,
}

/// Interactive hit-test rects produced by Tier 1 TabBar rendering.
///
/// The close and add rects are *only* present when the affordance was painted
/// this frame (ADR-0235). A hit target with nothing on screen is a phantom
/// click region — the defect ADR-0219 removed from the status cluster — so the
/// renderer that decides what the row can show is the one that decides what can
/// be clicked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabBarNavRects {
    /// Bounding rects for each rendered tab in the top tab bar: `(tab_index, rect)`.
    pub tab_rects: Vec<(usize, Rect)>,
    /// The hovered pill's close mark, when one was painted: `(tab_index, rect)`.
    pub close_rects: Vec<(usize, Rect)>,
    /// The trailing add-tab affordance, when it fit on the row.
    pub add_rect: Option<Rect>,
    /// Bounding rect for left overflow indicator (`‹N`), if present.
    pub prev_rect: Option<Rect>,
    /// Bounding rect for right overflow indicator (`N›`), if present.
    pub next_rect: Option<Rect>,
    /// Bounding rect for overflow overview / switcher button (`[▾]`), if present.
    pub overview_rect: Option<Rect>,
    /// Bounding rect for unread fault notice badge (`[!N]`), if present (ADR-0131).
    pub notices_rect: Option<Rect>,
    /// Bounding rect for the elastic help portal (`{key} help` / `{key}`),
    /// present if and only if painted this frame (ADR-0245 `[INV-TAB-20]`).
    pub help_rect: Option<Rect>,
    /// Whether a hover title reveal is mid-pass. The event loop folds this into
    /// its `animating` predicate, so an idle strip costs no repaints while a
    /// hovered overflow still finishes its one pass (ADR-0235 `[INV-TAB-15]`).
    pub reveal_active: bool,
    /// Whether any *visible* pill painted a live `Running` text shimmer this
    /// frame. Off-screen running tabs and hover-paused running pills report
    /// `false` so the event loop spends zero 30Hz wakeups when no shimmer is on
    /// screen.
    pub shimmer_active: bool,
}

impl std::ops::Deref for TabBarNavRects {
    type Target = [(usize, Rect)];
    fn deref(&self) -> &Self::Target {
        &self.tab_rects
    }
}

impl<'a> IntoIterator for &'a TabBarNavRects {
    type Item = &'a (usize, Rect);
    type IntoIter = std::slice::Iter<'a, (usize, Rect)>;
    fn into_iter(self) -> Self::IntoIter {
        self.tab_rects.iter()
    }
}

impl From<Vec<(usize, Rect)>> for TabBarNavRects {
    fn from(tab_rects: Vec<(usize, Rect)>) -> Self {
        Self {
            tab_rects,
            ..Self::default()
        }
    }
}

/// Compute adaptive maximum display width for tab titles based on open tab count.
pub(crate) fn adaptive_tab_title_width(tab_count: usize) -> usize {
    match tab_count {
        0..=3 => MAX_TAB_TITLE_WIDTH, // 20
        4..=6 => 14,
        _ => 10,
    }
}

mod scene_bar;
mod tab_bar;
mod thread_bar;

#[cfg(test)]
mod tests;

pub use self::scene_bar::{SceneNavRects, draw_scene_bar};
#[cfg(test)]
pub(crate) use self::tab_bar::compute_tab_window;
pub use self::tab_bar::draw_tab_bar;
#[cfg(test)]
pub(crate) use self::thread_bar::{PostureToken, workspace_spans};
