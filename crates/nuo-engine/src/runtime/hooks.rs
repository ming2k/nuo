//! The hook registry and matcher (ADR-0025).
//!
//! [`HookRegistry`] holds the `Vec<Arc<dyn Hook>>` installed on the [`crate::Agent`]
//! (mirroring the `reviews` list at `agent.rs`) and offers one typed query per
//! lifecycle event. Each query filters by event kind and tool-name matcher, fires
//! the matching hooks, and interprets the outcomes the way that event's
//! insertion point needs — so the loop calls a one-liner (`check_pre_tool_use`,
//! `run_post_tool_use`, `check_stop`, …) instead of reimplementing dispatch.
//!
//! The [`Hook`] trait itself and the payload types live in `nuo_protocol`; the
//! matcher (which needs `regex`) stays here so core stays regex-free.

use std::path::Path;
use std::sync::Arc;

use nuo_protocol::{
    Hook, HookContext, HookEvent, HookEventKind, HookOutcome, HookStartSource, InjectionKind,
    Message, PermissionRequest, UserQuestionRequest,
};

/// Read-only permission check used before a hook with runtime side effects is
/// fired. The closure receives the hook's structured hazard submission.
pub type HookAuthorizer =
    Arc<dyn Fn(&nuo_protocol::ToolPermissionSubmission) -> bool + Send + Sync>;

/// Evaluate a Claude-Code-style tool-name matcher against a tool name.
///
/// A matcher made only of `[a-zA-Z0-9_|]` is a `|`-separated list of exact
/// names (`"Write|Edit"`). Any other character makes it a regular expression
/// matched with [`regex::Regex`]. An invalid regex matches nothing and is warned.
pub fn matcher_matches(matcher: &str, tool_name: &str) -> bool {
    let simple = !matcher.is_empty()
        && matcher
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '|');
    if simple {
        return matcher.split('|').any(|part| part == tool_name);
    }
    match regex::Regex::new(matcher) {
        Ok(re) => re.is_match(tool_name),
        Err(_) => {
            tracing::warn!(matcher = matcher, "invalid regex in hook matcher; ignoring");
            false
        }
    }
}

pub use nuo_guardrails::{HookSideEffects, PreToolUseVerdict};

/// The set of hooks installed on an [`crate::Agent`]. Built once at startup
/// (from the `[hooks]` config, by the CLI) and read at every lifecycle point,
/// so it is shared cheaply as `Arc<HookRegistry>`.
#[derive(Default)]
pub struct HookRegistry {
    hooks: Vec<Arc<dyn Hook>>,
    authorizer: Option<HookAuthorizer>,
}

impl HookRegistry {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn new(hooks: Vec<Arc<dyn Hook>>) -> Self {
        Self {
            hooks,
            authorizer: None,
        }
    }

