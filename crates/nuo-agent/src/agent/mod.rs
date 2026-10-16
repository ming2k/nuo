//! Agent runtime: the agent and its private cognitive loop.
//!
//! The loop is an implementation detail of [`Agent`] rather than a separate
//! public construct, so provider, tool, and budget configuration has exactly one
//! owner and one entry point.

pub(crate) mod r#loop;
pub mod spend_breaker;

mod runtime;

pub use runtime::{Agent, AgentBuilder, ChannelWake};
pub use spend_breaker::SpendCircuitBreaker;
