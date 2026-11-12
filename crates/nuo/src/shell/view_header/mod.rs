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
//!     (`‹  thread › subagent  ›`) with interactive navigation targets (ADR-0092 [INV-SCENE-03, INV-INTERACT-01]).
//!     The affordances are pure chevrons: chord text lives only in the Help
//!     dialog, resolved from the live keymap (ADR-0131 [INV-HELP-04]).
//!
//! - **Tier 3 — Sovereign ThreadContextBar (`draw_thread_context_bar`).**
//!   Height: 1 row on Thread viewports; **0 rows on non-thread scenes** (`Dashboard`, `Settings`)
//!   [INV-HEAD-05]. Reads *who → how → where* across the row: the staffing
//!   `[ROLE]` badge (brand), the persistent safety posture tokens (`auto`,
//!   `open`; warning tone) on the left, and the bound workspace path pinned to
//!   the right edge, where it shortens from the ancestor end and therefore
//!   never moves the governance cluster around.
use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use super::{
    CONTEXT_BAR_WORKSPACE_MIN_COLS, JOIN_ENUMERATE_COLS, STEP_MIN_WIDTH, TRANSCRIPT_H_INSET, Theme,
};
use nuo_ui_primitives::components::path::{PathFormatStrategy, PathStyle, PathView};

/// Canonical scene identifier alias for header presentation (ADR-0042).
pub(crate) type ViewKind = crate::shell::surfaces::SceneKind;

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
    /// Scene context shown: the thread's authentic chat title, a subagent's
    /// task label, the dashboard's fleet summary, or the settings breadcrumb.
    pub context: Option<&'a str>,
    /// `true` while the context is an attention state (e.g. the dashboard has
    /// a session needing approval), escalating it to the warning tone.
    pub context_warn: bool,
    /// Optional breadcrumbs from the active tab's history stack (ADR-0040, ADR-0042, ADR-0060).
    pub breadcrumbs: Option<&'a [crate::shell::surfaces::SceneKind]>,
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
pub struct TabBarProps<'a> {
    pub tabs: Option<&'a [crate::shell::surfaces::ClientTab]>,
    pub active_tab: usize,
    /// Unread fault notices requiring persistent attention (ADR-0131).
    pub unread_faults: usize,
}

/// Canonical thread execution governance context (ADR-0043, ADR-0060, ADR-0062 [INV-HEAD-05, INV-HEAD-07, INV-HEAD-08]).
///
/// Tier 3: Sovereign ThreadContextBar is owned strictly by the Thread domain.
/// Displays staffing role, safety posture tokens, and bound workspace.
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabBarNavRects {
    /// Bounding rects for each rendered tab in the top tab bar: `(tab_index, rect)`.
    pub tab_rects: Vec<(usize, Rect)>,
    /// Bounding rect for left overflow indicator (`‹N`), if present.
    pub prev_rect: Option<Rect>,
    /// Bounding rect for right overflow indicator (`N›`), if present.
    pub next_rect: Option<Rect>,
    /// Bounding rect for overflow overview / switcher button (`[▾]`), if present.
    pub overview_rect: Option<Rect>,
    /// Bounding rect for unread fault notice badge (`[!N]`), if present (ADR-0131).
    pub notices_rect: Option<Rect>,
    /// Bounding rect for help affordance button (`F1 help`), if present (ADR-0131).
    pub help_rect: Option<Rect>,
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
            prev_rect: None,
            next_rect: None,
            overview_rect: None,
            notices_rect: None,
            help_rect: None,
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

pub(crate) use self::scene_bar::{SceneNavRects, draw_scene_bar};
#[cfg(test)]
pub(crate) use self::tab_bar::compute_tab_window;
pub(crate) use self::tab_bar::draw_tab_bar;
pub(crate) use self::thread_bar::draw_thread_context_bar;
#[cfg(test)]
pub(crate) use self::thread_bar::{PostureToken, workspace_spans};
