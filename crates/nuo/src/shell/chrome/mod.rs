//! Transient chrome around the input box: the consolidated activity strip with
//! an animated breathing-dot indicator, the completion menu anchored above the
//! input, and the persistent model bar pinned below the input (context usage,
//! stream rate, model identity).
//!
//! There is exactly one activity renderer. `activity_bar` was a second one that
//! no live code path called — only its own tests — and it carried a *divergent*
//! copy of the retry clause formatting (it re-derived `(N/M)` by splitting the
//! rendered sentence on `"retry "`). Two renderers for one bar is how the
//! clause grew a second vocabulary; it is deleted, not deprecated.

pub mod activity_strip;
pub mod common;
pub mod completion_menu;
pub mod model_bar;
pub mod persistence_health;
pub mod queue_bar;
pub mod tasks_bar;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use activity_strip::{ActivityStripProps, draw_activity_strip};
#[allow(unused_imports)]
pub use common::{
    Liveness, SPINNER_PHASES, breathing_color, classify_liveness, dot_color, format_elapsed,
    spinner_glyph, tilde_home,
};
pub use completion_menu::draw_completion_menu;
#[allow(unused_imports)]
pub use model_bar::{
    CONTEXT_USAGE_CRIT_THRESHOLD, CONTEXT_USAGE_WARN_THRESHOLD, ModelBarProps, ModelBarRects,
    draw_model_bar, format_token_count,
};
#[allow(unused_imports)]
pub use persistence_health::draw_persistence_health_bar;
#[allow(unused_imports)]
pub use queue_bar::{QueueBarProps, QueueItemProps, draw_queue_bar};
#[allow(unused_imports)]
pub use tasks_bar::{BackgroundTaskItem, TasksBarProps, draw_tasks_bar};