    /// Install hooks with a runtime permission checker. A side-effecting hook
    /// is skipped unless its exact submitted scope is already authorized.
    pub fn with_authorizer(hooks: Vec<Arc<dyn Hook>>, authorizer: HookAuthorizer) -> Self {
        Self {
            hooks,
            authorizer: Some(authorizer),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// Fire every hook of `kind`, honouring each hook's tool-name matcher when
    /// `tool_name` is supplied. Returns the outcomes in registration order.
    async fn fire(
        &self,
        kind: HookEventKind,
        tool_name: Option<&str>,
        ctx: &HookContext,
    ) -> Vec<HookOutcome> {
        let mut outcomes = Vec::new();
        for hook in &self.hooks {
            if hook.kind() != kind {
                continue;
            }
            if let (Some(tool), Some(matcher)) = (tool_name, hook.matcher())
                && !matcher_matches(matcher, tool)
            {
                continue;
            }
            if let Some(submission) = hook.permission_submission(ctx) {
                let allowed = self
                    .authorizer
                    .as_ref()
                    .is_some_and(|authorize| authorize(&submission));
                if !allowed {
                    tracing::warn!(
                        scope = %submission.scope,
                        hazard = ?submission.hazard_level,
                        "hook skipped: runtime permission required"
                    );
                    outcomes.push(HookOutcome::Pass);
                    continue;
                }
            }
            outcomes.push(hook.fire(ctx).await);
        }
        outcomes
    }

    /// `PreToolUse`: the first `Deny` reason wins and blocks the call. `None`
    /// means proceed. `Inject` is meaningless before a call and ignored.
    pub async fn check_pre_tool_use(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
        session_id: &str,
        cwd: Option<&Path>,
    ) -> PreToolUseVerdict {
        if self.hooks.is_empty() {
            return PreToolUseVerdict::default();
        }
        let ctx = HookContext {
            session_id: session_id.to_string(),
            cwd: cwd.map(Path::to_path_buf),
            event: HookEvent::PreToolUse {
                tool_name: tool_name.to_string(),
                tool_input: tool_input.clone(),
            },
        };
        let outcomes = self
            .fire(HookEventKind::PreToolUse, Some(tool_name), &ctx)
            .await;
        let mut deny = None;
        let mut side = HookSideEffects::default();
        for o in outcomes {
            match o {
                HookOutcome::Deny { reason } if deny.is_none() => deny = Some(reason),
                HookOutcome::Inject { context } => side.injected.push(context),
                HookOutcome::ScopeTools {
                    disable,
                    restore_at,
                } => {
                    for name in disable {
                        side.scoped_disables.push((name, restore_at));
                    }
                }
                _ => {}
            }
        }
        PreToolUseVerdict { deny, side }
    }

    /// `PostToolUse`: observers run; every `Inject` context is collected to be
    /// appended as hidden user messages on the next turn.
    pub async fn run_post_tool_use(
        &self,
        tool_name: &str,
        tool_output: &str,
        duration_ms: u64,
        session_id: &str,
        cwd: Option<&Path>,
    ) -> Vec<String> {
        if self.hooks.is_empty() {
            return Vec::new();
        }
        let ctx = HookContext {
            session_id: session_id.to_string(),
            cwd: cwd.map(Path::to_path_buf),
            event: HookEvent::PostToolUse {
                tool_name: tool_name.to_string(),
                tool_output: tool_output.to_string(),
                duration_ms,
            },
        };
        self.fire(HookEventKind::PostToolUse, Some(tool_name), &ctx)
            .await
            .into_iter()
            .filter_map(|o| match o {
                HookOutcome::Inject { context } => Some(context),
                _ => None,
            })
            .collect()
    }

    /// `PostToolUseFailure`: observers run after a failed call; `Inject`
    /// contexts are collected. Same shape as [`Self::run_post_tool_use`] under
    /// a different event kind, so a hook can target only failures.
    pub async fn run_post_tool_use_failure(
        &self,
        tool_name: &str,
        error: &str,
        session_id: &str,
        cwd: Option<&Path>,
    ) -> Vec<String> {
        if self.hooks.is_empty() {
            return Vec::new();
        }
        let ctx = HookContext {
            session_id: session_id.to_string(),
            cwd: cwd.map(Path::to_path_buf),
            event: HookEvent::PostToolUseFailure {
                tool_name: tool_name.to_string(),
                error: error.to_string(),
            },
        };
        self.fire(HookEventKind::PostToolUseFailure, Some(tool_name), &ctx)
            .await
            .into_iter()
            .filter_map(|o| match o {
                HookOutcome::Inject { context } => Some(context),
                _ => None,
            })
            .collect()
    }

    /// `Stop`: the first `Deny` reason forces another turn (feeding the reason
    /// back to the model). `None` lets the round end. Mirrors `/pursue`'s gate;
    /// the two compose (stop requires both to agree).
    pub async fn check_stop(
        &self,
        last_message: &str,
        session_id: &str,
        cwd: Option<&Path>,
    ) -> Option<String> {
        if self.hooks.is_empty() {
            return None;
        }
        let ctx = HookContext {
            session_id: session_id.to_string(),
            cwd: cwd.map(Path::to_path_buf),
            event: HookEvent::Stop {
                last_message: last_message.to_string(),
            },
        };
        self.fire(HookEventKind::Stop, None, &ctx)
            .await
            .into_iter()
            .find_map(|o| match o {
                HookOutcome::Deny { reason } => Some(reason),
                HookOutcome::Inject { context } => Some(context),
                _ => None,
            })
    }

    /// `UserPromptSubmit`: a `Deny` drops the prompt; an `Inject` is prepended
    /// to the prompt as context.
    pub async fn check_user_prompt_submit(
        &self,
        prompt: &str,
        session_id: &str,
        cwd: Option<&Path>,
    ) -> UserPromptVerdict {
        if self.hooks.is_empty() {
            return UserPromptVerdict::Allow;
        }
        let ctx = HookContext {
            session_id: session_id.to_string(),
            cwd: cwd.map(Path::to_path_buf),
            event: HookEvent::UserPromptSubmit {
                prompt: prompt.to_string(),
            },
        };
        let mut denied = None;
        let mut injected = Vec::new();
        for outcome in self.fire(HookEventKind::UserPromptSubmit, None, &ctx).await {
            match outcome {
                HookOutcome::Deny { reason } if denied.is_none() => denied = Some(reason),
                HookOutcome::Inject { context } => injected.push(context),
                _ => {}
            }
        }
        match denied {
            Some(reason) => UserPromptVerdict::Deny(reason),
            None if injected.is_empty() => UserPromptVerdict::Allow,
            None => UserPromptVerdict::Prepend(injected.join("\n\n")),
        }
    }

    /// `PreCompact`: observers may inject extra context folded into the next
    /// summarization. Run before a compaction.
    pub async fn pre_compact(&self, session_id: &str, cwd: Option<&Path>) -> Vec<String> {
        if self.hooks.is_empty() {
            return Vec::new();
        }
        let ctx = HookContext {
            session_id: session_id.to_string(),
            cwd: cwd.map(Path::to_path_buf),
            event: HookEvent::PreCompact,
        };
        self.fire(HookEventKind::PreCompact, None, &ctx)
            .await
            .into_iter()
            .filter_map(|o| match o {
                HookOutcome::Inject { context } => Some(context),
                _ => None,
            })
            .collect()
    }

    /// `PostCompact`: observers fire after a compaction completes. Best-effort.
    pub async fn post_compact(&self, session_id: &str, cwd: Option<&Path>) {
        if self.hooks.is_empty() {
            return;
        }
        let ctx = HookContext {
            session_id: session_id.to_string(),
            cwd: cwd.map(Path::to_path_buf),
            event: HookEvent::PostCompact,
        };
        let _ = self.fire(HookEventKind::PostCompact, None, &ctx).await;
    }

    /// `SessionStart`: observers fire; their `Inject` contexts become hidden
    /// setup messages. Best-effort — failures are logged, not fatal.
    pub async fn session_start(
        &self,
        source: HookStartSource,
        session_id: &str,
        cwd: Option<&Path>,
        messages: &mut Vec<Message>,
    ) {
        if self.hooks.is_empty() {
            return;
        }
        let ctx = HookContext {
            session_id: session_id.to_string(),
            cwd: cwd.map(Path::to_path_buf),
            event: HookEvent::SessionStart { source },
        };
        for outcome in self.fire(HookEventKind::SessionStart, None, &ctx).await {
            if let HookOutcome::Inject { context } = outcome {
                messages.push(crate::runtime::message::hidden_user(
                    InjectionKind::Hook(HookEventKind::SessionStart),
                    context,
                ));
            }
        }
    }

    /// `SessionEnd`: observers fire. Best-effort.
    pub async fn session_end(&self, session_id: &str, cwd: Option<&Path>) {
        if self.hooks.is_empty() {
            return;
        }
        let ctx = HookContext {
            session_id: session_id.to_string(),
            cwd: cwd.map(Path::to_path_buf),
            event: HookEvent::SessionEnd,
        };
        let _ = self.fire(HookEventKind::SessionEnd, None, &ctx).await;
    }

    /// `Turn` (ADR-0030): fires after each non-terminal ReAct turn, before the
    /// next model request. Every `Inject` context is collected as hidden user
    /// messages for that request. `Deny` is **ignored** by contract — a
    /// turn-count hook cannot abort the round (the ADR-0009 concern).
    /// `ScopeTools` disables are gathered for the agent to apply.
    /// `consecutive_readonly` carries the read-only streak.
    pub async fn run_turn(
        &self,
        round: u64,
        turn: usize,
        consecutive_readonly: u32,
        session_id: &str,
        cwd: Option<&Path>,
    ) -> HookSideEffects {
        if self.hooks.is_empty() {
            return HookSideEffects::default();
        }
        let ctx = HookContext {
            session_id: session_id.to_string(),
            cwd: cwd.map(Path::to_path_buf),
            event: HookEvent::Turn {
                round,
                turn,
                consecutive_readonly,
            },
        };
        HookSideEffects::from_outcomes(self.fire(HookEventKind::Turn, None, &ctx).await)
    }

    /// `TurnStart`: symmetric partner of
    /// [`Self::run_turn`], fired at the start of each ReAct turn — after tools
    /// are prepared but before the next model completion. Every `Inject`
    /// context is collected as hidden user messages at the top of the model's
    /// attention for this turn, and `ScopeTools` disables are gathered. `Deny`
    /// is **ignored** by contract (same constraint as `Turn`).
    pub async fn run_turn_start(
        &self,
        round: u64,
        turn: usize,
        consecutive_readonly: u32,
        session_id: &str,
        cwd: Option<&Path>,
    ) -> HookSideEffects {
        if self.hooks.is_empty() {
            return HookSideEffects::default();
        }
        let ctx = HookContext {
            session_id: session_id.to_string(),
            cwd: cwd.map(Path::to_path_buf),
            event: HookEvent::TurnStart {
                round,
                turn,
                consecutive_readonly,
            },
        };
        HookSideEffects::from_outcomes(self.fire(HookEventKind::TurnStart, None, &ctx).await)
    }

    /// `PermissionRequest`: observe-only. The agent is about to block waiting
    /// for a permission decision; matching hooks fire (the typical use is a
    /// fire-and-forget desktop notification so the user notices the agent is
    /// blocked), but their outcomes are ignored — neither `Deny` nor `Inject`
    /// can gate the prompt or alter the transcript. The tool-name matcher
    /// targets the tool seeking approval.
    pub async fn run_permission_request(
        &self,
        request: &PermissionRequest,
        session_id: &str,
        cwd: Option<&Path>,
    ) {
        if self.hooks.is_empty() {
            return;
        }
        let ctx = HookContext {
            session_id: session_id.to_string(),
            cwd: cwd.map(Path::to_path_buf),
            event: HookEvent::PermissionRequest {
                request: request.clone(),
            },
        };
        // Fire for side effects; discard outcomes by contract.
        let _ = self
            .fire(HookEventKind::PermissionRequest, Some(&request.tool), &ctx)
            .await;
    }

    /// `UserQuestion`: observe-only, same contract as
    /// [`Self::run_permission_request`]. Fires when the agent is about to block
    /// on an `ask_user` question. Does not honour a matcher.
    pub async fn run_user_question(
        &self,
        request: &UserQuestionRequest,
        session_id: &str,
        cwd: Option<&Path>,
    ) {
        if self.hooks.is_empty() {
            return;
        }
        let ctx = HookContext {
            session_id: session_id.to_string(),
            cwd: cwd.map(Path::to_path_buf),
            event: HookEvent::UserQuestion {
                request: request.clone(),
            },
        };
        let _ = self.fire(HookEventKind::UserQuestion, None, &ctx).await;
    }
}

/// The decision a `UserPromptSubmit` hook produces.
pub enum UserPromptVerdict {
    /// Proceed with the prompt unchanged.
    Allow,
    /// Drop the prompt; `reason` is surfaced to the user.
    Deny(String),
    /// Proceed, prepending `context` to the prompt.
    Prepend(String),
}

#[cfg(test)]
mod tests;
