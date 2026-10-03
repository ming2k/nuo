//! The hook registry and matcher (ADR-0025).
//!
//! [`HookRegistry`] holds the `Vec<Arc<dyn Hook>>` installed on the [`crate::Agent`]
//! (mirroring the `reviews` list at `agent.rs`) and offers one typed query per
//! lifecycle event. Each query filters by event kind and tool-name matcher, fires
//! the matching hooks, and interprets the outcomes the way that event's
//! insertion point needs — so the loop calls a one-liner (`check_pre_tool_use`,
//! `run_post_tool_use`, `check_stop`, …) instead of reimplementing dispatch.
//!
//! The [`Hook`] trait itself and the payload types live in `nuo_wire`; the
//! matcher (which needs `regex`) stays here so core stays regex-free.

use std::path::Path;
use std::sync::Arc;

use nuo_wire::{
    Hook, HookContext, HookEvent, HookEventKind, HookOutcome, InjectionKind, Message,
    PermissionRequest, RestorePoint, SessionSource, UserQuestionRequest,
};

/// Read-only permission check used before a hook with runtime side effects is
/// fired. The closure receives the hook's structured hazard submission.
pub type HookAuthorizer =
    Arc<dyn Fn(&nuo_wire::ToolPermissionSubmission) -> bool + Send + Sync>;

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

/// Side effects a hook run wants applied beyond its primary outcome: hidden
/// context to inject, and tools to temporarily scope-disable (each with its
/// restore point). Extracted from a batch of [`HookOutcome`]s so each
/// lifecycle insertion point can apply them uniformly.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HookSideEffects {
    /// `Inject` contexts, in firing order, to fold into the transcript.
    pub injected: Vec<String>,
    /// `ScopeTools` disables: `(tool_name, restore_point)` pairs, in firing
    /// order. The caller (the agent) applies these to its scoped mask.
    pub scoped_disables: Vec<(String, RestorePoint)>,
}

