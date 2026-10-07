//! TUI surface routing: Stage-Scene-Overlay architecture (ADR-0035).
//!
//! - A [`SceneKind`] is an **independent full-screen workspace** (`Conversation`,
//!   `Dashboard`, `Settings`, `TaskInspection`, `Aside`). Exactly one Scene is
//!   active at any given moment.
//! - A [`DialogKind`] names a **centered floating dialog** that floats over
//!   whatever Scene is active. Each dialog is an encapsulated entity in the
//!   [`Dialogs`] registry, owning its own cursor, scroll, input, and sub-layer
//!   state (`[INV-SURFACE-01]`).
//! - A [`SheetKind`] names an **edge-anchored action prompt** (Permission,
//!   Question, InputInjection, ModelEditor, etc.).
//! - The [`SurfaceRouter`] is the single authority managing the active Scene,
//!   the LIFO `overlay_stack`, and the dialog entity registry.
//!
//! ## Domain scoping (ADR-0035)
//!
//! Every dialog declares exactly one [`DialogScope`]: `Global` (survives
//! session and scene switches), `Session` (bound to the ambient session), or
//! `Scene` (bound to one workspace). Precondition gating
//! ([`DialogKind::is_available`]) hides and refuses a dialog whose context is
//! absent, and overlay eviction always runs the entity's `on_dismiss` hook
//! through the structured unwinding pipeline — never a blunt `clear()`
//! (`[INV-SURFACE-03]`, `[INV-SURFACE-04]`).

#![allow(dead_code)]

mod dialogs;

pub use dialogs::{DialogRenderCtx, DialogView, Dialogs};

/// Root full-screen scene identifier (closed set of destinations).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SceneKind {
    /// The live conversation: transcript + composer. The default workspace.
    #[default]
    Conversation,
    /// The session and cluster dashboard (`/dashboard`).
    Dashboard,
    /// The full-screen settings center (`/config` / `/settings`).
    Settings,
    /// Deep-dive inspection into a subagent/envoy task's transcript.
    TaskInspection,
    /// An aside's transcript (`/btw`).
    Aside,
}

impl SceneKind {
    /// The label shown in the quick switcher and used for fuzzy matching.
    pub fn label(self) -> &'static str {
        match self {
            Self::Conversation => "Session",
            Self::Dashboard => "Session dashboard",
            Self::Settings => "Settings",
            Self::TaskInspection => "Subagent task",
            Self::Aside => "Aside",
        }
    }

    /// The secondary line the switcher shows under the label.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Conversation => "Esc  home",
            Self::Dashboard => "/dashboard",
            Self::Settings => "/config  /settings",
            Self::TaskInspection => "zoom a subagent task",
            Self::Aside => "focus an aside",
        }
    }
}

/// Centered, reference and management dialogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DialogKind {
    Tools,
    Mcp,
    Skills,
    Permissions,
    UsageStats,
    Telemetry,
    Asides,
    Models,
    Connections,
    HistorySearch,
    Queue,
    Sessions,
    SessionTree,
    Switcher,
}

impl DialogKind {
    /// Every dialog id that appears in the switcher's reference/discovery list.
    pub const ALL: [DialogKind; 13] = [
        DialogKind::Tools,
        DialogKind::Mcp,
        DialogKind::Skills,
        DialogKind::Permissions,
        DialogKind::UsageStats,
        DialogKind::Telemetry,
        DialogKind::Asides,
        DialogKind::Models,
        DialogKind::Connections,
        DialogKind::HistorySearch,
        DialogKind::Queue,
        DialogKind::Sessions,
        DialogKind::SessionTree,
    ];

