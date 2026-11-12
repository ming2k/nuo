//! Encapsulated dialog entities (ADR-0035, `[INV-SURFACE-01]`).
//!
//! Every floating dialog owns its own presentation state — selection cursor,
//! body scroll, follow mode, embedded text input, and sub-layer flags. No
//! dialog state is shared between dialogs or aliased onto `App` scratchpad
//! fields, and no dialog borrows the thread composer line.
//!
//! The [`DialogView`] contract binds each entity to a [`DialogKind`] and its
//! [`DialogScope`] domain; the [`Dialogs`] registry is owned by the surface
//! router and is the single source of truth for dialog state. Server-fed model
//! data (provider snapshots, session context, reports) stays on `App` as
//! read-only model, never as dialog scratchpad.

#![allow(dead_code)]

use std::any::Any;
use std::collections::HashSet;

use nuotc::{Frame, Rect};

use crate::shell::input::InputAction;
use crate::shell::render::Theme;
use crate::shell::surfaces::{DialogKind, DialogScope};
use nuo_ui_primitives::primitives::{ContentModalSpec, FixedModalSpec, ModalSpec};
use nuo_view::model::layout::LayoutMap;
use nuo_view::model::selection::SelectionState;

/// An embedded single-line text field owned by a dialog. Replaces the old
/// practice of borrowing the thread composer line for a dialog's
/// filter/query (`[INV-SURFACE-01]`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    /// The live text.
    pub text: String,
    /// Caret position as a byte offset into `text`.
    pub cursor: usize,
}

impl TextInput {
    /// Clear the field and reset the caret.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Whether the field is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The trimmed query, for fuzzy matching.
    pub fn query(&self) -> &str {
        self.text.trim()
    }

    /// Move the caret to the end of the text.
    pub fn set_cursor_end(&mut self) {
        self.cursor = self.text.len();
    }
}

/// The result of offering a key to a dialog entity (`[INV-SURFACE-01]`).
#[derive(Debug)]
pub enum DialogOutcome {
    /// The entity handled the input.
    Consumed,
    /// The entity does not own this input; the event loop continues.
    Unhandled,
    /// The entity asks to be dismissed.
    Dismiss,
    /// The entity hands its surface to a successor view.
    SwitchTo(Box<dyn DialogView>),
}

impl PartialEq for DialogOutcome {
    /// Identity equality only: a `SwitchTo` payload is a trait object and is
    /// never equal to another outcome, including another `SwitchTo`.
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (DialogOutcome::Consumed, DialogOutcome::Consumed)
                | (DialogOutcome::Unhandled, DialogOutcome::Unhandled)
                | (DialogOutcome::Dismiss, DialogOutcome::Dismiss)
        )
    }
}

impl DialogOutcome {
    /// Whether the outcome consumed the input action.
    pub fn is_consumed(&self) -> bool {
        matches!(self, Self::Consumed)
    }
}

/// The immutable environment injected into [`DialogView::render`]: server-fed
/// model data (read-only) plus the per-frame layout/selection context. Dialogs
/// draw from their own state plus these injected views — never from the
/// composer.
pub struct DialogRenderCtx<'a> {
    pub app: &'a crate::shell::App,
    pub layout_map: &'a mut LayoutMap,
    pub selection: &'a SelectionState,
    pub theme: &'a Theme,
    pub spinner_phase: usize,
    pub viewed_session_id: &'a str,
    pub startup_picker: bool,
    pub input_rect: Option<Rect>,
    pub activity_height: u16,
    pub overlay_owns_caret: bool,
}

