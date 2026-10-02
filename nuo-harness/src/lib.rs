//! The orchestration layer between the pure domain (`nuo-contracts`) and the
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
//! `nuo-harness` is the wiring layer: it depends on `nuo-contracts`
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
//!   in the former `muta-tools` crate, which forced tools to depend on
//!   this crate —
//!   another inversion, since tools are below the agent layer. The
//!   subagent tool is fundamentally an orchestration primitive that
//!   happens to satisfy the `Tool` trait, so it lives here too.
//!
//! Everything `nuo-contracts` exports is re-exported here so consumers can
//! `use nuo_harness::*` and get the full domain vocabulary alongside the
//! orchestration API.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub use nuo_contracts::*;

// Same ambient std/tokio prelude the Agent struct used to inherit from
// `nuo-contracts`'s lib.rs (`use super::*`).
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

mod bash_policy;
pub mod budget;
pub mod catalog;
pub mod compaction;
pub mod context_lifecycle;
pub mod context_projection;
mod conversation_context;
pub mod dynamic;
mod dynamic_tools;
pub mod hooks;
pub mod interaction;
pub mod trajectory_guard;
pub use interaction::{InteractionConfig, InteractionController};

pub mod human_broker;
pub use hooks::{HookRegistry, UserPromptVerdict, matcher_matches};
pub mod inflight;
pub use inflight::Inflight;
pub mod agent_slot;
mod dispatch_pipeline;
pub mod subagent_tool;
pub use agent_slot::AgentSlot;
pub mod guard;
mod hook_runner;
pub use guard::{GuardAction, RoundGuardState};
mod model_request;
pub mod no_provider;
pub mod orchestration;
pub use orchestration::{
    compact_round_history, compact_round_history_with_mode, round_response, send_compaction,
    ContextProjectionSettings,
};
mod permission_policy;
mod permission_store;
pub mod round_lifecycle;
pub use round_lifecycle::{ParkedInterrupt, RoundBegin, RoundLifecycle};
pub mod aspects;
pub use aspects::AspectEngine;
pub mod cognitive;
pub mod session_title;
mod shell_input;
pub use cognitive::{CognitiveError, CognitivePipeline, HarnessTaskError, HarnessTaskPipeline};
pub mod stream_loop_detector;
pub mod skills;
pub use skills::{
    DiscoveryResult, ListSkillsTool, ShadowedSkill, Skill, SkillDependency, SkillHost,
    SkillPolicy, SkillRegistry, SkillRoots, SkillScope, SkillTrust, UseSkillTool, discover_all,
    discover_all_with_trust_state, discoverable_skill_directories, format_skill_injection,
    format_skill_list, list_skill_files, project_skills_present, resolve_mentions,
};
pub use stream_loop_detector::{DegeneratePattern, StreamLoopDetector};
pub mod execution;
pub mod tool_call;
pub use tool_call::extract_partial_string_field;
mod tool_integration;
mod tool_manager;
mod tool_scheduler;
pub mod tools;

pub mod extension;
pub(crate) mod sync;
pub mod syntax;
pub use extension::CodeIntelligenceExtension;

pub use context_projection::ContextProjectionGate;
pub use model_request::system_prompt::{
    InstructionOrder, SystemPromptContext, SystemPromptRegistry, SystemPromptRegistryError,
    SystemPromptSection,
};
pub use no_provider::{NO_PROVIDER_ID, NoProvider};
pub use subagent_tool::{SubagentRegistry, SubagentTool};

#[cfg(test)]
mod tests;

pub mod durability;
pub use durability::{Ack, FactSink, NullSink, SinkError, SinkHealth};

pub mod host;
pub use host::*;
pub mod execution_record;
pub use execution_record::*;

pub mod durability_conformance;
pub use durability_conformance::{ConformanceReport, Hydrate, MemorySink, run_conformance};