    /// The label shown in the quick switcher and used for fuzzy matching.
    pub fn label(self) -> &'static str {
        match self {
            DialogKind::Tools => "Tools",
            DialogKind::Mcp => "MCP servers",
            DialogKind::Skills => "Skills",
            DialogKind::Permissions => "Permissions",
            DialogKind::UsageStats => "Usage stats",
            DialogKind::Telemetry => "Session telemetry",
            DialogKind::Asides => "Asides (/btw)",
            DialogKind::Models => "Switch model",
            DialogKind::Connections => "Connections",
            DialogKind::HistorySearch => "History",
            DialogKind::Queue => "Queue (outbox)",
            DialogKind::Sessions => "Sessions",
            DialogKind::SessionTree => "Session tree",
            DialogKind::Switcher => "Quick switcher",
        }
    }

    /// The secondary hint line in the switcher.
    pub fn hint(self) -> &'static str {
        match self {
            DialogKind::Tools => "/tools",
            DialogKind::Mcp => "/mcp",
            DialogKind::Skills => "/skills",
            DialogKind::Permissions => "/permissions",
            DialogKind::UsageStats => "/usage",
            DialogKind::Telemetry => "/telemetry",
            DialogKind::Asides => "/btw",
            DialogKind::Models => "/models",
            DialogKind::Connections => "/connections",
            DialogKind::HistorySearch => "Ctrl-r",
            DialogKind::Queue => "/queue",
            DialogKind::Sessions => "/sessions",
            DialogKind::SessionTree => "/tree",
            DialogKind::Switcher => "C-x p",
        }
    }

    /// This dialog's ownership domain (`[INV-SURFACE-02]`). Exactly one scope
    /// per dialog; the matrix is defined once, here.
    pub fn scope(self) -> DialogScope {
        match self {
            // Global: the terminal app's own surfaces. They survive session
            // and scene switches.
            DialogKind::Switcher
            | DialogKind::Sessions
            | DialogKind::Models
            | DialogKind::Connections
            | DialogKind::UsageStats => DialogScope::Global,
            // Session: bound to the ambient session; unwound on session change.
            DialogKind::Telemetry
            | DialogKind::Asides
            | DialogKind::SessionTree
            | DialogKind::Tools
            | DialogKind::Mcp
            | DialogKind::Skills
            | DialogKind::Permissions
            | DialogKind::Queue => DialogScope::Session,
            // Scene: bound to one workspace capability.
            DialogKind::HistorySearch => DialogScope::Scene(SceneKind::Conversation),
        }
    }

    /// Whether this dialog may be opened or advertised in the current
    /// environment (`[INV-SURFACE-03]`).
    pub fn is_available(self, current_scene: SceneKind, has_session: bool) -> bool {
        match self.scope() {
            DialogScope::Global => true,
            DialogScope::Session => has_session,
            DialogScope::Scene(required) => current_scene == required && has_session,
        }
    }

    /// Retention policy for this dialog. Switchers are ephemeral; session-bound
    /// dialogs are retained per session; everything else is retained globally.
    pub fn retention_policy(self) -> RetentionPolicy {
        match self {
            DialogKind::Switcher => RetentionPolicy::Ephemeral,
            _ => match self.scope() {
                DialogScope::Session => RetentionPolicy::SessionScoped,
                _ => RetentionPolicy::Retained,
            },
        }
    }
}

/// Logical ownership domain of a dialog (`[INV-SURFACE-02]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DialogScope {
    /// Global: survives session and scene switches.
    Global,
    /// Bound to the ambient session.
    Session,
    /// Bound to one workspace scene.
    Scene(SceneKind),
}

/// Action-oriented, task-driven edge prompts and wizard steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SheetKind {
    Permission,
    Question,
    InputInjection,
    ModelEditor,
    ProviderPreset,
    OAuthPending,
    CustomProvider,
    ProviderDeleteConfirm,
}

impl SheetKind {
    pub fn retention_policy(self) -> RetentionPolicy {
        RetentionPolicy::Ephemeral
    }
}

/// Any surface floating above the active scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverlaySurface {
    Dialog(DialogKind),
    Sheet(SheetKind),
}

impl OverlaySurface {
    pub fn dialog(self) -> Option<DialogKind> {
        match self {
            Self::Dialog(d) => Some(d),
            Self::Sheet(_) => None,
        }
    }

    pub fn sheet(self) -> Option<SheetKind> {
        match self {
            Self::Sheet(s) => Some(s),
            Self::Dialog(_) => None,
        }
    }
}

/// One focused TUI surface: either the underlying root scene or an overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    Scene(SceneKind),
    Overlay(OverlaySurface),
}

impl Surface {
    pub fn scene(self) -> Option<SceneKind> {
        match self {
            Self::Scene(s) => Some(s),
            Self::Overlay(_) => None,
        }
    }

