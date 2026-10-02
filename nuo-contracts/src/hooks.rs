//! Lifecycle event hooks (ADR-0025): user-configurable interception at
//! session, round, turn, and tool-call points.
//!
//! muta keeps a single event axis — the context-threshold, turn-count, and
//! clock concerns are already owned by `CompactionPolicy` and `/repeat` and
//! are deliberately **not** re-exposed here. The capability a
//! hook has (block / inject / observe) is implicit in the event it fires on,
//! matching Claude Code's model: a `PreToolUse` hook may deny, a `Stop` hook
//! may force another turn, the rest only observe or inject context.
//!
//! v1 ships a single command-handler implementation (see `muta`); the
//! [`Hook`] trait lives here so the registry and insertion points in
//! `muta_agent` stay frontend-agnostic and so future handler types
//! (`http`, `mcp_tool`) slot in without re-touching the loop.

use crate::async_trait;
use serde::{Deserialize, Serialize};

/// How a session started. Surfed as the `SessionStart` source/matcher value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    Startup,
    Resume,
}

/// Which lifecycle point a hook fires on — the routing key only. The payload
/// travels in [`HookContext`]; matcher evaluation lives in the registry
/// (`muta_agent::hooks`), not here, so core stays free of the `regex` crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum HookEventKind {
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    Stop,
    PreCompact,
    PostCompact,
    /// Fires after each non-terminal ReAct turn, before the next model request
    /// (ADR-0030). Constrained: only `Inject` is honoured — `Deny` is ignored
    /// so a turn-count hook cannot become a de-facto turn cap (the ADR-0009
    /// concern). The harness declares no built-in threshold on this axis.
    Turn,
    /// Fires at the start of each ReAct turn —
    /// after tools are prepared but before the model is asked for its next
    /// completion. The symmetric partner of [`HookEventKind::Turn`] (which
    /// fires at turn end). Lets a hook re-inject context at the turn boundary
    /// so it lands at the top of the model's attention, e.g. to re-anchor the
    /// principal's role after read-only delegations (anti "role bleed").
    /// Constrained the same way as `Turn`: only `Inject` is honoured — `Deny`
    /// is ignored so the hook cannot gate or cap the round.
    #[serde(alias = "RoundStart")]
    TurnStart,
    /// Fires when the agent is about to block on a permission request (a tool
    /// with a real `ScopeTarget` needs user approval before it runs). Honours a
    /// tool-name matcher (so a hook can target e.g. only `bash` requests).
    /// Constrained: **observe-only** — only `Pass` is honoured; `Deny`/`Inject`
    /// are ignored so a notification hook cannot gate the prompt or alter the
    /// transcript. The natural use is fire-and-forget: spawn a desktop
    /// notification / terminal bell so the user knows the agent is blocked.
    PermissionRequest,
    /// Fires when the agent is about to block on an `ask_user` question (the
    /// model needs the operator's input). Same observe-only constraint as
    /// [`HookEventKind::PermissionRequest`]. Does not honour a matcher
    /// (`ask_user` is a single tool).
    UserQuestion,
}

impl HookEventKind {
    /// Whether this event filters on a tool name and so honours a matcher.
    pub fn is_tool_event(self) -> bool {
        matches!(
            self,
            Self::PreToolUse
                | Self::PostToolUse
                | Self::PostToolUseFailure
                | Self::PermissionRequest
        )
    }
}

/// Owned snapshot of the moment a hook fires. Serialized to JSON and piped to
/// command handlers on stdin. Owned (not borrowed) so it crosses the async
/// spawn into the command subagent without lifetime gymnastics.
#[derive(Debug, Clone)]
pub struct HookContext {
    pub session_id: String,
    pub cwd: Option<std::path::PathBuf>,
    pub event: HookEvent,
}

