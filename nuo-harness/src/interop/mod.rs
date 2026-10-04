//! Interoperability, human-in-the-loop, and subagent coordination subsystem.
//!
//! Provides human confirmation brokers, subagent invocation primitives, and
//! tenant agent slot containment.

pub mod agent_slot;
pub mod human_broker;
pub mod subagent_tool;

pub use agent_slot::AgentSlot;
pub use human_broker::*;
pub use subagent_tool::{SubagentRegistry, SubagentTool};