    pub fn overlay(self) -> Option<OverlaySurface> {
        match self {
            Self::Overlay(o) => Some(o),
            Self::Scene(_) => None,
        }
    }

    pub fn dialog(self) -> Option<DialogKind> {
        self.overlay().and_then(OverlaySurface::dialog)
    }

    pub fn sheet(self) -> Option<SheetKind> {
        self.overlay().and_then(OverlaySurface::sheet)
    }
}

/// One selectable row of the quick switcher: Scene or Dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwitcherTarget {
    Scene(SceneKind),
    Dialog(DialogKind),
}

impl SwitcherTarget {
    pub fn label(self) -> &'static str {
        match self {
            Self::Scene(s) => s.label(),
            Self::Dialog(d) => d.label(),
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Scene(s) => s.hint(),
            Self::Dialog(d) => d.hint(),
        }
    }
}

/// Orthogonal state retention policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RetentionPolicy {
    /// Preserved across dismissal.
    #[default]
    Retained,
    /// Discarded immediately upon dismissal or pop.
    Ephemeral,
    /// Scoped to the ambient session; cleared on session change.
    SessionScoped,
}

const SCENE_HISTORY_CAP: usize = 16;

/// Unified router managing the active root scene, the LIFO overlay stack, and
/// the dialog entity registry.
#[derive(Debug)]
pub struct SurfaceRouter {
    /// The active full-screen root workspace.
    scene: SceneKind,
    /// Stack of overlays currently floating over `scene` (bottom to top).
    /// The top of the stack has primary input focus.
    overlay_stack: Vec<OverlaySurface>,
    /// Bounded historical trace of scenes for explicit back-navigation.
    scene_history: Vec<SceneKind>,
    /// The single source of truth for every dialog entity's state.
    pub dialogs: Dialogs,
}

impl Default for SurfaceRouter {
    fn default() -> Self {
        Self {
            scene: SceneKind::Conversation,
            overlay_stack: Vec::new(),
            scene_history: Vec::new(),
            dialogs: Dialogs::default(),
        }
    }
}

