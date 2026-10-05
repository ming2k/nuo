//! Application component composition. Structural behavior belongs to the
//! engine runtime; this module supplies typed identities and surface policy.

use nuotc::Rect;
use nuotc::ui::{Component, InputPolicy, LayoutBox, PointerPolicy, Scene, UiRuntime};

use crate::model::layout::{LayoutMap, PermissionActionHit, QuestionOptionHit};
use crate::sheet::SheetKind;
use crate::surfaces::{DialogKind, OverlaySurface, SheetKind as SurfaceSheetKind};

/// Keyboard event families (ADR-0197 §D2). Scene components claim the
/// families they own at mount time; keyboard routing resolves the foreground
/// per family, so an unclaimed family falls through to the components below.
pub mod family {
    /// Composer text editing: printable keys, caret motion, readline chords.
    pub const COMPOSER: u64 = 1 << 0;
    /// Transcript scrolling and step navigation.
    pub const TRANSCRIPT: u64 = 1 << 1;
    /// Sheet decision keys (ADR-0173 §3).
    pub const SHEET: u64 = 1 << 2;
    /// Completion-menu cycling and auto-accept.
    pub const COMPLETION: u64 = 1 << 3;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UiKey {
    Root,
    Transcript,
    Footer,
    Composer,
    Queue,
    Activity,
    ModelBar,
    Context,
    Performance,
    Connection,
    Sticky,
    Backdrop,
    Sheet(SheetKind),
    QuestionOption(usize),
    PermissionAction(usize),
    Completion,
    CompletionItem(usize),
    Overlay(OverlaySurface),
    ConfigDropdown,
    ProviderDelete,
    OauthUrl,
    OauthCode,
    Toast,
    PreAttach,
    SettingsOption(usize),
    /// A visible (non-cursor) Settings detail row that exists only as a pointer
    /// hover target so the row under the mouse can light up.
    SettingsRow(usize),
}

/// The application has one mounted UI runtime. Semantic text mappings travel
/// with its frame transaction but remain document data, not UI hit targets.
pub struct ComponentTree {
    pub runtime: UiRuntime<UiKey>,
    pub document: LayoutMap,
    pending_document: Option<LayoutMap>,
}

impl Default for ComponentTree {
    fn default() -> Self {
        Self {
            runtime: UiRuntime::default(),
            document: LayoutMap::new(),
            pending_document: None,
        }
    }
}

impl ComponentTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens a frame. Which components mount is decided by the render phase
    /// below; the scene — not an app-side modal flag — is the sole source of
    /// keyboard modality (ADR-0197 §D2).
    pub fn begin(&mut self, viewport: Rect) {
        self.pending_document = None;
        self.runtime.begin(viewport);
        self.mount(UiKey::Root, viewport);
        self.mount(UiKey::Transcript, viewport);
        self.mount(UiKey::Footer, viewport);
        self.mount(UiKey::Composer, Rect::default());
    }

    pub fn stage_document(&mut self, document: LayoutMap) {
        self.pending_document = Some(document);
    }

    pub fn commit(&mut self) {
        if let Err(error) = self.runtime.commit() {
            panic!("invalid application component frame: {error}");
        }
        if let Some(document) = self.pending_document.take() {
            self.document = document;
        }
    }

    pub fn scene(&self) -> &Scene<UiKey> {
        self.runtime.presented()
    }

    /// Resolve the committed keyboard route for one event family.
    ///
    /// Returning owned keys keeps the scene borrow out of component handlers,
    /// which may mutate application state after routing has completed.
    pub fn keyboard_path_for(&self, family: u64) -> Vec<UiKey> {
        self.scene()
            .keyboard_path_for(self.runtime.focused(), family)
            .into_iter()
            .copied()
            .collect()
    }

    pub fn bounds(&self, key: UiKey) -> Option<Rect> {
        self.scene().rect(&key).filter(|rect| rect.area() > 0)
    }

    pub fn contains(&self, key: UiKey, x: u16, y: u16) -> bool {
        self.scene()
            .layout(&key)
            .is_some_and(|layout| layout.clip.contains(x, y))
    }

    pub fn target(&self, x: u16, y: u16) -> Option<UiKey> {
        self.runtime.pointer_target(x, y).copied()
    }

    pub fn modal_bounds(&self) -> Option<Rect> {
        self.scene()
            .paint_order()
            .rev()
            .find_map(|(key, layout)| matches!(key, UiKey::Overlay(_)).then_some(layout.bounds))
    }

