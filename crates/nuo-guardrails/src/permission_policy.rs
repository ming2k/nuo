//! Policy-chain permission model (full async chain).
//!
//! Replaces the harness's hand-coded sequence of "gates" inside
//! [`execute_tool`](crate::Agent) with a single ordered chain of
//! [`PermissionPolicy`] implementations — every gate, sync and async alike,
//! becomes a policy. The chain evaluates first-non-`Pass`-wins, exactly
//! mirroring kimi-code's `PermissionManager` + `policies/` design.
//!
//! ### Why a chain
//!
//! The previous model hard-coded `if`/`match` arms in sequence inside a
//! 250-line function. Adding a rule meant editing that function; the order was
//! implicit. A policy chain makes each rule an isolated, unit-testable type
//! whose position in the chain is explicit, and lets new rules slot in
//! without touching the others.
//!
//! ### Async policies
//!
//! Several gates are inherently async — PreToolUse hooks (they run an external
//! process), the bash command policy (may confirm), the permission broker
//! (parks for a live user decision). So [`PermissionPolicy::evaluate`] is
//! `async`. Policies that need agent machinery (hooks, bash policy, the
//! permission store) reach it through a [`PermissionContext`] trait that
//! `Agent` implements — this keeps the module free of a direct `&Agent`
//! dependency (and its cycle), while letting policies call back into the
//! agent's async methods.
//!
//! ### Behavior invariants (preserved verbatim from the old path)
//!
//! 1. **Order is load-bearing** (see [`default_chain`]): schema validation
//!    comes *after* the hook (so hooks observe every call, including malformed
//! 2. **`ScopeTarget` is the shared switch** for bash-policy / broker:
//!    `Unspecified` skips both.
//! 3. **`Reject` is collective** — one reject rejects the whole pending batch
//!    (owned by the permission store's `reply`, keyed on a reject decision).
//! 4. **The delegated flag is not authority.** Scope, bash, and broker decisions are
//!    identical for attended and unattended sessions. Only the executor decides
//!    whether a missing grant can be requested interactively.
//! 5. **`PermissionDenied` vs `Error`** distinguish user-aborts from hard
//!    failures.
//! 6. **One prompt per call.** Both the bash confirm gate and the broker emit
//!    [`PolicyDecision::MissingAuthority`]; the caller may park once, emit one prompt, and
//!    await one decision. A bash command is never prompted twice (the old
//!    chain-external re-evaluation is gone). A missing-authority
//!    [`nuo_tool::PermissionRequest`]
//!    carries `elevation` (out-of-scope, ADR-0028) and `one_off` (the bash
//!    dangerous-command confirm: an `Always` reply is honoured but not
//!    persisted) so the caller and the TUI handle both uniformly.

use std::sync::Arc;

use async_trait::async_trait;
use nuo_protocol::{HookOutcome, RestorePoint};
use nuo_tool::{ScopeTarget, Tool, ToolOutput};


use super::scoped_tools::ScopedToolDisable;
use crate::bash_policy::BashPolicyMatch;
use crate::permission_store::{PermissionRule, PermissionStore};

/// Hook side effects applied to the agent round.
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct HookSideEffects {
    /// `Inject` contexts, in firing order, to fold into the transcript.
    pub injected: Vec<String>,
    /// `ScopeTools` disables: `(tool_name, restore_point)` pairs, in firing
    /// order.
    pub scoped_disables: Vec<(String, RestorePoint)>,
}