impl SurfaceRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Boot directly into a dialog (e.g. startup sessions picker).
    pub fn with_dialog(id: DialogKind) -> Self {
        Self {
            scene: SceneKind::Conversation,
            overlay_stack: vec![OverlaySurface::Dialog(id)],
            scene_history: Vec::new(),
            dialogs: Dialogs::default(),
        }
    }

    /// Boot directly into a root scene (e.g. dashboard or settings).
    pub fn with_scene(scene: SceneKind) -> Self {
        Self {
            scene,
            overlay_stack: Vec::new(),
            scene_history: Vec::new(),
            dialogs: Dialogs::default(),
        }
    }

    /// The focused surface: top overlay if any, otherwise the active root scene.
    pub fn active_surface(&self) -> Surface {
        if let Some(top) = self.overlay_stack.last().copied() {
            Surface::Overlay(top)
        } else {
            Surface::Scene(self.scene)
        }
    }

    /// The full-screen root scene beneath any overlays.
    pub fn active_scene(&self) -> SceneKind {
        self.scene
    }

    /// The overlay stack (bottom to top).
    pub fn overlay_stack(&self) -> &[OverlaySurface] {
        &self.overlay_stack
    }

    /// The top overlay, if any.
    pub fn active_overlay(&self) -> Option<OverlaySurface> {
        self.overlay_stack.last().copied()
    }

    /// The active dialog, if the top overlay is a dialog.
    pub fn active_dialog(&self) -> Option<DialogKind> {
        self.active_overlay().and_then(OverlaySurface::dialog)
    }

    /// If an overlay is active, return the overlay immediately underneath the top overlay, if any.
    pub fn underlying_overlay(&self) -> Option<OverlaySurface> {
        if self.overlay_stack.len() >= 2 {
            self.overlay_stack
                .get(self.overlay_stack.len() - 2)
                .copied()
        } else {
            None
        }
    }

    /// If an overlay is active, return the dialog immediately underneath the top overlay, if any.
    pub fn underlying_dialog(&self) -> Option<DialogKind> {
        self.underlying_overlay().and_then(OverlaySurface::dialog)
    }

    /// The active sheet, if the top overlay is a sheet.
    pub fn active_sheet(&self) -> Option<SheetKind> {
        self.active_overlay().and_then(OverlaySurface::sheet)
    }

    /// The entity of the active dialog, if any.
    pub fn active_view(&self) -> Option<&dyn DialogView> {
        self.active_dialog().map(|d| self.dialogs.view(d))
    }

    /// The mutable entity of the active dialog, if any.
    pub fn active_view_mut(&mut self) -> Option<&mut dyn DialogView> {
        match self.active_dialog() {
            Some(d) => Some(self.dialogs.view_mut(d)),
            None => None,
        }
    }

    /// Resolve the whole overlay stack, running every dialog entity's
    /// `on_dismiss` hook in reverse LIFO order (`[INV-SURFACE-04]`).
    fn unwind_all(&mut self) {
        while let Some(overlay) = self.overlay_stack.pop() {
            if let OverlaySurface::Dialog(d) = overlay {
                self.dialogs.on_dismiss(d);
                if d.retention_policy() == RetentionPolicy::Ephemeral {
                    self.dialogs.reset(d);
                }
            }
        }
    }

    /// Unwind every dialog owned by `leaving_scene` in reverse LIFO order,
    /// preserving overlays of other domains (`[INV-SURFACE-04]`).
    pub fn unwind_scene(&mut self, leaving_scene: SceneKind) {
        let mut idx = self.overlay_stack.len();
        while idx > 0 {
            idx -= 1;
            let is_scene_bound = matches!(
                self.overlay_stack[idx],
                OverlaySurface::Dialog(d)
                    if d.scope() == DialogScope::Scene(leaving_scene)
            );
            if is_scene_bound {
                let removed = self.overlay_stack.remove(idx);
                if let OverlaySurface::Dialog(d) = removed {
                    self.dialogs.on_dismiss(d);
                }
            }
        }
    }

    /// Remove every overlay whose dialog is no longer available in the current
    /// environment (`[INV-SURFACE-03]`), running dismissal hooks.
    pub fn retain_available(&mut self, scene: SceneKind, has_session: bool) {
        let mut idx = self.overlay_stack.len();
        while idx > 0 {
            idx -= 1;
            let unavailable = matches!(
                self.overlay_stack[idx],
                OverlaySurface::Dialog(d) if !d.is_available(scene, has_session)
            );
            if unavailable {
                let removed = self.overlay_stack.remove(idx);
                if let OverlaySurface::Dialog(d) = removed {
                    self.dialogs.on_dismiss(d);
                }
            }
        }
    }

    /// Unwind every session-scoped dialog in reverse LIFO order
    /// (`[INV-SURFACE-05]`).
    pub fn unwind_session(&mut self) {
        let mut idx = self.overlay_stack.len();
        while idx > 0 {
            idx -= 1;
            let is_session_bound = matches!(
                self.overlay_stack[idx],
                OverlaySurface::Dialog(d) if d.scope() == DialogScope::Session
            );
            if is_session_bound {
                let removed = self.overlay_stack.remove(idx);
                if let OverlaySurface::Dialog(d) = removed {
                    self.dialogs.on_dismiss(d);
                }
            }
        }
    }

    /// Navigate to a root scene, unwinding the leaving scene's bound dialogs.
    pub fn switch_scene(&mut self, scene: SceneKind) {
        if scene != self.scene {
            if matches!(self.scene, SceneKind::TaskInspection | SceneKind::Aside) {
                self.scene_history.push(self.scene);
                if self.scene_history.len() > SCENE_HISTORY_CAP {
                    self.scene_history.remove(0);
                }
            }
            let leaving = self.scene;
            self.unwind_scene(leaving);
            // Sheet overlays carry no scene binding; a scene switch still
            // resets the transient stack, but dialogs of other domains stay.
            self.overlay_stack
                .retain(|o| matches!(o, OverlaySurface::Dialog(_)));
        }
        self.scene = scene;
    }

    /// Navigate back to the previous scene in history, or Conversation.
    pub fn back_scene(&mut self) -> SceneKind {
        let leaving = self.scene;
        let next = self.scene_history.pop().unwrap_or(SceneKind::Conversation);
        self.unwind_scene(leaving);
        self.overlay_stack
            .retain(|o| matches!(o, OverlaySurface::Dialog(_)));
        self.scene = next;
        next
    }

    /// Hard reset to Conversation home scene: unwind all overlays and history.
    pub fn reset_to_conversation(&mut self) {
        self.unwind_all();
        self.scene = SceneKind::Conversation;
        self.scene_history.clear();
    }

    /// Push an overlay onto the stack.
    ///
    /// There is deliberately **no** capacity truncation here: a bounded stack
    /// would have to evict out of LIFO order (the bottom entry), which
    /// `[INV-SURFACE-04]` forbids. Overlays are only ever removed by the
    /// structured `pop`/`unwind_*` pipelines, which run `on_dismiss` in reverse
    /// LIFO order. The stack is bounded in practice because opening a dialog
    /// deactivates the current one and sheets are transient.
    pub fn push_overlay(&mut self, overlay: OverlaySurface) {
        self.overlay_stack.push(overlay);
    }

    /// Present a dialog on top of the stack.
    pub fn present_dialog(&mut self, id: DialogKind) {
        self.push_overlay(OverlaySurface::Dialog(id));
    }

    /// Present a sheet on top of the stack.
    pub fn present_sheet(&mut self, sheet: SheetKind) {
        self.push_overlay(OverlaySurface::Sheet(sheet));
    }

    /// Replace the top overlay (or push if empty).
    pub fn replace_top_overlay(&mut self, overlay: OverlaySurface) {
        if self.overlay_stack.is_empty() {
            self.push_overlay(overlay);
        } else {
            let last_idx = self.overlay_stack.len() - 1;
            let removed = self.overlay_stack[last_idx];
            if let OverlaySurface::Dialog(d) = removed {
                self.dialogs.on_dismiss(d);
            }
            self.overlay_stack[last_idx] = overlay;
        }
    }

    /// Pop the top overlay, running its entity dismissal hook.
    pub fn pop_overlay(&mut self) -> Option<OverlaySurface> {
        let popped = self.overlay_stack.pop()?;
        if let OverlaySurface::Dialog(d) = popped {
            self.dialogs.on_dismiss(d);
            if d.retention_policy() == RetentionPolicy::Ephemeral {
                self.dialogs.reset(d);
            }
        }
        Some(popped)
    }

    /// Dismiss all overlays over the active scene, running every hook.
    pub fn dismiss_all_overlays(&mut self) {
        self.unwind_all();
    }

    /// Remove and return every sheet currently on the stack (top-to-bottom),
    /// so the caller can run the App-side teardown each sheet owns. Sheets are
    /// action prompts, not [`DialogView`] entities, so their `on_dismiss`
    /// equivalent lives on `App`.
    pub fn take_sheets(&mut self) -> Vec<SheetKind> {
        let mut sheets = Vec::new();
        let mut idx = self.overlay_stack.len();
        while idx > 0 {
            idx -= 1;
            if let OverlaySurface::Sheet(s) = self.overlay_stack[idx] {
                sheets.push(s);
                self.overlay_stack.remove(idx);
            }
        }
        sheets
    }

    /// Check if a specific dialog is anywhere in the overlay stack.
    pub fn contains_dialog(&self, id: DialogKind) -> bool {
        self.overlay_stack.contains(&OverlaySurface::Dialog(id))
    }

    /// Check if a specific sheet is anywhere in the overlay stack.
    pub fn contains_sheet(&self, sheet: SheetKind) -> bool {
        self.overlay_stack.contains(&OverlaySurface::Sheet(sheet))
    }
}

