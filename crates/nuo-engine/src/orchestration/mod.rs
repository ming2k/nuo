//! Turn-level orchestration policy on top of the `Agent` struct.
//!
//! `Agent` (in [`crate::cognition::agent`]) runs a single ReAct turn against a provider.
//! This module wraps every turn with the cross-cutting policy a frontend
//! cannot reasonably reimplement: context compaction (pre-turn and mid-turn
//! pruning), retry with exponential backoff, permission relay, and the
//! `/schedule` cron + countdown scheduler.
//!
//! Frontends drive the harness through [`execute_round`],
//! [`start_interactive_round`], and
//! `start_schedule_scheduler`. They own only the UI-specific input path (slash commands for the CLI, menus/dialogs for a
//! future GUI); the actual round machinery is shared here.
//!
//! All items are `pub` because they are assembled by the binary, which knows
//! the concrete provider/tool instances and the frontend's request channel.
pub mod round_lifecycle;
pub mod session_title;

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::task::{Context, Poll};
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use futures::stream::{BoxStream, StreamExt};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{Agent, RequestTokenEstimate, RoundBegin, RoundLifecycle};
use nuo_persistence::{
    CommitTurn,
    config::Config,
    thread::{CommitBasis, ContextProjectionCheckpoint, ContextProjectionResult, ThreadSession},
};
use nuo_protocol::{AgentEvent, AgentResponse, HarnessError, HarnessSnapshot, LoopStatus, ModelRequest, Provider, ProviderStreamEvent, RoundEvent, RoundScope, TurnContext};
use nuo_tool::{ImagePart, InjectionKind, Message, NoticeKind, NoticeSeverity, NoticeSource, Role};


/// Wrap a session-scoped [`RoundEvent`] in the [`AgentResponse::Round`]
/// envelope. Every round-scoped emitter routes through this so the
/// session id is attached uniformly, letting the TUI key transcript buffers
/// by `session_id` correctly.
pub mod context_gate;

mod compaction;
mod execute;
mod harness_state;
mod interactive;
mod projection;
mod proxy;
mod relay;
mod usage;

#[cfg(test)]
mod tests;

pub use self::compaction::{
    compact_round_history, compact_round_history_with_mode, prune_and_commit, send_compaction,
};
pub use self::execute::{RoundCompletion, execute_round};
pub use self::harness_state::{send_harness_state_for_session, send_harness_state_running};
pub use self::interactive::{InteractiveRoundContext, start_interactive_round};
pub use self::projection::{CompactionReport, ContextProjectionSettings};
pub use self::proxy::ProxyProvider;
pub use self::relay::relay_agent_event;

pub fn round_response(session_id: &str, event: RoundEvent) -> AgentResponse {
    AgentResponse::Round {
        thread_id: session_id.to_string(),
        scope: RoundScope::Session { event },
    }
}