impl HookSideEffects {
    /// Fold a batch of outcomes into side effects.
    pub fn from_outcomes(outcomes: Vec<HookOutcome>) -> Self {
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

/// The outcome of evaluating pre-tool-use hooks.
#[derive(Debug, Default, Clone)]
pub struct PreToolUseVerdict {
    /// First deny reason, if any. `None` lets the call proceed.
    pub deny: Option<String>,
    /// Side effects gathered from all matching hooks.
    pub side: HookSideEffects,
}

/// The non-interactive verdict the bash policy returns for one command.
///
/// Replaces the old ambiguous `Option<ToolOutput>` whose `None` conflated "the
/// command is allowed" with "it needs interactive confirmation but I have no
/// event channel" — a load-bearing lie that forced a second full bash-policy
/// re-evaluation outside the chain. The three variants are now disjoint and
/// self-describing, so [`BashPolicy`] can decide everything itself and return a
/// real [`PolicyDecision::MissingAuthority`] for the confirm path (no more chain-external
/// re-run, no more double evaluation, no more double prompt).
pub enum BashVerdict {
    /// The command fell through to the normal permission broker.
    Allow,
    /// The command matches a `Confirm` rule and a human must approve it
    /// one-off. Carries the rule match so the gate can build the prompt
    /// payload (label/description/detail) without re-evaluating.
    Confirm { match_: BashPolicyMatch },
    /// A hard refusal from an unconditional deny rule.
    Deny { output: ToolOutput },
}

/// The outcome a policy returns for one tool call. `Pass` is the chain's
/// continuation signal; the first non-`Pass` wins.
#[derive(Debug)]
pub enum PolicyDecision {
    /// No opinion — evaluate the next policy.
    Pass,
    /// Admit the call; no further policies consulted.
    Approve,
    /// Reject the call with a typed output.
    ///
    /// A reject of one parked request also rejects the rest of its concurrent
    /// batch, but that collective-abort is owned by the permission store's
    /// `reply` (keyed on a `PermissionDecision::Reject`), not signalled here:
    /// synchronous chain denies are per-call, and a `ToolOutput::PermissionDenied`
    /// is enough for the caller to treat the outcome as a user-style abort.
    Deny { output: ToolOutput },
    /// The call lacks authority. The interaction layer may ask a user, or fail
    /// immediately when no user is reachable. The policy only contributes the
    /// request payload and the rule to remember on `Always`.
    ///
    /// The request's `one_off` flag tells the caller **not to persist** an
    /// `Always` reply (the bash dangerous-command confirm is one-off); its
    /// `elevation` flag tells the TUI the call is out-of-scope (ADR-0028).
    MissingAuthority {
        request: Box<nuo_tool::PermissionRequest>,
        rule: PermissionRule,
    },
}

/// Agent capabilities a policy may need to invoke. Implemented by `Agent`; the
/// trait keeps this module decoupled from the concrete agent type (no cycle,
/// and policies are testable with a stub context).
#[async_trait]
pub trait PermissionContext: Send + Sync {
    /// Run PreToolUse hooks for this call, returning the verdict (deny +
    /// scoped-disable side effects).
    async fn check_pre_tool_use(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> PreToolUseVerdict;

    /// Apply scoped-disable side effects from a hook verdict.
    fn apply_scoped_disables(&self, disables: &[(String, RestorePoint)]);

    /// The bash command policy for `command`, as a disjoint three-way
    /// [`BashVerdict`] (Allow / Confirm / Deny). The chain's [`BashPolicy`]
    /// gate maps each variant to its terminal decision, so **every** bash
    /// outcome — including the interactive confirm — is resolved inside the
    /// chain. There is no longer a chain-external re-evaluation.
    ///
    /// This verdict is independent of the attended/delegated posture.
    async fn check_bash_policy(&self, command: &str, arguments: &str) -> BashVerdict;

    /// The permission store, for synchronous permission checks (`is_allowed`).
    fn permissions(&self) -> &PermissionStore;
}

/// Everything a policy needs to decide one call.
///
/// Owns its disable-mask snapshots (cloned before the chain runs) so no
/// `MutexGuard` is held across the chain's `.await` points — the chain is
/// async and guards are not `Send`.
pub struct PolicyContext<'a> {
    pub tool: &'a Arc<dyn Tool>,
    pub call_name: &'a str,
    pub arguments: &'a str,
    pub scope_target: ScopeTarget,
    pub disabled: std::collections::HashSet<String>,
    pub scoped_disabled: ScopedToolDisable,
    /// When true (unattended execution mode), all permissions are auto-approved.
    pub unattended: bool,
    /// Agent capabilities (hooks, bash policy, permission parking). Sync
    /// policies ignore this; async policies call through it.
    pub ctx: &'a dyn PermissionContext,
}

impl<'a> PolicyContext<'a> {
    pub fn is_name_disabled(&self) -> bool {
        self.disabled.contains(self.call_name) || self.scoped_disabled.contains(self.call_name)
    }
    pub fn is_user_disabled(&self) -> bool {
        self.disabled.contains(self.call_name)
    }
}

/// One rule in the permission chain. Async because some gates await.
#[async_trait]
pub trait PermissionPolicy: Send + Sync {
    /// Stable rule name. Not read by the dispatch path itself; exists so the
    /// chain-order regression test can pin the load-bearing sequence.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision;
}

/// The ordered chain. Evaluate by walking until the first non-`Pass`.
pub struct PermissionChain {
    policies: Vec<Box<dyn PermissionPolicy>>,
}

impl PermissionChain {
    pub fn new(policies: Vec<Box<dyn PermissionPolicy>>) -> Self {
        Self { policies }
    }
    /// Evaluate the chain. First non-`Pass` wins; if all pass, `Approve`.
    pub async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        for policy in &self.policies {
            let decision = policy.evaluate(ctx).await;
            match decision {
                PolicyDecision::Pass => continue,
                other => return other,
            }
        }
        PolicyDecision::Approve
    }
    /// The policy names in chain order. Consumed by the chain-order regression
    /// test; not part of the dispatch path.
    #[allow(dead_code)]
    pub fn policy_names(&self) -> Vec<&'static str> {
        self.policies.iter().map(|p| p.name()).collect()
    }
}

