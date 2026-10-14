//! Cognitive agent runtime: session lifecycle, tool dispatch, and
//! token-pressure management, plus first-class agent-to-agent collaboration.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub use acp;
pub use acp as comm;
pub use acp as protocol;
#[cfg(feature = "mcp")]
pub use nuo_mcp as mcp;
#[cfg(feature = "wire")]
pub use nuo_model_codec as wire;
pub use nuo_tool as tool;

pub mod agent;
pub mod agent_kind;
pub mod collaboration;
pub mod identity;
pub mod error;
pub mod memory;
pub mod message;
pub mod provider;
pub mod session;
pub mod skill;
pub mod token;
pub mod tools;

pub use agent::{Agent, AgentBuilder, ChannelWake};
pub use agent_kind::AgentKind;
pub use identity::AgentIdentity;
pub use collaboration::{
    Collaboration, CollaborationMode, CollaborationTool, DelegateToPeerTool, DelegationContext,
    ListChannelsTool, ListPeersTool, OpenChannelTool, PublishToChannelTool, ReadChannelTool,
    SubscribeChannelTool, install_collaboration_tools, install_direct_delegation_tools,
};
pub use error::{AgentError, Result};
pub use memory::{
    EmbeddingProvider, InMemoryMemory, Memory, MemoryFact, MemoryQuery, MemoryScope, TurnOutcome,
    cosine_similarity, reciprocal_rank_fusion,
};
pub use message::{Message, Role, ToolCall, ToolResult};
#[cfg(feature = "wire")]
pub use provider::{ModelCodecAdapter, WireProvider};
pub use provider::{
    MockProvider, ModelRequest, ModelResponse, Provider, ProviderDelta, TokenUsage,
};
pub use session::{
    InMemorySessionStore, Inbound, Session, SessionEvent, SessionKey, SessionStore, SteeringEffect,
    SteeringHandle, TurnQueue,
};
pub use skill::{Skill, SkillBuilder, SkillRegistry, SkillStack};
pub use token::{
    CompactionPolicy, Compactor, FileObservationStore, InMemoryObservationStore, ObservationStore,
    PressureLevel, TokenBudget, TokenCounter,
};
pub use tools::{
    AlwaysApprove, ApprovalDecision, ApprovalHandler, CommandTool, DynamicTool, InspectTool,
    McpTool, RiskProfile, ShellKind, Tool, ToolCallRequest, ToolContext, ToolError, ToolOutput,
    ToolPolicy, ToolRegistry, ToolScope,
};