/// The payload for one fire. Tool events carry a name + a reduced view of the
/// input/output — commands read JSON on stdin, not live Rust values, so the
/// full [`crate::ToolOutput`] (which may embed a subagent transcript) is not
/// forwarded wholesale; its `to_text()` summary is.
// Hook payloads are constructed once per fire and immediately serialized to
// the hook process's stdin; boxing `PermissionRequest` would not reduce peak
// memory in any real workload, so the variant size spread is accepted.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum HookEvent {
    SessionStart {
        source: SessionSource,
    },
    SessionEnd,
    UserPromptSubmit {
        prompt: String,
    },
    PreToolUse {
        tool_name: String,
        tool_input: serde_json::Value,
    },
    PostToolUse {
        tool_name: String,
        tool_output: String,
        duration_ms: u64,
    },
    PostToolUseFailure {
        tool_name: String,
        error: String,
    },
    Stop {
        last_message: String,
    },
    PreCompact,
    PostCompact,
    /// Fires after each non-terminal ReAct turn (ADR-0030).
    /// `round` and `turn` identify the canonical nested position;
    /// `consecutive_readonly` carries the read-only-turn streak so a hook can
    /// act on "exploration without progress" without re-deriving it. Only
    /// `Inject` is honoured (see [`HookEventKind::Turn`]).
    Turn {
        round: u64,
        turn: usize,
        consecutive_readonly: u32,
    },
    /// Fires at the start of each ReAct turn. `round` is the one-based
    /// enclosing user round; `turn` is the zero-based index of the turn about
    /// to run (so the first is `0`);
    /// `consecutive_readonly` is the read-only streak carried from the previous
    /// turn. Only `Inject` is honoured (see [`HookEventKind::TurnStart`]).
    TurnStart {
        round: u64,
        turn: usize,
        consecutive_readonly: u32,
    },
    /// The agent is about to block waiting for a permission decision. Observe-
    /// only (see [`HookEventKind::PermissionRequest`]). Carries the full
    /// request so a notification hook can render "which tool / what scope".
    PermissionRequest {
        request: crate::PermissionRequest,
    },
    /// The agent is about to block waiting for the user to answer an
    /// `ask_user` question. Observe-only (see [`HookEventKind::UserQuestion`]).
    UserQuestion {
        request: crate::UserQuestionRequest,
    },
}

impl HookEvent {
    pub fn kind(&self) -> HookEventKind {
        match self {
            Self::SessionStart { .. } => HookEventKind::SessionStart,
            Self::SessionEnd => HookEventKind::SessionEnd,
            Self::UserPromptSubmit { .. } => HookEventKind::UserPromptSubmit,
            Self::PreToolUse { .. } => HookEventKind::PreToolUse,
            Self::PostToolUse { .. } => HookEventKind::PostToolUse,
            Self::PostToolUseFailure { .. } => HookEventKind::PostToolUseFailure,
            Self::Stop { .. } => HookEventKind::Stop,
            Self::PreCompact => HookEventKind::PreCompact,
            Self::PostCompact => HookEventKind::PostCompact,
            Self::Turn { .. } => HookEventKind::Turn,
            Self::TurnStart { .. } => HookEventKind::TurnStart,
            Self::PermissionRequest { .. } => HookEventKind::PermissionRequest,
            Self::UserQuestion { .. } => HookEventKind::UserQuestion,
        }
    }

    /// Tool name when this is a tool event; `None` otherwise (the matcher is
    /// then ignored).
    pub fn tool_name(&self) -> Option<&str> {
        match self {
            Self::PreToolUse { tool_name, .. }
            | Self::PostToolUse { tool_name, .. }
            | Self::PostToolUseFailure { tool_name, .. } => Some(tool_name),
            Self::PermissionRequest { request } => Some(&request.tool),
            _ => None,
        }
    }
}