/// The presentation contract every floating dialog implements.
pub trait DialogView: std::fmt::Debug + Send + 'static {
    /// This dialog's identity.
    fn kind(&self) -> DialogKind;

    /// This dialog's identity as a compile-time constant, for generic
    /// accessors that must resolve an entity by its concrete type.
    fn kind_static() -> DialogKind
    where
        Self: Sized;

    /// This dialog's ownership domain. Defaults to the static classification
    /// on [`DialogKind::scope`] so the matrix has exactly one definition.
    fn scope(&self) -> DialogScope {
        self.kind().scope()
    }

    /// Visual layout constraints for modal positioning.
    fn layout_spec(&self) -> ModalSpec {
        modal_spec_for(self.kind())
    }

    /// Self-contained input handling: consumes a normalized action and
    /// produces an outcome. Semantic App effects are emitted by the entity;
    /// the event loop applies the outcome.
    fn handle_input(
        &mut self,
        action: &InputAction,
        app: &mut crate::shell::App,
        viewed_session_id: &str,
    ) -> DialogOutcome {
        let _ = (action, app, viewed_session_id);
        DialogOutcome::Unhandled
    }

    /// Autonomous rendering: draws entirely from internal state plus the
    /// injected immutable [`DialogRenderCtx`].
    fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        ctx: &mut DialogRenderCtx<'_>,
    ) -> Option<Rect> {
        let _ = (frame, area, ctx);
        None
    }

    /// The dialog's selection cursor.
    fn nav_index(&self) -> usize;
    /// Set the dialog's selection cursor.
    fn set_nav_index(&mut self, value: usize);
    /// Whether the in-dialog key-reference sub-layer is open.
    fn keys_open(&self) -> bool;
    /// Open or close the in-dialog key-reference sub-layer.
    fn set_keys_open(&mut self, open: bool);
    /// The key-reference sub-layer's scroll offset.
    fn keys_scroll_mut(&mut self) -> &mut usize;

    /// Deterministic dismissal hook, executed by the overlay stack on pop and
    /// on scene/session unwinding (`[INV-SURFACE-04]`).
    fn on_dismiss(&mut self) {}

    /// Clear all state back to its first-open value. Used for ephemeral
    /// dismissal and for session-scoped domain isolation (`[INV-SURFACE-05]`).
    fn reset(&mut self);

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn into_any(self: Box<Self>) -> Box<dyn Any>;

    /// Clone into a boxed trait object. Used to leave a read-consistent
    /// placeholder in a stack entry while the live entity is rendered/input.
    fn clone_box(&self) -> Box<dyn DialogView>;
}

/// The canonical [`ModalSpec`] geometry for a dialog kind.
pub fn modal_spec_for(kind: DialogKind) -> ModalSpec {
    match kind {
        DialogKind::Connections => FixedModalSpec::PROVIDER.modal_spec(),
        DialogKind::Models => FixedModalSpec::MODELS.modal_spec(),
        DialogKind::Threads => FixedModalSpec::SESSIONS.modal_spec(),
        DialogKind::Tools | DialogKind::SessionTree => ContentModalSpec::TOOLS.modal_spec(),
        DialogKind::Mcp => ContentModalSpec::MCP.modal_spec(),
        DialogKind::Queue => ContentModalSpec::QUEUE.modal_spec(),
        DialogKind::ThreadStats => ContentModalSpec::THREAD_STATS.modal_spec(),
        DialogKind::SessionTrace => ContentModalSpec::SESSION_TRACE.modal_spec(),
        DialogKind::UsageStats | DialogKind::Quotas => ContentModalSpec::USAGE_STATS.modal_spec(),
        DialogKind::Permissions => ContentModalSpec::PERMISSIONS.modal_spec(),
        DialogKind::Skills => ContentModalSpec::SKILLS.modal_spec(),
        // The history panel is a composer-anchored dropdown and the switcher
        // is a command palette: neither uses a centered modal footprint.
        DialogKind::HistorySearch
        | DialogKind::Switcher
        | DialogKind::PortShare
        | DialogKind::Help
        | DialogKind::Notices => ContentModalSpec::TOOLS.modal_spec(),
    }
}

/// Per-dialog teardown run on dismissal: the entity's own hook plus clearing
/// its embedded search field, so a dismissed picker never leaves a stale query
/// behind (`[INV-SURFACE-01]`).
/// The type-erasing downcast below cannot fail: each `DialogView` implementation
/// reports exactly one `DialogKind`, so the cast is exempt from the workspace
/// `expect_used` deny (same contract as `input_dialog` / `render_dialog`).
#[allow(clippy::expect_used)]
pub fn dismiss_cleanup(view: &mut dyn DialogView) {
    match view.kind() {
        DialogKind::Models => {
            let d = view
                .as_any_mut()
                .downcast_mut::<ModelsDialog>()
                .expect("kind");
            d.search = false;
            d.query.clear();
        }
        DialogKind::Connections => {
            let d = view
                .as_any_mut()
                .downcast_mut::<ConnectionsDialog>()
                .expect("kind");
            d.search = false;
            d.query.clear();
        }
        DialogKind::HistorySearch => {
            let d = view
                .as_any_mut()
                .downcast_mut::<HistorySearchDialog>()
                .expect("kind");
            d.index = 0;
            d.scroll = 0;
            d.follow = true;
        }
        _ => {}
    }
}

/// Render one dialog entity from its own state plus the injected immutable
/// context (`DialogView::render`). The entity's fields are the only mutable
/// dialog state; `ctx` carries read-only server model data and layout context.
#[allow(clippy::expect_used)]
mod actions;
mod entities;
mod input;
mod navigation;
mod registry;
mod render;

#[cfg(test)]
mod tests;

pub use self::entities::*;
pub use self::registry::*;

#[cfg(test)]
pub(crate) use self::navigation::rotate_selectable;
