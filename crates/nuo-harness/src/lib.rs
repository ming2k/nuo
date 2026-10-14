//! The orchestration layer between the pure domain (`nuo-wire`) and the
//! application services (`nuo-persistence`) on one side, and the frontends on the
//! other.
//!
//! # What lives here
//!
//! - **The `Agent` struct** (`agent.rs`) — holds the provider, tool set, mode,
//!   and skill registry; runs the streaming ReAct loop
//!   (`run_streaming_with_events`).
//! - **Model-request assembly** (`model_request/`) — immutable request
//!   projection and system-prompt policy. Durable harness-authored messages
//!   live separately under `conversation_context/`.
//! - **Extension integration** — consumes an optional `nuo-skills`
//!   registry for model-context injection and accepts connector tools through
//!   a protocol-neutral dynamic-tool port. Discovery and transport stay in
//!   their dedicated capability crates.
//! - **Turn orchestration** (`orchestration.rs`) — the policy that wraps every
//!   agent turn: compaction, mid-turn pruning, retries with backoff, and the
//!   `/repeat` cron scheduler. Frontends drive the harness
//!   through [`orchestration::execute_round`] and friends; they own only the
//!   UI-specific input path (slash commands for the CLI, menus/dialogs for a
//!   future GUI).
//!
//! # Dependency posture
//!
//! `nuo-harness` is the wiring layer: it depends on `nuo-wire`
//! (domain vocabulary), `nuo-persistence` (durable state: `SessionStore`,
//! `Config`, `EmbeddingStore`), and `nuo-providers` (the
//! `build_provider_for_channel` factory plus the user-agent / spec
//! constants the catalog uses when constructing concrete impls). The
//! concrete coding-tool implementations live in this crate's [`tools`]
//! module; skill capability comes from `nuo-skills`, and tools are
//! dispatched through the
//! core [`Tool`] and [`ToolSet`] contracts. These dependencies point downward
//! (`agent -> skills`); orchestration-native tools that
//! construct or control agents remain in this crate.
//!
//! ## Why catalog and SubagentTool live here (not in store / tools)
//!
//! Both got relocated here from their intuitive homes to keep the
//! dependency graph strictly layered (see ADR-0005):
//!
//! - **`catalog`** builds concrete `Provider` impls from a `Config`. It
//!   used to live in `nuo-persistence`, which forced store to depend on
//!   `nuo-providers` — an inversion, since store is otherwise a peer
//!   of providers. The catalog is fundamentally a factory consumed by
//!   orchestration, so it lives where orchestration lives.
//! - **`SubagentTool`** spawns subagents via `Agent::new`. It used to live
//!   in the former `nuo-tools` crate, which forced tools to depend on
//!   this crate —
//!   another inversion, since tools are below the agent layer. The
//!   subagent tool is fundamentally an orchestration primitive that
//!   happens to satisfy the `Tool` trait, so it lives here too.
//!
//! Everything `nuo-wire` exports is re-exported here so consumers can
//! `use nuo_harness::*` and get the full domain vocabulary alongside the
//! orchestration API.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

// Bounded contract re-exports: only protocol types that cross the harness boundary.
pub use nuo_wire::{
    async_trait, AgentEvent, AgentIdentity, AgentNotice, AgentOp, AgentRequest, AgentResponse,
    AgentRoleProfile, DynamicToolSink, Hook, HookContext, HookEvent, HookEventKind, HookOutcome,
    InstructionTier, LoopStatus, Message, PermissionDecision, Provider, ProviderStreamEvent,
    RetryPoint, Role, RoundEvent, SessionDelta, SessionSource, SubagentEvent, TokenUsage, Tool,
    ToolContext, ToolOutput, ToolSet, ToolStream, Transport,
};

#[allow(unused_imports)]
pub(crate) use nuo_wire::{
    message, pressure, ExecutionEnvironment, HarnessError, ImagePart, InjectionKind,
    InjectionOrigin, InputRequest, NoticeKind, NoticeSeverity, NoticeSource, NoticeSurface,
    QueuedMessage, StdinReply, TodoList, ToolCall, UserQuestion, UserQuestionReply,
    UserQuestionRequest, WorkspaceRoot, WorkspaceRoots,
};

