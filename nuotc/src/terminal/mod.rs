//! Terminal execution context, backend abstractions, and frame lifecycle.

pub mod backend;
pub mod frame;

pub use backend::{Backend, Bce};
pub use frame::{CursorState, Frame, Terminal, TestTerminal, Widget};
