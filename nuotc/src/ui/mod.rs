//! Retained UI runtime above the cell renderer (ADR-0195).
//!
//! Component descriptions are disposable. Mounted identities and local state
//! survive updates; only a successful presentation publishes geometry and
//! interaction. Logical ownership, clipping, paint order, and input barriers
//! have separate contracts. The renderer below this module knows none of them.

mod runtime;
mod scene;

pub use runtime::{Lifecycle, UiRuntime};
pub use scene::{
    Component, InputPolicy, LayoutBox, NodeId, NodeLayout, PointerPolicy, Scene, UiError,
};