// Same ambient std/tokio prelude the Agent struct used to inherit from
// `nuo-wire`'s lib.rs (`use super::*`).
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Maximum interval between consecutive stream events (text/reasoning/tool-call
/// deltas) before the stream is considered stalled. The shared LLM client sets
/// a connect timeout but deliberately no read timeout on streaming responses
/// (a legitimate stream may pause between deltas), so without this guard a
/// reasoning model whose SSE connection hangs mid-generation (server stops
/// sending but keeps the TCP connection alive) blocks the turn loop
/// indefinitely — the UI spins "running · responding" forever and only a user
/// interrupt can break it. The bound is generous: reasoning models stream
/// deltas frequently and SSE keepalives arrive every 15–30 s, so two full
/// minutes of total silence is a genuine stall. On timeout the harness
/// surfaces a retryable error so the turn retries with backoff instead of
/// hanging.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// How long a provider stream that has *already delivered output this turn*
/// gets to reach its natural end after the round is cancelled, before the
/// cancellation is honoured anyway. This closes the biased-select race at the
/// end of an answer: the model can emit its final delta (and the terminal
/// `usage` chunk) in the same instant the user sends the next message or hits
/// Esc Esc — the UI has already rendered a complete answer, but the cancel arm
/// of the stream `select!` used to win the very next poll, unwinding the round
/// as `Interrupted` and later projecting a false "▲ interrupted · new message"
/// marker over a round that finished. Within this window the stream is drained
/// normally (chunks keep flowing, so a still-generating answer completes or
/// the window expires); if the stream stays silent past it, the interrupt
/// stands. Kept short — an interrupt must feel instant, and a stream that
/// needs longer than this to finish was not settling.
pub(crate) const FINISH_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(750);

/// How long the tool executors wait for a cooperatively-cancelled in-flight
/// call (a subagent) to drain after the user interrupts a turn, before falling
/// back to dropping its future. The subagent observes its token at the next safe
/// boundary (the current provider stream or tool call, both bounded by their
/// own timeouts) and returns its partial transcript — normally in well under
/// a second. This is the backstop for pathological cases (a child parked on a
/// human answer it will never get because the same human just pressed Esc).
/// Bounded so an interrupt never hangs the UI.
const SUBAGENT_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

pub mod agent;
pub use agent::TitleEstablishedFn;
pub use agent::{Agent, AgentBuilder, RequestTokenEstimate, RoundOutcome, SwitchedRole};

// -----------------------------------------------------------------------------
// Subsystem: Governance & Policy Enforcement
// -----------------------------------------------------------------------------
pub mod governance;
pub(crate) use governance::{
    bash_policy, permission_policy, permission_store, shell_input,
};
pub use governance::{
    guard, interaction, stream_loop_detector, trajectory_guard,
};
pub use governance::{
    DegeneratePattern, GuardAction, InteractionConfig, InteractionController, RoundGuardState,
    StreamLoopDetector, TrajectoryLoopGuard,
};

// -----------------------------------------------------------------------------
// Subsystem: Dispatch, Execution Pipeline & Tool Scheduling
// -----------------------------------------------------------------------------
pub mod dispatch;
#[allow(unused_imports)]
pub(crate) use dispatch::pipeline as dispatch_pipeline;
pub(crate) use dispatch::{
    dynamic_tools, tool_integration, tool_manager, tool_scheduler,
};
pub use dispatch::{dynamic, tool_call};
pub use dispatch::tool_call::extract_partial_string_field;

// -----------------------------------------------------------------------------
// Subsystem: Interoperability, Human Gateways & Subagents
// -----------------------------------------------------------------------------
pub mod interop;
pub use interop::{agent_slot, human_broker, subagent_tool};
pub use interop::agent_slot::AgentSlot;
pub use interop::human_broker::*;
pub use interop::subagent_tool::{SubagentRegistry, SubagentTool};

// -----------------------------------------------------------------------------
// Subsystem: Durability, Audit & Conformance
// -----------------------------------------------------------------------------
pub mod durability;
pub use durability::{
    conformance as durability_conformance, record as execution_record,
    Ack, ConformanceReport, FactSink, Hydrate, MemorySink, NullSink, SinkError, SinkHealth,
    run_conformance,
};
pub use durability::record::*;

// -----------------------------------------------------------------------------
// Context, Compaction & Prompts
// -----------------------------------------------------------------------------
pub mod budget;
pub mod compaction;
pub mod context_lifecycle;
pub mod context_projection;
pub use context_projection::ContextProjectionGate;
mod conversation_context;
mod model_request;
pub use model_request::system_prompt::{
    InstructionOrder, SystemPromptContext, SystemPromptRegistry, SystemPromptRegistryError,
    SystemPromptSection,
};
pub mod offstream;
pub use offstream::{OffstreamEntry, OffstreamRegistry, OffstreamSource, OffstreamStatus, PagedOffstreamContent};
pub mod session_title;

// -----------------------------------------------------------------------------
// Orchestration & Runtime Lifecycle
// -----------------------------------------------------------------------------
pub mod aspects;
pub use aspects::AspectEngine;
pub mod cognitive;
pub use cognitive::{CognitiveError, CognitivePipeline, HarnessTaskError, HarnessTaskPipeline};
pub mod hooks;
pub use hooks::{HookRegistry, PreToolUseVerdict, UserPromptVerdict, matcher_matches};
mod hook_runner;
pub mod host;
pub use host::*;
pub mod inflight;
pub use inflight::Inflight;
pub mod no_provider;
pub use no_provider::{NO_PROVIDER_ID, NoProvider};
pub mod orchestration;
pub use orchestration::{
    compact_round_history, compact_round_history_with_mode, round_response, send_compaction,
    ContextProjectionSettings,
};
pub mod round_lifecycle;
pub use round_lifecycle::{ParkedInterrupt, RoundBegin, RoundLifecycle};

// -----------------------------------------------------------------------------
// Capabilities, Skills & Extensions
// -----------------------------------------------------------------------------
pub mod catalog;
pub mod extension;
pub use extension::CodeIntelligenceExtension;
pub mod skills;
pub use skills::{
    DiscoveryResult, ListSkillsTool, ShadowedSkill, Skill, SkillDependency, SkillHost,
    SkillPolicy, SkillRegistry, SkillRoots, SkillScope, SkillTrust, UseSkillTool, discover_all,
    discover_all_with_trust_state, discoverable_skill_directories, format_skill_injection,
    format_skill_list, list_skill_files, project_skills_present, resolve_mentions,
};
pub(crate) mod sync;
pub mod syntax {
    pub use nuo_code::syntax::*;
}
pub mod execution;
pub mod tools;

#[cfg(test)]
mod tests;

