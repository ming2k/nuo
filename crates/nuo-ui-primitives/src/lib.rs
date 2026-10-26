//! Domain-agnostic terminal UI primitives, design tokens, and components for Nuo (ADR-0107).
//!
//! This crate holds the pure, zero-IO UI substrate — semantic keys and chords,
//! design tokens (theme, geometry, elevation), viewports and layout helpers,
//! pure text wrapping, and generic terminal components (modal, dropdown, list, toast, etc.).
//!
//! It possesses zero knowledge of AI agents, session transcripts, or tool calls
//! ([INV-ARCH-UI-PRIMITIVES-PURITY]).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod chord;
pub mod components;
pub mod design;
pub mod elevation;
pub mod fuzzy;
pub mod key;
pub mod keyvocab;
pub mod primitives;
pub mod syntax;
pub mod text_layout;
pub mod theme;
pub mod time;

pub use components::action::ActionStatus;
pub use theme::Theme;
