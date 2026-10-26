//! The orchestration layer between the pure domain (`nuo-wire`) and the
//! application services (`nuo-persistence`) on one side, and the frontends on the
//! other.
//!
//! # What lives here
//!
//! The crate is organized into subsystem groups (ADR-0101). The intended
//! dependency direction — `runtime ⊳ governance ⊳ cognition ⊳ {dispatch,
//! orchestration}` — is a strict inner-layer rule enforced by `tests/layering.rs`.
//!
//! - **`cognition/`** — the single agent's mind: the `Agent` struct and the
//!   streaming ReAct loop (`cognition::agent`), model-request assembly
//!   (`cognition::model_request`), durable harness-authored context
//!   (`cognition::conversation_context`), causal compaction, the out-of-band task
//!   pipeline and aspect engine, skills, and agent interoperability (subagents,
//!   the human broker).
//! - **`governance/`** — the restraints: permission brokering, bash policy,
//!   execution containment, trajectory/stream guards, and interactive gating.
//!   A strict inner layer (`[INV-HARNESS-03]`).
//! - **`orchestration/`** — the per-turn policy wrapping cognition: compaction
//!   gating, retries with backoff, round lifecycle, session titles, and the
//!   `/schedule` scheduler. Frontends drive the harness through
//!   [`orchestration::execute_round`] and friends.
//! - **`dispatch/`** — the tool execution pipeline and scheduler.
//! - **`runtime/`** — the innermost substrate: host ports, execution
//!   environment, durability sinks, hooks, the message/time/retry leaves, and
//!   extensions (`[INV-HARNESS-04]`).
//! - **`tools/`** — orchestration-native model tools (ask_user, recall, todo).
//!
//! # Dependency posture
//!
//! `nuo-harness` is the runtime wiring layer: it depends on `nuo-wire` (domain
//! vocabulary), `nuo-persistence` (durable state: `SessionStore`, `Config`,
//! `EmbeddingStore`), `nuo-model-routing`, and the capability tool crates. Concrete
//! filesystem / process / web / AST tools live in `nuo-host` and `tools/nuo-tool-*`;
//! this crate's [`tools`] module holds only orchestration-native model tools
//! (ask_user, recall, todo). Capabilities arrive through the core [`Tool`] and
//! [`ToolSet`] contracts.
//!
//! ## Why SubagentTool lives here (not in a tool crate)
//!
//! **`SubagentTool`** spawns subagents via the `Agent`. It used to live in the
//! former `nuo-tools` crate, which forced tools to depend on this crate — an
//! inversion, since tools sit below the runtime. The subagent tool is
//! fundamentally a runtime primitive that happens to satisfy the `Tool` trait,
//! so it lives here, under `cognition::interop`.
//!
//! Everything `nuo-wire` exports is re-exported here so consumers can
//! `use nuo_harness::*` and get the full domain vocabulary alongside the
//! orchestration API.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

// Bounded contract re-exports: only protocol types that cross the harness boundary.
pub use nuo_wire::{
    AgentEvent, AgentIdentity, AgentNotice, AgentOp, AgentRequest, AgentResponse, AgentRoleProfile,
    DynamicToolSink, Hook, HookContext, HookEvent, HookEventKind, HookOutcome, InstructionTier,
    LoopStatus, Message, PermissionDecision, Provider, ProviderStreamEvent, RetryPoint, Role,
    RoundEvent, SessionDelta, SessionSource, SubagentEvent, TokenUsage, Tool, ToolContext,
    ToolOutput, ToolSet, ToolStream, Transport, async_trait,
};

#[allow(unused_imports)]
pub(crate) use nuo_wire::{
    ExecutionEnvironment, HarnessError, ImagePart, InjectionKind, InjectionOrigin, InputRequest,
    NoticeKind, NoticeSeverity, NoticeSource, NoticeSurface, QueuedMessage, StdinReply, TodoList,
    ToolCall, UserQuestion, UserQuestionReply, UserQuestionRequest, WorkspaceRoot, WorkspaceRoots,
    message, pressure,
};