/// What a hook decided. The effect each variant has depends on the firing
/// event; an irrelevant variant returned by a handler is ignored, so a command
/// that unconditionally prints `{"decision":"deny"}` only bites on events that
/// honour denial.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum HookOutcome {
    /// No effect. The default.
    #[default]
    Pass,
    /// `PreToolUse`: the call is blocked; `reason` becomes the tool error the
    /// model sees. `Stop`: the round continues for another turn with `reason`
    /// fed back as a hidden user message. Ignored on other events, including
    /// `Turn` (ADR-0030: a turn-count hook may not become a de-facto cap).
    Deny { reason: String },
    /// Inject `context` as a hidden user message the model sees on its next
    /// turn. Honoured on `UserPromptSubmit` (prepended), `Stop`,
    /// `PostToolUse`, and `Turn`. Ignored elsewhere.
    Inject { context: String },
    /// Temporarily hide the named tools from the model (their schemas are
    /// dropped and dispatch rejects them) until the [`RestorePoint`] fires,
    /// where they are re-enabled automatically. Honoured on `PreToolUse`,
    /// `TurnStart`, and `Turn`. **Not persisted**: scoped disables live only
    /// in memory and never reach the session store, so they never survive a
    /// restart and never collide with a user's manual `/tools` toggles (which
    /// use a separate, persisted mask).
    ///
    /// This lets a hook scope the agent's toolset to a scenario — e.g. a
    /// `PreToolUse` policy hook can drop `bash` for a read-only sub-task and
    /// have it come back at the turn boundary — without the user having to
    /// manage `/tools` manually.
    ScopeTools {
        disable: Vec<String>,
        restore_at: RestorePoint,
    },
}

/// When a [`HookOutcome::ScopeTools`] disable is automatically undone. The
/// harness clears every scoped disable whose restore point has fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RestorePoint {
    /// Undo at the end of the current ReAct turn
    /// (the next `Turn` hook boundary). Good for "narrow the toolset for just
    /// this turn" policies. `round_end` is accepted as a legacy alias.
    #[serde(rename = "react_turn_end", alias = "round_end")]
    TurnEnd,
    /// Undo when the whole user round ends (the model
    /// emits a text reply with no tool calls, or the round otherwise
    /// terminates). Good for "narrow the toolset for the rest of this user
    /// request" policies. `turn_end` is accepted as a legacy alias.
    #[serde(rename = "user_round_end", alias = "turn_end")]
    RoundEnd,
}

/// One user-configurable lifecycle hook (ADR-0025). A hook declares the
/// [`HookEventKind`] it wants and an optional tool-name matcher, then reacts
/// to each matching fire. The built-in implementation runs a shell command
/// (see `muta`); the trait lives here so the registry and insertion
/// points in `muta_agent` stay frontend-agnostic.
#[async_trait]
pub trait Hook: Send + Sync {
    fn kind(&self) -> HookEventKind;
    /// Tool-name filter. `None` matches every event; only tool events honour
    /// it. Syntax: a `|`-separated list of exact names (`"Write|Edit"`) when
    /// it matches `[a-zA-Z0-9_|]+`, otherwise a regular expression. Matching
    /// is implemented in `muta_agent::hooks`.
    fn matcher(&self) -> Option<&str> {
        None
    }
    /// Describe the runtime authority required to fire this hook. Hooks that
    /// perform no external action may return `None`; command/network hooks
    /// must submit the same hazard metadata as an equivalent tool call.
    fn permission_submission(&self, _ctx: &HookContext) -> Option<crate::ToolPermissionSubmission> {
        None
    }
    async fn fire(&self, ctx: &HookContext) -> HookOutcome;
}

#[cfg(test)]
mod tests {
    use super::{HookEventKind, RestorePoint};

    #[test]
    fn turn_start_accepts_the_legacy_event_name() {
        let kind: HookEventKind = serde_json::from_str("\"RoundStart\"").unwrap();
        assert_eq!(kind, HookEventKind::TurnStart);
        assert_eq!(serde_json::to_string(&kind).unwrap(), "\"TurnStart\"");
    }

    #[test]
    fn restore_points_write_canonical_names_and_read_legacy_names() {
        assert_eq!(
            serde_json::to_string(&RestorePoint::TurnEnd).unwrap(),
            "\"react_turn_end\""
        );
        assert_eq!(
            serde_json::to_string(&RestorePoint::RoundEnd).unwrap(),
            "\"user_round_end\""
        );
        assert_eq!(
            serde_json::from_str::<RestorePoint>("\"round_end\"").unwrap(),
            RestorePoint::TurnEnd
        );
        assert_eq!(
            serde_json::from_str::<RestorePoint>("\"turn_end\"").unwrap(),
            RestorePoint::RoundEnd
        );
    }
}
