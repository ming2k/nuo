//! TUI surface routing: Stage-Scene-Overlay architecture (ADR-0205).
//!
//! - A [`SceneKind`] is an **independent full-screen workspace** (`Conversation`,
//!   `Dashboard`, `Settings`, `TaskInspection`, `Aside`). Exactly one Scene is
//!   active at any given moment.
//! - A [`DialogKind`] names a **centered floating dialog** (Tools, Mcp,
//!   Models, Connections, etc.) that floats over whatever Scene is active.
//! - A [`SheetKind`] names an **edge-anchored action prompt** (Permission,
//!   Question, InputInjection, ModelEditor, etc.).
//! - An [`OverlaySurface`] is either a `Dialog` or a `Sheet`.
//! - The [`SurfaceRouter`] is the single authority managing the active Scene and
//!   the LIFO `overlay_stack: Vec<OverlaySurface>`.
//! - [`SurfaceStore`] manages retained states (`DialogState`) with an explicit
//!   [`RetentionPolicy`].

#![allow(dead_code)]

use std::collections::HashMap;

/// Root full-screen scene identifier (ADR-0205: closed set of destinations).
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

/// Centered, reference and management dialogs (ADR-0205).
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
            DialogKind::Switcher => "Ctrl-l",
        }
    }

    /// Default retention policy for this dialog.
    pub fn retention_policy(self) -> RetentionPolicy {
        match self {
            DialogKind::Switcher => RetentionPolicy::Ephemeral,
            _ => RetentionPolicy::Retained,
        }
    }
}

/// Action-oriented, task-driven edge prompts and wizard steps (ADR-0205).
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

/// Orthogonal state retention policies (ADR-0205).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RetentionPolicy {
    /// Preserved in `SurfaceStore` across dismissal (scroll offset, search filter, cursor).
    #[default]
    Retained,
    /// Discarded immediately upon dismissal or pop.
    Ephemeral,
    /// Scoped to the active session id; purged on session switch.
    SessionScoped,
}

const OVERLAY_STACK_CAP: usize = 16;
const SCENE_HISTORY_CAP: usize = 16;

/// Unified router managing the active root scene and the LIFO overlay stack.
#[derive(Debug)]
pub struct SurfaceRouter {
    /// The active full-screen root workspace.
    scene: SceneKind,
    /// Stack of overlays currently floating over `scene` (bottom to top).
    /// The top of the stack has primary input focus.
    overlay_stack: Vec<OverlaySurface>,
    /// Bounded historical trace of scenes for explicit back-navigation.
    scene_history: Vec<SceneKind>,
}

impl Default for SurfaceRouter {
    fn default() -> Self {
        Self {
            scene: SceneKind::Conversation,
            overlay_stack: Vec::new(),
            scene_history: Vec::new(),
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
        }
    }