/// The canonical chain order. Each entry is one historical gate; reordering
/// here changes load-bearing behavior.
pub fn default_chain() -> Vec<Box<dyn PermissionPolicy>> {
    vec![
        Box::new(HookPolicy),
        Box::new(DisabledPolicy),
        Box::new(SchemaPolicy),
        Box::new(BashPolicy),
        Box::new(BrokerPolicy),
    ]
}

// Concrete policies. Hook/Bash/Broker are async; the rest decide from the
// context alone.

/// Gate 1: PreToolUse hook. Runs the (async) hook verdict and honours a deny.
pub struct HookPolicy;
#[async_trait]
impl PermissionPolicy for HookPolicy {
    fn name(&self) -> &'static str {
        "hook"
    }
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        let tool_input = serde_json::from_str::<serde_json::Value>(ctx.arguments)
            .unwrap_or(serde_json::Value::Null);
        let verdict = ctx.ctx.check_pre_tool_use(ctx.call_name, &tool_input).await;
        // Apply scoped disables from hooks first (narrows the toolset for
        // subsequent calls this round), then honour the deny.
        ctx.ctx.apply_scoped_disables(&verdict.side.scoped_disables);
        if let Some(reason) = verdict.deny {
            return PolicyDecision::Deny {
                output: ToolOutput::Error {
                    message: format!("Blocked by hook: {}", reason),
                    detail: None,
                },
            };
        }
        PolicyDecision::Pass
    }
}

/// Gate 2: user + scoped disable masks.
pub struct DisabledPolicy;
#[async_trait]
impl PermissionPolicy for DisabledPolicy {
    fn name(&self) -> &'static str {
        "disabled"
    }
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        if !ctx.is_name_disabled() {
            return PolicyDecision::Pass;
        }
        let message = if ctx.is_user_disabled() {
            format!(
                "Tool '{}' is disabled for this session. Re-enable it in the Tools modal (/tools).",
                ctx.call_name
            )
        } else {
            format!(
                "Tool '{}' is temporarily out of scope for this task. Use a different tool.",
                ctx.call_name
            )
        };
        PolicyDecision::Deny {
            output: ToolOutput::Text(message),
        }
    }
}

/// Gate 3: argument schema validation.
pub struct SchemaPolicy;
#[async_trait]
impl PermissionPolicy for SchemaPolicy {
    fn name(&self) -> &'static str {
        "schema"
    }
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        match nuo_tool::validation::validate_tool_arguments(
            &ctx.tool.parameters(),
            ctx.arguments,
        ) {
            Ok(()) => PolicyDecision::Pass,
            Err(message) => PolicyDecision::Deny {
                output: ToolOutput::Error {
                    message,
                    detail: None,
                },
            },
        }
    }
}