/// Wrap a turn-scoped event with the turn it mutated.
///
/// The context is mandatory: a turn event without a context is not a state the
/// protocol can express (ADR-0168).
pub fn turn_response(session_id: &str, context: TurnContext, event: RoundEvent) -> AgentResponse {
    AgentResponse::Round {
        thread_id: session_id.to_string(),
        scope: RoundScope::Turn { context, event },
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// The explicit "the fold did not reach the gate" signal.
fn compaction_insufficient_notice(tokens_after: usize, gate: usize) -> nuo_tool::AgentNotice {
    nuo_tool::AgentNotice::new(
        NoticeKind::ReviewAlert,
        NoticeSeverity::Warning,
        "Compaction could not bring the window under the limit",
        NoticeSource::Harness,
    )
    .with_body(format!(
        "The compacted window is {tokens_after} tokens; this model's compaction gate is {gate}. \
         The fold was kept (it is still smaller than before), but the next request may cross the \
         gate again. Run /compact for a round-shaped fold or switch to a larger window."
    ))
}

/// The user-facing explanation for withholding image attachments.
///
/// One wording, because there is one kind of evidence: the probe's outcome. The
/// harness deliberately does **not** read the vendor's error text to decide
/// whether images were the cause — every vendor formats that differently — so it
/// states the empirical finding instead of attributing a claim to the provider
/// that it may never have made.
fn image_withheld_notice(model: &str) -> nuo_tool::AgentNotice {
    nuo_tool::AgentNotice::new(
        NoticeKind::ImageInputWithheld,
        NoticeSeverity::Warning,
        format!("{model} cannot take images — continuing without them"),
        NoticeSource::Harness,
    )
    .with_body(format!(
        "A request to {model} was refused; retrying it without the attachments succeeded, so the \
         images were the cause. They were left out of the retried request — and of later requests \
         on this route. The images stay in this session's history and reappear when you switch \
         back to a model that accepts them. To force images onto this route anyway, set Vision to \
         \"force on\" for it in the model editor."
    ))
}

#[derive(Clone)]
pub struct RoundContext {
    pub agent: Arc<Agent>,
    pub tx: mpsc::UnboundedSender<AgentResponse>,
    pub token: CancellationToken,
    pub session: Arc<ThreadSession>,
    /// Session id this round belongs to. Tags every emitted
    /// [`RoundEvent`].
    pub session_id: String,
    pub projection: ContextProjectionSettings,
    pub retry_max_attempts: usize,
    /// Budget for attempts that failed *after* visible output (text, reasoning,
    /// or a tool-call announcement). Separate from `retry_max_attempts` because
    /// each such retry must discard visible state; a provider that dies
    /// mid-stream will usually do it again, so this budget is deliberately
    /// small. Counts restarts, not initial attempts.
    pub stream_restart_max_attempts: usize,
    pub retry_base_ms: u64,
    pub retry_max_ms: u64,
    /// Emit the frontend's natural-completion signal. Repeat drivers
    /// call `execute_round` internally and must not release a user's paused
    /// next-round outbox between their own continuation iterations.
    pub emit_round_completed: bool,
}

/// Which kind of round `execute_round` is about to run — a fresh round, or
/// the `/retry` resume of a stopped one..
#[derive(Clone, Debug)]
pub enum RoundDriver {
    /// A normal round: a new prompt is admitted, the round counter bumps, and
    /// turn numbering starts at 0.
    Fresh,
    /// `/retry`: the round that stopped before completing continues *as
    /// itself*. The counter must not bump, turns number onward from the
    /// committed count, and the history is re-seeded from the checkpoint.
    Resume {
        /// The durable resume point captured when the round stopped.
        point: nuo_protocol::RetryPoint,
    },
}

pub struct RoundInput {
    pub prompt: String,
    pub hidden: bool,
    pub display_prompt: Option<String>,
    /// Exact TUI send time for user-authored messages, in Unix-epoch milliseconds.
    pub sent_at_ms: Option<u64>,
    /// Inline images pasted into the prompt, attached to the user message.
    pub images: Vec<ImagePart>,
    /// Which round this input drives: a fresh prompt or a `/retry` resume of
    /// the stopped round. Replaces the old boolean: a resume carries its
    /// checkpoint, which is what makes the round "complete itself" instead of
    /// starting over.
    pub driver: RoundDriver,
}

impl RoundInput {
    /// Shorthand for the common fresh-prompt construction sites.
    pub fn fresh(prompt: String) -> Self {
        Self {
            prompt,
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: RoundDriver::Fresh,
        }
    }

    /// Shorthand for the `/retry` resume construction site.
    pub fn resume(point: nuo_protocol::RetryPoint) -> Self {
        Self {
            prompt: String::new(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: RoundDriver::Resume { point },
        }
    }

    /// Whether this input bypasses the UserPromptSubmit hook gate. A resume
    /// re-sends the already-admitted request; the gate already ran when the
    /// stopped round was admitted.
    pub fn is_retry(&self) -> bool {
        matches!(self.driver, RoundDriver::Resume { .. })
    }
}
