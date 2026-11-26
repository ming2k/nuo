//! Pure zero-IO chrome renderers and projection types (ADR-0107):
//!
//! - [`activity_strip`] — consolidated single-row live activity & queue strip
//! - [`common`] — breathing-dot liveness, spinner glyphs, elapsed formatting
//! - [`model_bar`] — persistent model, effort, context gauge, and spend bar
//! - [`persistence_health`] — durability degradation banner
//! - [`queue_bar`] — queued outbox projection types
//! - [`tasks_bar`] — background tasks status bar

pub mod activity_strip;
pub mod common;
pub mod model_bar;
pub mod persistence_health;
pub mod queue_bar;
pub mod tasks_bar;

#[cfg(test)]
mod tests;

pub use activity_strip::{ActivityStripProps, draw_activity_strip};
pub use common::{
    Liveness, SPINNER_PHASES, breathing_color, classify_liveness, dot_color, format_elapsed,
    spinner_glyph, tilde_home,
};
pub use model_bar::{
    CONTEXT_USAGE_CRIT_THRESHOLD, CONTEXT_USAGE_WARN_THRESHOLD, ModelBarKeys, ModelBarProps,
    ModelBarRects, draw_model_bar, format_token_count,
};
pub use persistence_health::draw_persistence_health_bar;
pub use queue_bar::{QueueBarProps, QueueItemProps};
pub use tasks_bar::{BackgroundTaskItem, TasksBarProps, draw_tasks_bar};