    pub fn mount(&mut self, key: UiKey, rect: Rect) {
        let (parent, layer, pointer, input, focusable) = match key {
            UiKey::Root => (
                None,
                0,
                PointerPolicy::Transparent,
                InputPolicy::None,
                false,
            ),
            UiKey::Transcript => (
                Some(UiKey::Root),
                1,
                PointerPolicy::Target,
                InputPolicy::Scope(family::TRANSCRIPT),
                true,
            ),
            UiKey::Footer => (
                Some(UiKey::Root),
                2,
                PointerPolicy::Transparent,
                InputPolicy::None,
                false,
            ),
            UiKey::Composer => (
                Some(UiKey::Footer),
                3,
                PointerPolicy::Target,
                InputPolicy::Scope(family::COMPOSER),
                true,
            ),
            UiKey::Queue | UiKey::Activity | UiKey::ModelBar => (
                Some(UiKey::Footer),
                3,
                PointerPolicy::Target,
                InputPolicy::None,
                false,
            ),
            UiKey::Context | UiKey::Performance | UiKey::Connection => (
                Some(UiKey::Footer),
                4,
                PointerPolicy::Target,
                InputPolicy::None,
                false,
            ),
            UiKey::Sticky => (
                Some(UiKey::Transcript),
                5,
                PointerPolicy::Target,
                InputPolicy::None,
                false,
            ),
            UiKey::Backdrop => (
                Some(UiKey::Root),
                25,
                PointerPolicy::Transparent,
                InputPolicy::None,
                false,
            ),
            UiKey::Sheet(kind) => (
                Some(UiKey::Footer),
                10,
                PointerPolicy::Target,
                if matches!(kind, SheetKind::Permission) {
                    // The pass-through sheet (ADR-0173 §2): it claims its own
                    // decision keys and the composer slot it occupies, but not
                    // the transcript family, so scroll/navigation below it
                    // stays live.
                    InputPolicy::Scope(family::SHEET | family::COMPOSER | family::COMPLETION)
                } else {
                    // Exclusive sheet: owns every keyboard family while up.
                    InputPolicy::Scope(u64::MAX)
                },
                true,
            ),
            UiKey::QuestionOption(_) => (
                Some(UiKey::Sheet(SheetKind::Question)),
                11,
                PointerPolicy::Target,
                InputPolicy::Bubble,
                true,
            ),
            UiKey::PermissionAction(_) => (
                Some(UiKey::Sheet(SheetKind::Permission)),
                11,
                PointerPolicy::Target,
                InputPolicy::Bubble,
                true,
            ),
            UiKey::Completion => (
                Some(UiKey::Composer),
                20,
                PointerPolicy::Target,
                InputPolicy::Scope(family::COMPLETION),
                false,
            ),
            UiKey::CompletionItem(_) => (
                Some(UiKey::Completion),
                21,
                PointerPolicy::Target,
                InputPolicy::Bubble,
                false,
            ),
            UiKey::Overlay(_) => (
                Some(UiKey::Root),
                30,
                PointerPolicy::Barrier,
                InputPolicy::Modal,
                true,
            ),
            UiKey::ConfigDropdown => (
                Some(UiKey::Root),
                45,
                PointerPolicy::Barrier,
                InputPolicy::Modal,
                true,
            ),
            UiKey::OauthUrl | UiKey::OauthCode => (
                Some(UiKey::Overlay(OverlaySurface::Sheet(
                    SurfaceSheetKind::OAuthPending,
                ))),
                31,
                PointerPolicy::Target,
                InputPolicy::None,
                false,
            ),
            UiKey::ProviderDelete => (
                Some(UiKey::Overlay(OverlaySurface::Dialog(
                    DialogKind::Connections,
                ))),
                40,
                PointerPolicy::Barrier,
                InputPolicy::Modal,
                true,
            ),
            UiKey::Toast => (
                Some(UiKey::Root),
                50,
                PointerPolicy::Transparent,
                InputPolicy::None,
                false,
            ),
            UiKey::PreAttach => (
                Some(UiKey::Root),
                60,
                PointerPolicy::Barrier,
                InputPolicy::Modal,
                true,
            ),
            UiKey::SettingsOption(_) | UiKey::SettingsRow(_) => (
                Some(UiKey::Root),
                31,
                PointerPolicy::Target,
                InputPolicy::Bubble,
                true,
            ),
        };
        // Popups and sheets retain logical ownership while escaping the
        // composer's narrow clip. All other children inherit clipping.
        let layout = if matches!(
            key,
            UiKey::Sheet(_) | UiKey::Completion | UiKey::ProviderDelete | UiKey::ConfigDropdown
        ) {
            LayoutBox::Viewport(rect)
        } else {
            LayoutBox::Placed(rect)
        };
        let result = if self
            .runtime
            .pending()
            .is_ok_and(|scene| scene.id(&key).is_some())
        {
            self.runtime.place(&key, layout)
        } else {
            let mut component = Component::new(key, parent, layout).layer(layer);
            component.pointer = pointer;
            component.input = input;
            component.focusable = focusable;
            self.runtime.mount(component)
        };
        if let Err(error) = result {
            panic!("invalid component {key:?}: {error}");
        }
    }

