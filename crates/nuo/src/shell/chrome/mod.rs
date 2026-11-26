//! Shell-coupled completion popup chrome.
//!
//! Pure zero-IO chrome bars (`activity_strip`, `model_bar`, `tasks_bar`,
//! `queue_bar`, `persistence_health`, `common`) live in `view::chrome`
//! (ADR-0107 `[INV-ARCH-VIEW-ZERO-IO]`). Only the completion popup — which
//! paints shell-owned completion items and interactive hit-map handles —
//! remains here.

pub mod completion_menu;

#[cfg(test)]
mod tests;

pub use completion_menu::draw_completion_menu;
