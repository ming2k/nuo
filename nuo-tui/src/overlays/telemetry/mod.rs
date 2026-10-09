//! Session Stats and Session Trace overlays (ADR-0037).
//!
//! Separated into two distinct surfaces:
//! - [`draw::draw_session_stats_modal`] — Session Stats (/stats)
//! - [`draw::draw_session_trace_modal`] — Session Trace (/trace) with hierarchical drill-in (L1/L2/L3)
//!
//! Rendering lives in level-focused submodules:
//! - [`draw`]     — modal chrome, breadcrumbs, level routing
//! - [`overview`] — stats overview body
//! - [`tables`]   — trace rounds/turns tables
//! - [`attempt`]  — attempt inspector with the latency timeline

pub mod attempt;
pub mod draw;
pub mod model;
pub mod overview;
pub mod tables;

#[cfg(test)]
mod tests;

pub use draw::{draw_session_stats_modal, draw_session_trace_modal};
pub use model::{
    ContextUsageProps, telemetry_attempt_count, telemetry_attempt_key, telemetry_round_count,
};