/// A MRU-ordered registry of opened dialogs. Dialog *state* is owned by the
/// [`Dialogs`] entity registry; this store only tracks open order for the
/// quick switcher and drives explicit forgetting.
#[derive(Debug, Default)]
pub struct SurfaceStore {
    /// Most-recent-first open order. Drives the quick switcher's MRU list.
    order: Vec<DialogKind>,
}

impl SurfaceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Focus a dialog: moves it to front of MRU order.
    pub fn open(&mut self, id: DialogKind) {
        self.order.retain(|&v| v != id);
        self.order.insert(0, id);
    }

    /// Explicit close: forget MRU order.
    pub fn close(&mut self, id: DialogKind) {
        self.order.retain(|&v| v != id);
    }

    /// Forget all MRU entries.
    pub fn close_all(&mut self) {
        self.order.clear();
    }

    /// Whether a dialog has an active entry in the store.
    pub fn is_open(&self, id: impl Into<DialogKind>) -> bool {
        self.order.contains(&id.into())
    }

    /// MRU order of opened dialogs.
    pub fn order(&self) -> &[DialogKind] {
        &self.order
    }

    /// Switcher rows: Dashboard & Settings first, then MRU dialogs, then
    /// unvisited discovery dialogs. Rows whose preconditions are unsatisfied
    /// are filtered out (`[INV-SURFACE-03]`).
    pub fn switcher_rows(&self, scene: SceneKind, has_session: bool) -> Vec<SwitcherTarget> {
        let mut rows: Vec<SwitcherTarget> = [SceneKind::Dashboard, SceneKind::Settings]
            .into_iter()
            .map(SwitcherTarget::Scene)
            .collect();
        for id in self.order.clone() {
            if id != DialogKind::Switcher && id.is_available(scene, has_session) {
                rows.push(SwitcherTarget::Dialog(id));
            }
        }
        for id in DialogKind::ALL {
            if !self.order.contains(&id)
                && id != DialogKind::Switcher
                && id.is_available(scene, has_session)
            {
                rows.push(SwitcherTarget::Dialog(id));
            }
        }
        rows
    }

    /// Filter switcher rows by fuzzy query.
    pub fn switcher_rows_filtered(
        &self,
        query: &str,
        scene: SceneKind,
        has_session: bool,
    ) -> Vec<SwitcherTarget> {
        let rows = self.switcher_rows(scene, has_session);
        if query.trim().is_empty() {
            return rows;
        }
        let q = query.trim();
        rows.into_iter()
            .filter(|target| {
                let label = target.label();
                let hint = target.hint();
                crate::fuzzy::fuzzy_match(label, q).is_some()
                    || crate::fuzzy::fuzzy_match(hint, q).is_some()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_matrix_assigns_exactly_one_domain_per_dialog() {
        // `[INV-SURFACE-02]`: the matrix is total and each dialog has exactly
        // one scope.
        assert_eq!(DialogKind::HistorySearch.scope(), DialogScope::Scene(SceneKind::Conversation));
        for id in [
            DialogKind::Switcher,
            DialogKind::Sessions,
            DialogKind::Models,
            DialogKind::Connections,
            DialogKind::UsageStats,
        ] {
            assert_eq!(id.scope(), DialogScope::Global, "{id:?}");
        }
        for id in [
            DialogKind::Telemetry,
            DialogKind::Asides,
            DialogKind::SessionTree,
            DialogKind::Tools,
            DialogKind::Mcp,
            DialogKind::Skills,
            DialogKind::Permissions,
            DialogKind::Queue,
        ] {
            assert_eq!(id.scope(), DialogScope::Session, "{id:?}");
        }
    }

    #[test]
    fn availability_gates_by_scope() {
        // `[INV-SURFACE-03]`.
        assert!(DialogKind::Models.is_available(SceneKind::Settings, false));
        assert!(!DialogKind::Tools.is_available(SceneKind::Conversation, false));
        assert!(DialogKind::Tools.is_available(SceneKind::Settings, true));
        assert!(DialogKind::HistorySearch.is_available(SceneKind::Conversation, true));
        assert!(!DialogKind::HistorySearch.is_available(SceneKind::Settings, true));
        assert!(!DialogKind::HistorySearch.is_available(SceneKind::Conversation, false));
    }

    #[test]
    fn retention_policies_follow_scope() {
        assert_eq!(DialogKind::Switcher.retention_policy(), RetentionPolicy::Ephemeral);
        assert_eq!(DialogKind::Models.retention_policy(), RetentionPolicy::Retained);
        assert_eq!(
            DialogKind::Telemetry.retention_policy(),
            RetentionPolicy::SessionScoped
        );
    }

    #[test]
    fn switch_scene_unwinds_scene_bound_dialogs_and_preserves_global() {
        // `[INV-SURFACE-04]` + `[INV-SURFACE-05]`: no blunt `clear()` — a
        // scene switch unwinds only the leaving scene's bound dialogs.
        let mut router = SurfaceRouter::new();
        router.present_dialog(DialogKind::Models);
        router.present_dialog(DialogKind::HistorySearch);
        assert!(router.contains_dialog(DialogKind::HistorySearch));

        router.switch_scene(SceneKind::Settings);
        assert!(
            !router.contains_dialog(DialogKind::HistorySearch),
            "scene-bound dialog unwound on scene exit"
        );
        assert!(
            router.contains_dialog(DialogKind::Models),
            "global dialog survives a scene switch"
        );
    }

    #[test]
    fn unwind_session_removes_session_dialogs_and_preserves_global() {
        let mut router = SurfaceRouter::new();
        router.present_dialog(DialogKind::Models);
        router.present_dialog(DialogKind::Telemetry);
        router.unwind_session();
        assert!(!router.contains_dialog(DialogKind::Telemetry));
        assert!(router.contains_dialog(DialogKind::Models));
    }

    #[test]
    fn ephemeral_switcher_resets_on_pop() {
        let mut router = SurfaceRouter::new();
        router.present_dialog(DialogKind::Switcher);
        router.dialogs.switcher.selected = 5;
        router.dialogs.switcher.query.text = "abc".to_string();
        router.pop_overlay();
        assert_eq!(router.dialogs.switcher.selected, 0);
        assert!(router.dialogs.switcher.query.is_empty());
    }

    #[test]
    fn pop_runs_dismissal_hook_clearing_embedded_search() {
        let mut router = SurfaceRouter::new();
        router.present_dialog(DialogKind::Models);
        router.dialogs.models.search = true;
        router.dialogs.models.query.text = "gpt".to_string();
        router.pop_overlay();
        assert!(!router.dialogs.models.search);
        assert!(router.dialogs.models.query.is_empty());
    }

    #[test]
    fn take_sheets_drains_sheet_overlays_preserving_dialogs() {
        let mut router = SurfaceRouter::new();
        router.present_dialog(DialogKind::Models);
        router.present_sheet(SheetKind::CustomProvider);
        let sheets = router.take_sheets();
        assert_eq!(sheets, vec![SheetKind::CustomProvider]);
        assert!(!router.contains_sheet(SheetKind::CustomProvider));
        assert!(router.contains_dialog(DialogKind::Models));
    }

    #[test]
    fn dialog_view_contract_reports_identity_scope_and_geometry() {
        // The `DialogView` contract (ADR-0035 §1) exposes identity, domain,
        // and the modal geometry.
        let mut d = Dialogs::default();
        let tools = d.view_mut(DialogKind::Tools);
        assert_eq!(tools.kind(), DialogKind::Tools);
        assert_eq!(tools.scope(), DialogScope::Session);
        assert!(tools.layout_spec().width_percent > 0);
        assert_eq!(
            d.view_mut(DialogKind::HistorySearch).scope(),
            DialogScope::Scene(SceneKind::Conversation)
        );
    }

    #[test]
    fn session_switch_archives_and_reinstates_session_scoped_state() {
        // `[INV-SURFACE-05]` §5: session-scoped state is archived per session
        // and reinstated on return; global state is never touched.
        let mut d = Dialogs::default();
        d.switch_session("a");
        d.telemetry.scroll = 11;
        d.models.scroll = 3;
        d.switch_session("b");
        assert_eq!(d.telemetry.scroll, 0, "fresh session starts clean");
        assert_eq!(d.models.scroll, 3, "global state untouched");
        d.telemetry.scroll = 22;
        d.switch_session("a");
        assert_eq!(d.telemetry.scroll, 11, "session A reinstated");
        d.switch_session("b");
        assert_eq!(d.telemetry.scroll, 22, "session B reinstated");
    }

    #[test]
    fn dialog_entities_are_independent() {
        let mut d = Dialogs::default();
        d.tools.scroll = 7;
        d.mcp.scroll = 9;
        assert_eq!(d.tools.scroll, 7);
        assert_eq!(d.mcp.scroll, 9);
        d.reset(DialogKind::Tools);
        assert_eq!(d.tools.scroll, 0);
        assert_eq!(d.mcp.scroll, 9, "no aliasing between entities");
    }
}
