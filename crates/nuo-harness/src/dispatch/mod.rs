//! Dispatch, execution middleware, and tool scheduling subsystem.
//!
//! Owns the execution middleware pipeline, dynamic tool registries,
//! concurrency scheduling, and tool call serialization.

pub mod dynamic;
pub(crate) mod dynamic_tools;
pub(crate) mod pipeline;
pub mod tool_call;
pub(crate) mod tool_integration;
pub(crate) mod tool_manager;
pub(crate) mod tool_scheduler;

pub use dynamic::*;
pub use tool_call::*;