impl HookSideEffects {
    /// Fold a batch of outcomes into side effects, dropping anything the
    /// firing event does not honour (the caller has already handled `Deny`
    /// upstream where relevant).
    fn from_outcomes(outcomes: Vec<HookOutcome>) -> Self {
        let mut se = Self::default();
        for o in outcomes {
            match o {
                HookOutcome::Inject { context } => se.injected.push(context),
                HookOutcome::ScopeTools {
                    disable,
                    restore_at,
                } => {
                    for name in disable {
                        se.scoped_disables.push((name, restore_at));
                    }
                }
                _ => {}
            }
        }
        se
    }
}

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
        source: SessionSource,
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
                messages.push(crate::conversation_context::hidden_user(
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

/// The decision a `PreToolUse` hook produces. The first `Deny` blocks the call;
/// any `Inject` / `ScopeTools` side effects are still applied.
#[derive(Default)]
pub struct PreToolUseVerdict {
    /// First deny reason, if any. `None` lets the call proceed.
    pub deny: Option<String>,
    /// Side effects (`Inject` context, scoped disables) gathered from all
    /// matching hooks, applied by the agent regardless of deny.
    pub side: HookSideEffects,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher_pipe_list_is_exact() {
        assert!(matcher_matches("Write|Edit", "Write"));
        assert!(matcher_matches("Write|Edit", "Edit"));
        assert!(!matcher_matches("Write|Edit", "Bash"));
    }

    #[test]
    fn matcher_regex_when_special_char() {
        assert!(matcher_matches("^Bash.*", "Bash"));
        assert!(matcher_matches("mcp__.*", "mcp__memory__create"));
        assert!(!matcher_matches("mcp__.*", "Write"));
    }

    #[test]
    fn matcher_invalid_regex_matches_nothing() {
        assert!(!matcher_matches("[invalid", "anything"));
    }

    /// Stub hook that returns a fixed outcome regardless of context, but only
    /// for one declared [`HookEventKind`]. Lets a test assert routing + outcome
    /// handling without spinning up a real shell command.
    struct StubHook {
        kind: HookEventKind,
        outcome: HookOutcome,
    }

    #[async_trait::async_trait]
    impl Hook for StubHook {
        fn kind(&self) -> HookEventKind {
            self.kind
        }
        async fn fire(&self, _ctx: &HookContext) -> HookOutcome {
            self.outcome.clone()
        }
    }

    fn registry_of(hooks: Vec<Arc<dyn Hook>>) -> HookRegistry {
        HookRegistry::new(hooks)
    }

    struct HazardHook {
        fires: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Hook for HazardHook {
        fn kind(&self) -> HookEventKind {
            HookEventKind::TurnStart
        }

        fn permission_submission(
            &self,
            _ctx: &HookContext,
        ) -> Option<nuo_wire::ToolPermissionSubmission> {
            Some(nuo_wire::ToolPermissionSubmission {
                hazard_level: nuo_wire::HazardLevel::CommandExecution,
                label: "test hook".to_string(),
                description: "test command hook".to_string(),
                scope: "echo gated".to_string(),
                payload: nuo_wire::ToolPermissionPayload::Generic {
                    summary: "test hook".to_string(),
                    details: serde_json::Value::Null,
                },
            })
        }

        async fn fire(&self, _ctx: &HookContext) -> HookOutcome {
            self.fires
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            HookOutcome::Pass
        }
    }

    #[tokio::test]
    async fn side_effecting_hooks_fail_closed_without_exact_runtime_authority() {
        let fires = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hook: Arc<dyn Hook> = Arc::new(HazardHook {
            fires: fires.clone(),
        });
        HookRegistry::new(vec![hook.clone()])
            .run_turn_start(1, 0, 0, "s", None)
            .await;
        assert_eq!(fires.load(std::sync::atomic::Ordering::Relaxed), 0);

        HookRegistry::with_authorizer(
            vec![hook],
            Arc::new(|submission| submission.scope == "echo gated"),
        )
        .run_turn_start(1, 0, 0, "s", None)
        .await;
        assert_eq!(fires.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    /// `TurnStart` honours `Inject` like `Turn` does — the symmetric turn
    /// boundary collects injected context for the upcoming turn.
    #[tokio::test]
    async fn turn_start_collects_inject() {
        let reg = registry_of(vec![Arc::new(StubHook {
            kind: HookEventKind::TurnStart,
            outcome: HookOutcome::Inject {
                context: "re-anchor".to_string(),
            },
        })]);
        let side = reg.run_turn_start(1, 0, 0, "s", None).await;
        assert_eq!(side.injected, vec!["re-anchor".to_string()]);
        assert!(side.scoped_disables.is_empty());
    }

    /// `TurnStart` must discard `Deny` — a turn-start hook cannot gate the
    /// round (same ADR-0009 concern that constrains `Turn`). Only `Inject`
    /// (and `ScopeTools`) survives the filter.
    #[tokio::test]
    async fn turn_start_discards_deny() {
        let reg = registry_of(vec![
            Arc::new(StubHook {
                kind: HookEventKind::TurnStart,
                outcome: HookOutcome::Deny {
                    reason: "no".to_string(),
                },
            }),
            Arc::new(StubHook {
                kind: HookEventKind::TurnStart,
                outcome: HookOutcome::Inject {
                    context: "ok".to_string(),
                },
            }),
        ]);
        let side = reg.run_turn_start(1, 0, 0, "s", None).await;
        assert_eq!(side.injected, vec!["ok".to_string()]);
    }

    /// Routing isolation: a `Turn` hook must not fire on `TurnStart` and vice
    /// versa. Guards against a future refactor that folds both into one path
    /// and accidentally cross-triggers.
    #[tokio::test]
    async fn turn_start_is_routed_separately_from_turn() {
        let reg = registry_of(vec![Arc::new(StubHook {
            kind: HookEventKind::Turn,
            outcome: HookOutcome::Inject {
                context: "turn-leak".to_string(),
            },
        })]);
        let side = reg.run_turn_start(1, 0, 0, "s", None).await;
        assert!(
            side.injected.is_empty() && side.scoped_disables.is_empty(),
            "Turn hook must not fire on TurnStart"
        );
    }

    /// `ScopeTools` outcomes are collected by `TurnStart` (and `Turn`) so the
    /// agent can apply them to its scoped-disable mask. Regression guard that
    /// the side-effect channel actually carries scoped disables.
    #[tokio::test]
    async fn turn_start_collects_scope_tools() {
        let reg = registry_of(vec![Arc::new(StubHook {
            kind: HookEventKind::TurnStart,
            outcome: HookOutcome::ScopeTools {
                disable: vec!["execute_command".to_string(), "edit_text".to_string()],
                restore_at: RestorePoint::TurnEnd,
            },
        })]);
        let side = reg.run_turn_start(1, 0, 0, "s", None).await;
        assert!(side.injected.is_empty());
        assert_eq!(
            side.scoped_disables,
            vec![
                ("execute_command".to_string(), RestorePoint::TurnEnd),
                ("edit_text".to_string(), RestorePoint::TurnEnd),
            ]
        );
    }

    /// A recording hook: counts how many times `fire` ran, regardless of the
    /// outcome it returned. Used to prove an observe-only event actually
    /// dispatches the hook (for side effects) even though outcomes are dropped.
    struct RecordingHook {
        kind: HookEventKind,
        matcher: Option<String>,
        fires: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Hook for RecordingHook {
        fn kind(&self) -> HookEventKind {
            self.kind
        }
        fn matcher(&self) -> Option<&str> {
            self.matcher.as_deref()
        }
        async fn fire(&self, _ctx: &HookContext) -> HookOutcome {
            self.fires
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            HookOutcome::Pass
        }
    }

    /// `PermissionRequest` is observe-only: the hook fires (for its side effect,
    /// e.g. a notification) but a `Deny` outcome must NOT surface — a
    /// notification hook can never grant/deny the prompt. We assert fire-count
    /// rose (dispatch happened) and that the method returns `()` (no outcome to
    /// act on).
    #[tokio::test]
    async fn permission_request_is_observe_only_but_fires() {
        let fires = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reg = registry_of(vec![Arc::new(RecordingHook {
            kind: HookEventKind::PermissionRequest,
            matcher: None,
            fires: fires.clone(),
        })]);
        let request = nuo_wire::PermissionRequest {
            id: "permission_x".into(),
            tool: "execute_command".into(),
            label: "Run command".into(),
            description: "rm -rf /tmp/x".into(),
            arguments: "{}".into(),
            scope: "command".into(),
            elevation: false,
            one_off: false,
            origin: None,
            ..Default::default()
        };
        reg.run_permission_request(&request, "s", None).await;
        assert_eq!(
            fires.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "PermissionRequest hook must fire for its side effect"
        );
    }

    /// The `PermissionRequest` matcher targets the tool seeking approval, so a
    /// `execute_command`-only notification hook does not fire for an `edit_text` request.
    #[tokio::test]
    async fn permission_request_matcher_targets_tool() {
        let fires = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reg = registry_of(vec![Arc::new(RecordingHook {
            kind: HookEventKind::PermissionRequest,
            matcher: Some("execute_command".into()),
            fires: fires.clone(),
        })]);
        let bash_req = nuo_wire::PermissionRequest {
            id: "p1".into(),
            tool: "execute_command".into(),
            label: "".into(),
            description: "".into(),
            arguments: "".into(),
            scope: "command".into(),
            elevation: false,
            one_off: false,
            origin: None,
            ..Default::default()
        };
        let edit_req = nuo_wire::PermissionRequest {
            id: "p2".into(),
            tool: "edit_text".into(),
            label: "".into(),
            description: "".into(),
            arguments: "".into(),
            scope: "path".into(),
            elevation: false,
            one_off: false,
            origin: None,
            ..Default::default()
        };
        reg.run_permission_request(&bash_req, "s", None).await;
        reg.run_permission_request(&edit_req, "s", None).await;
        assert_eq!(
            fires.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "only the bash request should match"
        );
    }

    /// `UserQuestion` is observe-only and matcher-less: it fires once per
    /// ask_user block, irrespective of content.
    #[tokio::test]
    async fn user_question_is_observe_only_but_fires() {
        let fires = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reg = registry_of(vec![Arc::new(RecordingHook {
            kind: HookEventKind::UserQuestion,
            matcher: None,
            fires: fires.clone(),
        })]);
        let request = nuo_wire::UserQuestionRequest {
            id: "ask_user_x".into(),
            questions: vec![nuo_wire::UserQuestion {
                header: Some("Pick one".into()),
                question: "Which?".into(),
                options: vec![nuo_wire::UserQuestionOption {
                    label: "A".into(),
                    description: None,
                }],
                multi_select: false,
            }],
            origin: None,
        };
        reg.run_user_question(&request, "s", None).await;
        assert_eq!(
            fires.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "UserQuestion hook must fire for its side effect"
        );
    }
}