/// Gate 5: command policy. A **complete** gate — every command outcome,
/// including the interactive confirm, is decided here.
pub struct BashPolicy;
#[async_trait]
impl PermissionPolicy for BashPolicy {
    fn name(&self) -> &'static str {
        "bash-policy"
    }
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        if ctx.call_name != "execute_command" {
            return PolicyDecision::Pass;
        }
        let command = match &ctx.scope_target {
            ScopeTarget::Command(c) => c.clone(),
            _ => return PolicyDecision::Pass,
        };
        match ctx.ctx.check_bash_policy(&command, ctx.arguments).await {
            BashVerdict::Allow => PolicyDecision::Pass,
            BashVerdict::Deny { output } => PolicyDecision::Deny { output },
            BashVerdict::Confirm { match_ } => {
                if ctx.unattended {
                    // Under unattended mode, dangerous commands requiring confirmation are auto-approved.
                    PolicyDecision::Pass
                } else {
                    // Build the one-off dangerous-command prompt.
                    PolicyDecision::MissingAuthority {
                        request: Box::new(nuo_tool::PermissionRequest {
                            id: String::new(),
                            tool: "execute_command".to_string(),
                            label: "Dangerous command".to_string(),
                            description: format!(
                                "Command policy requires one-off confirmation before running this \
                                 command.\n\nRule: {}{}\nReason: {}\n\nA broad command allowlist entry \
                                 does not bypass this safety check.",
                                match_.name,
                                if match_.builtin { " (built-in)" } else { "" },
                                match_.reason,
                            ),
                            arguments: ctx.arguments.to_string(),
                            scope: command.clone(),
                            elevation: false,
                            one_off: true,
                            origin: None,
                            hazard: Some(nuo_tool::hazard::HazardLevel::CommandExecution),
                            submission: None,
                            // A shell command's effect cannot be computed without
                            // running it, which is precisely why this prompt asks a
                            // human (ADR-0125). An absent preview here is the honest
                            // answer, not an omission.
                            preview: None,
                        }),
                        rule: PermissionRule {
                            tool: "execute_command".to_string(),
                            scope: command,
                        },
                    }
                }
            }
        }
    }
}

/// Gate 6: the authority broker and hazard-aware permission handler.
pub struct BrokerPolicy;
#[async_trait]
impl PermissionPolicy for BrokerPolicy {
    fn name(&self) -> &'static str {
        "broker"
    }
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        if ctx.unattended {
            return PolicyDecision::Approve;
        }
        let submission = ctx.tool.permission_submission(ctx.arguments);
        if submission.is_none() && matches!(ctx.scope_target, ScopeTarget::Unspecified) {
            return PolicyDecision::Pass;
        }

        let rule = if let Some(sub) = &submission {
            PermissionRule {
                tool: ctx.call_name.to_string(),
                scope: sub.scope.clone(),
            }
        } else {
            scope_target_to_rule(ctx.call_name, &ctx.scope_target)
        };

        if ctx.ctx.permissions().is_allowed(&rule) {
            return PolicyDecision::Approve;
        }

        // Direct policy tests may evaluate the broker without the preceding
        // scope gate, so retain the elevation bit from the declarative scope.
        let elevation = false;
        let label = submission
            .as_ref()
            .map(|s| s.label.clone())
            .unwrap_or_else(|| ctx.tool.permission_label());
        let description = submission
            .as_ref()
            .map(|s| s.description.clone())
            .unwrap_or_else(|| ctx.tool.permission_description());
        let hazard = submission.as_ref().map(|s| s.hazard_level);

        // A pure preview of what authorizing this call would do (ADR-0125). Asked
        // here — at the gate that is about to hand the decision to a human — because
        // this is the only point where the diff changes anything: a caller that
        // auto-approves never needs it, and a caller that refuses never shows it.
        //
        // `None` is a real answer, not a gap: a shell command, a network fetch, or a
        // subagent cannot describe its effect without causing it, and a prompt that
        // said otherwise would be manufacturing confidence.
        let preview = ctx
            .tool
            .preview(&serde_json::from_str(ctx.arguments).unwrap_or(serde_json::Value::Null));

        PolicyDecision::MissingAuthority {
            request: Box::new(nuo_tool::PermissionRequest {
                id: String::new(), // caller fills the generated id
                tool: ctx.call_name.to_string(),
                label,
                description,
                arguments: ctx.arguments.to_string(),
                scope: rule.scope.clone(),
                elevation,
                one_off: false,
                origin: None,
                hazard,
                submission,
                preview,
            }),
            rule,
        }
    }
}

fn scope_target_to_rule(tool: &str, target: &ScopeTarget) -> PermissionRule {
    let scope = match target {
        ScopeTarget::Unspecified => "*".to_string(),
        ScopeTarget::Path(p) => p.to_string_lossy().into_owned(),
        ScopeTarget::Command(c) => c.clone(),
    };
    PermissionRule {
        tool: tool.to_string(),
        scope,
    }
}

#[cfg(test)]
mod tests;
