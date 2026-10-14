//! Agent runtime: the agent and its private cognitive loop.
//!
//! The loop is an implementation detail of [`Agent`] rather than a separate
//! public construct, so provider, tool, and budget configuration has exactly one
//! owner and one entry point.

pub(crate) mod r#loop;

mod runtime;

pub use runtime::{Agent, AgentBuilder, ChannelWake};