    pub fn mount_completion(&mut self, rect: Rect) {
        self.mount(UiKey::Completion, rect);
    }

    pub fn paint<R>(
        &self,
        frame: &mut nuotc::Frame<'_>,
        key: UiKey,
        painter: impl FnOnce(&mut nuotc::Frame<'_>) -> R,
    ) -> R {
        self.runtime
            .paint_node(&key, frame, painter)
            .unwrap_or_else(|error| panic!("cannot paint {key:?}: {error}"))
    }
    pub fn mount_completion_item(&mut self, index: usize, rect: Rect) {
        self.mount(UiKey::CompletionItem(index), rect);
    }
    pub fn mount_question_option(&mut self, hit: QuestionOptionHit) {
        self.mount(UiKey::QuestionOption(hit.option_index), hit.rect);
    }
    pub fn mount_permission_action(&mut self, hit: PermissionActionHit) {
        self.mount(UiKey::PermissionAction(hit.action_index), hit.rect);
    }
    pub fn mount_permission_sheet(&mut self, rect: Rect) {
        self.mount(UiKey::Sheet(SheetKind::Permission), rect);
    }
    pub fn mount_oauth_url(&mut self, rect: Rect) {
        self.mount(UiKey::OauthUrl, rect);
    }
    pub fn mount_oauth_code(&mut self, rect: Rect) {
        self.mount(UiKey::OauthCode, rect);
    }
    pub fn mount_oauth_modal(&mut self, rect: Rect) {
        self.mount(
            UiKey::Overlay(OverlaySurface::Sheet(SurfaceSheetKind::OAuthPending)),
            rect,
        );
    }

    pub fn completion_item_at(&self, x: u16, y: u16) -> Option<usize> {
        match self.target(x, y) {
            Some(UiKey::CompletionItem(i)) => Some(i),
            _ => None,
        }
    }
    pub fn completion_menu_contains(&self, x: u16, y: u16) -> bool {
        self.contains(UiKey::Completion, x, y)
    }
    pub fn permission_sheet_contains(&self, x: u16, y: u16) -> bool {
        self.contains(UiKey::Sheet(SheetKind::Permission), x, y)
    }
    pub fn oauth_modal_contains(&self, x: u16, y: u16) -> bool {
        self.contains(
            UiKey::Overlay(OverlaySurface::Sheet(SurfaceSheetKind::OAuthPending)),
            x,
            y,
        )
    }
    pub fn question_option_at(&self, x: u16, y: u16) -> Option<QuestionOptionHit> {
        match self.target(x, y) {
            Some(UiKey::QuestionOption(option_index)) => self
                .bounds(UiKey::QuestionOption(option_index))
                .map(|rect| QuestionOptionHit { option_index, rect }),
            _ => None,
        }
    }
    pub fn permission_action_at(&self, x: u16, y: u16) -> Option<PermissionActionHit> {
        match self.target(x, y) {
            Some(UiKey::PermissionAction(action_index)) => self
                .bounds(UiKey::PermissionAction(action_index))
                .map(|rect| PermissionActionHit { action_index, rect }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_sheet_routes_decisions_to_sheet_and_scroll_to_transcript() {
        let mut ui = ComponentTree::new();
        let viewport = Rect::new(0, 0, 80, 24);
        ui.begin(viewport);
        ui.mount(UiKey::Sheet(SheetKind::Permission), viewport);
        ui.commit();

        assert_eq!(
            ui.keyboard_path_for(family::SHEET),
            vec![UiKey::Sheet(SheetKind::Permission)]
        );
        assert_eq!(
            ui.keyboard_path_for(family::COMPOSER),
            vec![UiKey::Sheet(SheetKind::Permission)]
        );
        assert_eq!(
            ui.keyboard_path_for(family::TRANSCRIPT),
            vec![UiKey::Transcript]
        );
    }

    #[test]
    fn exclusive_component_terminates_every_keyboard_route() {
        let mut ui = ComponentTree::new();
        let viewport = Rect::new(0, 0, 80, 24);
        ui.begin(viewport);
        ui.mount(UiKey::ConfigDropdown, viewport);
        ui.commit();

        for family in [
            family::SHEET,
            family::COMPOSER,
            family::TRANSCRIPT,
            family::COMPLETION,
        ] {
            assert_eq!(ui.keyboard_path_for(family), vec![UiKey::ConfigDropdown]);
        }
    }
}