// Same ambient std/tokio prelude the Agent struct used to inherit from
// `nuo-wire`'s lib.rs (`use super::*`).
use futures::StreamExt;
use std::collections::HashSet;
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

// -----------------------------------------------------------------------------
// Subsystem: Cognition — the single agent's mind
// -----------------------------------------------------------------------------
pub mod cognition;
pub use cognition::agent::TitleEstablishedFn;
pub use cognition::agent::{Agent, AgentBuilder, RequestTokenEstimate, RoundOutcome, SwitchedRole};
pub use cognition::context_projection::ContextProjectionGate;
pub use cognition::interop::agent_slot::AgentSlot;
pub use cognition::interop::human_broker::*;
pub use cognition::interop::subagent_tool::{SubagentRegistry, SubagentTool};
pub use cognition::interop::{agent_slot, human_broker, subagent_tool};
pub use cognition::model_request::system_prompt::{
    InstructionOrder, SystemPromptContext, SystemPromptRegistry, SystemPromptRegistryError,
    SystemPromptSection,
};
pub use cognition::skills::{
    DiscoveryResult, ListSkillsTool, ShadowedSkill, Skill, SkillDependency, SkillHost, SkillPolicy,
    SkillRegistry, SkillRoots, SkillScope, SkillTrust, UseSkillTool, discover_all,
    discover_all_with_trust_state, discoverable_skill_directories, format_skill_injection,
    format_skill_list, list_skill_files, project_skills_present, resolve_mentions,
};

// -----------------------------------------------------------------------------
// Subsystem: Governance & Policy Enforcement
// -----------------------------------------------------------------------------
pub mod governance;
pub use governance::{
    DegeneratePattern, GuardAction, InteractionConfig, InteractionController, RoundGuardState,
    StreamLoopDetector, TrajectoryLoopGuard,
};
pub(crate) use governance::{bash_policy, permission_policy, permission_store, shell_input};
pub use governance::{guard, interaction, stream_loop_detector, trajectory_guard};

// -----------------------------------------------------------------------------
// Subsystem: Orchestration — per-round policy, lifecycle, titles & aspects
// -----------------------------------------------------------------------------
pub mod orchestration;
pub use cognition::aspects::AspectEngine;
pub use cognition::pipeline::{
    CognitiveError, CognitivePipeline, HarnessTaskError, HarnessTaskPipeline,
};
pub use orchestration::round_lifecycle::{ParkedInterrupt, RoundBegin, RoundLifecycle};
pub use orchestration::{
    ContextProjectionSettings, compact_round_history, compact_round_history_with_mode,
    round_response, send_compaction,
};

// -----------------------------------------------------------------------------
// Subsystem: Dispatch, Execution Pipeline & Tool Scheduling
// -----------------------------------------------------------------------------
pub mod dispatch;
#[allow(unused_imports)]
pub(crate) use dispatch::pipeline as dispatch_pipeline;
pub use dispatch::tool_call::extract_partial_string_field;
pub use dispatch::{dynamic, tool_call};
pub(crate) use dispatch::{dynamic_tools, tool_integration, tool_manager, tool_scheduler};

// -----------------------------------------------------------------------------
// Subsystem: Runtime substrate — env, durability, hooks, ports
// -----------------------------------------------------------------------------
pub mod runtime;
pub use runtime::durability::record::*;
pub use runtime::durability::{
    Ack, ConformanceReport, FactSink, Hydrate, MemorySink, NullSink, SinkError, SinkHealth,
    conformance as durability_conformance, record as execution_record, run_conformance,
};
pub use runtime::extension::CodeIntelligenceExtension;
pub use runtime::hooks::{HookRegistry, PreToolUseVerdict, UserPromptVerdict, matcher_matches};
pub use runtime::host::*;
pub use runtime::inflight::Inflight;
pub use runtime::no_provider::{NO_PROVIDER_ID, NoProvider};
pub use runtime::offstream::{
    OffstreamEntry, OffstreamRegistry, OffstreamSource, OffstreamStatus, PagedOffstreamContent,
};

// -----------------------------------------------------------------------------
// Tools (orchestration-native model tools)
// -----------------------------------------------------------------------------
pub mod tools;

#[cfg(test)]
mod tests;
