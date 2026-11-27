//! The one place the palette reads the application (RFC-0005 `[INV-PAL-CTX-01]`).
//!
//! Everything downstream (sources, ranking, view, execution) consumes a
//! [`PaletteContext`] and never `App`, so the projection is a pure function of
//! this value and the query. The context describes the state *under* the
//! palette: the palette's own overlay is never part of what it reasons about.

use crate::shell::App;
use crate::shell::focus::FocusPlane;
use crate::shell::keymap::{AppContext, GlobalOverrides, SurfaceOverrides};
use crate::shell::surfaces::{DialogKind, OverlaySurface, SceneKind, TabKind};

/// One open client tab, as the palette names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TabRef {
    /// 0-based ordinal, the payload of `InputAction::SelectTab`.
    pub index: usize,
    pub label: String,
    /// Stable identity for the entry key (`tab:thread:<id>`, `tab:dashboard`).
    pub key: String,
    pub active: bool,
}

/// A narrow, owned snapshot of everything the palette may consult.
#[derive(Debug, Clone, Default)]
pub(crate) struct PaletteContext {
    /// The predicate input shared with the command registry. Its
    /// `has_overlay` / `active_dialog` describe the stack *beneath* the palette.
    pub state: AppContext,
    /// Which chat plane owns the keyboard (ADR-0145).
    pub focus: FocusPlane,
    /// Whether the Settings scene's detail pane owns focus (segment verbs).
    pub settings_detail: bool,
    /// Active view inside `DialogKind::SessionTrace` (ADR-0263 `[INV-TRACE-02]`).
    pub trace_view: crate::shell::surfaces::TraceView,
    pub tabs: Vec<TabRef>,
    /// Effective chords, so every keycap shown is the one that fires.
    pub global_keys: GlobalOverrides,
    pub surface_keys: SurfaceOverrides,
}

impl PaletteContext {
    /// Capture the context from the application. This is the only function in
    /// the palette that reads `App`.
    pub(crate) fn capture(app: &App) -> Self {
        let beneath = app
            .surfaces
            .surfaces
            .overlay_stack()
            .iter()
            .map(|entry| entry.overlay)
            .rfind(|overlay| *overlay != OverlaySurface::Dialog(DialogKind::Palette));
        let responding = app.viewed_chrome().responding;
        let router = &app.surfaces.surfaces;
        let active_tab = router.active_tab_index();
        let projected = crate::shell::event_loop::render::project_tab_items(
            app,
            &app.session.current_session_id,
        );
        let tabs = router
            .tabs()
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let (label, key) = match &tab.kind {
                    TabKind::Thread(id) => (
                        projected
                            .get(index)
                            .map(|item| item.title.to_string())
                            .unwrap_or_else(|| "new thread".to_string()),
                        format!("tab:thread:{id}"),
                    ),
                    TabKind::Dashboard => ("Dashboard".to_string(), "tab:dashboard".to_string()),
                    TabKind::Settings => ("Settings".to_string(), "tab:settings".to_string()),
                };
                TabRef {
                    index,
                    label,
                    key,
                    active: index == active_tab,
                }
            })
            .collect();
        Self {
            state: AppContext {
                has_overlay: beneath.is_some(),
                active_dialog: beneath.and_then(OverlaySurface::dialog),
                is_responding: responding,
                has_selection: !matches!(
                    app.threads.selection,
                    nuo_view::model::selection::SelectionState::None
                ),
                has_focused_entry: app.threads.focus.entry().is_some(),
                has_notices: app.has_notices(),
                has_running_task: responding,
                queue_count: app.outbox_total(),
                has_session: app.has_session(),
                scene: app.current_scene(),
            },
            focus: app.threads.focus.plane(),
            settings_detail: app.surfaces.config_focus
                == crate::shell::overlays::ConfigFocus::Detail,
            trace_view: app
                .surfaces
                .surfaces
                .dlg::<crate::shell::surfaces::SessionTraceDialog>()
                .view
                .clone(),
            tabs,
            global_keys: app.surfaces.key_overrides.clone(),
            surface_keys: app.surfaces.surface_overrides.clone(),
        }
    }

    pub(crate) fn scene(&self) -> SceneKind {
        self.state.scene
    }

    /// The dialog the palette was opened over, if any.
    pub(crate) fn dialog(&self) -> Option<DialogKind> {
        self.state.active_dialog
    }

    /// Where the user stands, for the palette's title (`Thread › Models`).
    pub(crate) fn here(&self) -> String {
        match self.dialog() {
            Some(dialog) => format!("{} › {}", self.scene().label(), dialog.label()),
            None => self.scene().label().to_string(),
        }
    }
}
