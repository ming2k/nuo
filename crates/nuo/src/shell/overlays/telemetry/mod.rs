//! Thread Stats and Session Trace overlays (ADR-0037, ADR-0263).
//!
//! Separated into two distinct surfaces:
//! - [`draw::draw_thread_stats_modal`] — Thread Stats (/stats)
//! - [`draw::draw_session_trace_modal`] — Session Trace (/trace) with orthogonal
//!   `Enter` drill-in (`Rounds` -> `Turns`) and `i` inspection (`RoundInfo` / `AttemptInfo`)
//!
//! Rendering lives in level-focused submodules:
//! - [`draw`]     — modal chrome, breadcrumbs, view routing
//! - [`overview`] — stats overview body
//! - [`round`]    — L1 round inspector (timing breakdown, activity, tokens/cost)
//! - [`tables`]   — trace rounds/turns tables
//! - [`attempt`]  — L2 attempt inspector with the latency timeline

pub mod attempt;
pub mod draw;
pub mod model;
pub mod overview;
pub mod round;
pub mod tables;

#[cfg(test)]
mod tests;

pub use draw::{draw_session_trace_modal, draw_thread_stats_modal};
pub use model::{
    ContextUsageProps, telemetry_attempt_count, telemetry_attempt_key, telemetry_round_count,
};
