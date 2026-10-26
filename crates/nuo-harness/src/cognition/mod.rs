//! Cognition — the single agent's mind: the ReAct loop, prompt assembly,
//! conversation context, causal compaction, and agent interoperability.
pub mod agent;
pub mod aspects;
pub mod budget;
pub mod compaction;
pub mod context_lifecycle;
pub mod context_projection;
pub(crate) mod conversation_context;
pub mod interop;
pub(crate) mod model_request;
pub mod pipeline;
pub mod skills;