    /// Boot directly into a root scene (e.g. dashboard or settings).
    pub fn with_scene(scene: SceneKind) -> Self {
        Self {
            scene,
            overlay_stack: Vec::new(),
            scene_history: Vec::new(),
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

    /// Navigate to a root scene, pushing current scene into history if scoped.
    pub fn switch_scene(&mut self, scene: SceneKind) {
        if scene != self.scene && matches!(self.scene, SceneKind::TaskInspection | SceneKind::Aside)
        {
            self.scene_history.push(self.scene);
            if self.scene_history.len() > SCENE_HISTORY_CAP {
                self.scene_history.remove(0);
            }
        }
        self.scene = scene;
        self.overlay_stack.clear();
    }

    /// Navigate back to the previous scene in history, or Conversation.
    pub fn back_scene(&mut self) -> SceneKind {
        self.scene = self.scene_history.pop().unwrap_or(SceneKind::Conversation);
        self.overlay_stack.clear();
        self.scene
    }

    /// Hard reset to Conversation home scene, clearing all overlays and history.
    pub fn reset_to_conversation(&mut self) {
        self.scene = SceneKind::Conversation;
        self.overlay_stack.clear();
        self.scene_history.clear();
    }

    /// Push an overlay onto the stack.
    pub fn push_overlay(&mut self, overlay: OverlaySurface) {
        self.overlay_stack.push(overlay);
        if self.overlay_stack.len() > OVERLAY_STACK_CAP {
            self.overlay_stack.remove(0);
        }
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
            self.overlay_stack[last_idx] = overlay;
        }
    }

    /// Pop the top overlay, returning it.
    pub fn pop_overlay(&mut self) -> Option<OverlaySurface> {
        self.overlay_stack.pop()
    }

    /// Dismiss all overlays over the active scene.
    pub fn dismiss_all_overlays(&mut self) {
        self.overlay_stack.clear();
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

/// The retained state of one dialog (ADR-0205).
#[derive(Debug, Clone)]
pub struct DialogState {
    /// Selection cursor (`modal_index`).
    pub index: usize,
    /// Body scroll offset.
    pub scroll: usize,
    /// Whether body scroll follows the selection.
    pub follow: bool,
    /// Composer draft parked by this dialog when it borrowed the input line.
    pub draft: Option<String>,
    /// Search or filter query owned by this dialog.
    pub query: String,
    /// Whether the query field is actively focused.
    pub query_active: bool,
}

impl Default for DialogState {
    fn default() -> Self {
        Self {
            index: 0,
            scroll: 0,
            follow: true,
            draft: None,
            query: String::new(),
            query_active: false,
        }
    }
}

/// A MRU-ordered registry of retained dialog states (ADR-0205).
#[derive(Debug, Default)]
pub struct SurfaceStore {
    /// Most-recent-first open order. Drives the quick switcher's MRU list.
    order: Vec<DialogKind>,
    /// Retained per-dialog state.
    states: HashMap<DialogKind, DialogState>,
}

impl SurfaceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Focus a dialog: moves it to front of MRU order, initializing state on first open.
    pub fn open(&mut self, id: DialogKind) -> Option<DialogState> {
        self.order.retain(|&v| v != id);
        self.order.insert(0, id);
        match self.states.entry(id) {
            std::collections::hash_map::Entry::Occupied(e) => Some(e.get().clone()),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(DialogState::default());
                None
            }
        }
    }

    /// Record the dialog's state upon loss of focus.
    pub fn save(&mut self, id: DialogKind, state: DialogState) {
        self.states.insert(id, state);
    }

    /// Explicit close: forget state and MRU order.
    pub fn close(&mut self, id: DialogKind) {
        self.order.retain(|&v| v != id);
        self.states.remove(&id);
    }

    /// Forget all retained states (e.g. on session switch).
    pub fn close_all(&mut self) {
        self.order.clear();
        self.states.clear();
    }

    /// Whether a dialog has an active initialized buffer in the store.
    pub fn is_open(&self, id: impl Into<DialogKind>) -> bool {
        let d = id.into();
        self.order.contains(&d)
    }

    /// Retained state for a dialog, if it has been opened.
    pub fn state(&self, id: &DialogKind) -> Option<&DialogState> {
        self.states.get(id)
    }

    /// Mutable state for a dialog.
    pub fn state_mut(&mut self, id: &DialogKind) -> Option<&mut DialogState> {
        self.states.get_mut(id)
    }

    /// MRU order of opened dialogs.
    pub fn order(&self) -> &[DialogKind] {
        &self.order
    }

    /// Switcher rows: Dashboard & Settings first, then MRU dialogs, then unvisited discovery dialogs.
    pub fn switcher_rows(&self) -> Vec<SwitcherTarget> {
        let mut rows: Vec<SwitcherTarget> = [SceneKind::Dashboard, SceneKind::Settings]
            .into_iter()
            .map(SwitcherTarget::Scene)
            .collect();
        for id in self.order.clone() {
            if id != DialogKind::Switcher {
                rows.push(SwitcherTarget::Dialog(id));
            }
        }
        for id in DialogKind::ALL {
            if !self.order.contains(&id) && id != DialogKind::Switcher {
                rows.push(SwitcherTarget::Dialog(id));
            }
        }
        rows
    }

    /// Filter switcher rows by fuzzy query.
    pub fn switcher_rows_filtered(&self, query: &str) -> Vec<SwitcherTarget> {
        let rows = self.switcher_rows();
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
    fn first_open_initialises_once_and_returns_none() {
        let mut store = SurfaceStore::new();
        assert!(
            store.open(DialogKind::UsageStats).is_none(),
            "first open has no state"
        );
        store.save(
            DialogKind::UsageStats,
            DialogState {
                index: 3,
                scroll: 12,
                follow: false,
                draft: None,
                query: String::new(),
                query_active: false,
            },
        );
        let restored = store.open(DialogKind::UsageStats).expect("state retained");
        assert_eq!(
            (restored.index, restored.scroll, restored.follow),
            (3, 12, false)
        );
    }

    #[test]
    fn close_forgets_state() {
        let mut store = SurfaceStore::new();
        store.open(DialogKind::UsageStats);
        store.close(DialogKind::UsageStats);
        assert!(
            store.open(DialogKind::UsageStats).is_none(),
            "close forgets"
        );
    }

    #[test]
    fn mru_order_tracks_focusing() {
        let mut store = SurfaceStore::new();
        store.open(DialogKind::UsageStats);
        store.open(DialogKind::Tools);
        store.open(DialogKind::UsageStats);
        assert_eq!(store.order(), &[DialogKind::UsageStats, DialogKind::Tools]);
    }

    #[test]
    fn switcher_rows_scenes_first_then_dialogs() {
        let mut store = SurfaceStore::new();
        store.open(DialogKind::Skills);
        store.open(DialogKind::Asides);
        let rows = store.switcher_rows();
        assert_eq!(
            &rows[..2],
            &[
                SwitcherTarget::Scene(SceneKind::Dashboard),
                SwitcherTarget::Scene(SceneKind::Settings)
            ],
            "full-screen scenes first"
        );
        assert_eq!(
            &rows[2..4],
            &[
                SwitcherTarget::Dialog(DialogKind::Asides),
                SwitcherTarget::Dialog(DialogKind::Skills)
            ],
            "MRU dialogs next"
        );
    }

    #[test]
    fn overlay_stack_lifo_order() {
        let mut router = SurfaceRouter::new();
        assert_eq!(
            router.active_surface(),
            Surface::Scene(SceneKind::Conversation)
        );
        assert_eq!(router.active_scene(), SceneKind::Conversation);
        assert!(router.active_overlay().is_none());

        // Present a dialog
        router.present_dialog(DialogKind::Models);
        assert_eq!(router.active_dialog(), Some(DialogKind::Models));

        // Present an action sheet on top of the dialog
        router.present_sheet(SheetKind::Permission);
        assert_eq!(router.active_sheet(), Some(SheetKind::Permission));
        assert_eq!(router.overlay_stack().len(), 2);

        // Pop sheet restores the dialog
        let popped = router.pop_overlay();
        assert_eq!(popped, Some(OverlaySurface::Sheet(SheetKind::Permission)));
        assert_eq!(router.active_dialog(), Some(DialogKind::Models));

        // Pop dialog restores the scene
        let popped_dialog = router.pop_overlay();
        assert_eq!(
            popped_dialog,
            Some(OverlaySurface::Dialog(DialogKind::Models))
        );
        assert_eq!(
            router.active_surface(),
            Surface::Scene(SceneKind::Conversation)
        );
    }

    #[test]
    fn scene_switching_and_back() {
        let mut router = SurfaceRouter::new();
        router.switch_scene(SceneKind::TaskInspection);
        assert_eq!(router.active_scene(), SceneKind::TaskInspection);

        router.switch_scene(SceneKind::Dashboard);
        assert_eq!(router.active_scene(), SceneKind::Dashboard);

        assert_eq!(router.back_scene(), SceneKind::TaskInspection);
        assert_eq!(router.back_scene(), SceneKind::Conversation);
    }
}
