//! Scene-directed terminal input.
//!
//! `router` normalizes terminal events and owns the two cross-layer chains
//! global chords and Esc precedence. Surface verbs live with
//! their modal, sheet, or view handlers; `readline` owns shared text edits.
mod action;
pub(crate) mod interactive_key;
pub(crate) mod keystroke;
pub(crate) mod readline;
mod router;

pub use action::{InputAction, OauthCopyTarget};
pub use router::{Dispatch, event_family, resolve_block, route_event, route_event_motion};

// The legacy input regression modules are children of this module and use
// `super::*`; keep their test-only vocabulary local instead of widening the
// production API.
#[cfg(test)]
use graticule::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
#[cfg(test)]
use view::model::layout::{LayoutMap, SemanticCursor};
#[cfg(test)]
use view::model::selection::SelectionDrag;
#[cfg(test)]
mod tests;

// probe
