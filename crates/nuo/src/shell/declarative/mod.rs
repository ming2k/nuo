//! Pure Declarative Component Paradigm (ADR-0072) and Storybook Protocol (ADR-0073).
//!
//! This module defines the architectural foundation for pure, isolated,
//! state-sovereign UI components in `nuo-ui`.

use nuo_ui_primitives::theme::Theme;
use nuotc::{Frame, Rect};

pub mod ask_user;
pub mod empty_hero_card;
pub use nuo_ui_primitives::key;
pub mod modal_card;
pub mod model_row_card;
pub mod notice_card;
pub mod notice_dialog_card;
pub mod palette_card;
pub mod reasoning_card;
pub mod story;
pub mod testing;
pub mod toast_card;
pub mod tool_card;
pub mod turn_band_card;
pub mod which_key_card;
pub mod workbench;

/// Canonical declarative component contract (ADR-0072).
///
/// Every component is a pure projection:
/// - Presentation is stateless with respect to the component instance.
/// - Local interaction state is owned entirely by `State`.
/// - Transitions occur strictly via `update(msg)`.
pub trait Component {
    /// Pure, immutable presentation data provided by the parent.
    type Props: Clone + Send + 'static;
    /// Self-contained local volatile state (selection index, scroll, cursor).
    type State: Default + Clone + Send + 'static;
    /// Discrete local event messages handled by this component.
    type Msg: Send + 'static;
    /// Outward commands or intent emitted back to the host/parent.
    type Cmd: Send + 'static;

    /// Pure rendering projection into the terminal frame.
    ///
    /// Invariant `[INV-TUI-DECL-01]`: Render functions must never borrow
    /// mutable application state or read external side effects.
    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    );

    /// Sovereign, deterministic state transition.
    ///
    /// Invariant `[INV-TUI-DECL-02]`: State transitions must occur exclusively
    /// within this function in response to a typed `Msg`.
    fn update(props: &Self::Props, state: &mut Self::State, msg: Self::Msg) -> Option<Self::Cmd>;
}

#[cfg(test)]
mod tests;
