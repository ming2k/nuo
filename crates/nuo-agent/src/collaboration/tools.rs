//! Collaboration tools provided canonically by [`acp::tools`].
//!
//! Rather than duplicating tool implementations in agent runtimes,
//! tools for inter-agent communication, delegation, and channel messaging
//! are provided directly by the [`acp`] protocol crate and re-exported here.

pub use acp::tools::*;

/// Compatibility alias for the shared collaboration context.
pub type CollaborationCtx = acp::AcpToolContext;
