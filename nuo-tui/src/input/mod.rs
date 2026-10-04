//! Scene-directed terminal input.
//!
//! `router` normalizes terminal events and owns the two cross-layer chains
//! from ADR-0197: global chords and Esc precedence. Surface verbs live with
//! their modal, sheet, or view handlers; `readline` owns shared text edits,
//! and `sgr` guards terminal mouse-sequence leakage.

mod action;
pub(crate) mod readline;
mod router;
mod sgr;

pub use action::{InputAction, OauthCopyTarget};
pub use router::{Dispatch, event_family, resolve_block, route_event};
pub use sgr::{Feed, SgrLeakGuard};

// The legacy input regression modules are children of this module and use
// `super::*`; keep their test-only vocabulary local instead of widening the
// production API.
#[cfg(test)]
use crate::model::layout::{LayoutMap, SemanticCursor};
#[cfg(test)]
use crate::model::selection::SelectionDrag;
#[cfg(test)]
use crossterm::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
// Readline helpers consumed outside this module keep their stable paths.
pub(crate) use readline::{cursor_line_down, cursor_line_up};

#[cfg(test)]
mod tests;
