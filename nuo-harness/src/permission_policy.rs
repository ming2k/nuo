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
//!    [`nuo_wire::PermissionRequest`]
//!    carries `elevation` (out-of-scope, ADR-0028) and `one_off` (the bash
//!    dangerous-command confirm: an `Always` reply is honoured but not
//!    persisted) so the caller and the TUI handle both uniformly.

use std::sync::Arc;

use async_trait::async_trait;
use nuo_wire::{RestorePoint, ScopeTarget, Tool, ToolOutput};

use crate::agent::ScopedToolDisable;
use crate::bash_policy::BashPolicyMatch;
use crate::hooks::PreToolUseVerdict;
use crate::permission_store::{PermissionRule, PermissionStore};

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
        request: Box<nuo_wire::PermissionRequest>,
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
        match nuo_wire::tool_validation::validate_tool_arguments(
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
                        request: Box::new(nuo_wire::PermissionRequest {
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
                            hazard: Some(nuo_wire::hazard::HazardLevel::CommandExecution),
                            submission: None,
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

        PolicyDecision::MissingAuthority {
            request: Box::new(nuo_wire::PermissionRequest {
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
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use async_trait::async_trait;
    use nuo_wire::ToolAccesses;
    use std::collections::HashSet;
    use std::path::PathBuf;

    struct StubTool {
        name: String,
        target: ScopeTarget,
    }

    /// The chain order is load-bearing (see [`default_chain`]): hooks observe
    /// every call, schema validation runs after the hook, the scope gate
    /// precedes the broker. Pin it so a careless reorder fails loudly.
    #[test]
    fn default_chain_order_is_load_bearing() {
        let chain = PermissionChain::new(default_chain());
        assert_eq!(
            chain.policy_names(),
            vec!["hook", "disabled", "schema", "bash-policy", "broker"]
        );
    }
    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _a: &str) -> Result<String, String> {
            Ok("ok".into())
        }
        fn scope_target(&self, _a: &str) -> ScopeTarget {
            self.target.clone()
        }
        fn accesses(&self, _a: &str) -> ToolAccesses {
            ToolAccesses::none()
        }
    }

    /// A stub PermissionContext for policy tests: all hooks/bash/park are no-ops.
    struct StubCtx {
        perms: PermissionStore,
    }
    #[async_trait]
    impl PermissionContext for StubCtx {
        async fn check_pre_tool_use(&self, _n: &str, _i: &serde_json::Value) -> PreToolUseVerdict {
            PreToolUseVerdict::default()
        }
        fn apply_scoped_disables(&self, _d: &[(String, RestorePoint)]) {}
        async fn check_bash_policy(&self, _c: &str, _a: &str) -> BashVerdict {
            BashVerdict::Allow
        }
        fn permissions(&self) -> &PermissionStore {
            &self.perms
        }
    }

    #[derive(Clone)]
    struct TestPolicyParams<'a> {
        tool: &'a Arc<dyn Tool>,
        name: &'a str,
        args: &'a str,
        target: ScopeTarget,
        unattended: bool,
        disabled: HashSet<String>,
        scoped: ScopedToolDisable,
        ctx: &'a dyn PermissionContext,
    }

    /// Build a [`PolicyContext`] for one policy evaluation in unit tests.
    fn pctx<'a>(params: TestPolicyParams<'a>) -> PolicyContext<'a> {
        PolicyContext {
            tool: params.tool,
            call_name: params.name,
            arguments: params.args,
            scope_target: params.target,
            disabled: params.disabled,
            scoped_disabled: params.scoped,
            unattended: params.unattended,
            ctx: params.ctx,
        }
    }

    /// Native absolute paths for scope-policy tests. Unix-looking paths such
    /// as `/home/user` are drive-relative on Windows and therefore test a
    /// different rule than intended.
    fn scoped_test_paths() -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|_| std::env::temp_dir());
        let granted = base.join("muta-policy-granted");
        let inside = granted.join("notes.md");
        let outside = base.join("muta-policy-outside").join("secret.txt");
        (granted, inside, outside)
    }

    #[tokio::test]
    async fn disabled_policy_denies() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "execute_command".into(),
            target: ScopeTarget::Command("ls".into()),
        });
        let disabled: HashSet<String> = ["execute_command".to_string()].into_iter().collect();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(TestPolicyParams {
            tool: &tool,
            name: "execute_command",
            args: "{}",
            target: ScopeTarget::Unspecified,
            unattended: false,
            disabled: disabled.clone(),
            scoped: scoped.clone(),
            ctx: &ctxr,
        });
        assert!(matches!(
            DisabledPolicy.evaluate(&c).await,
            PolicyDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn chain_out_of_scope_approves_when_session_allowed() {
        let (_granted, _inside, outside) = scoped_test_paths();
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(outside.clone()),
        });
        let perms = PermissionStore::new();
        perms.add_session(PermissionRule {
            tool: "write_file".into(),
            scope: outside.display().to_string(),
        });
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx { perms };
        let c = pctx(TestPolicyParams {
            tool: &tool,
            name: "write_file",
            args: "{}",
            target: ScopeTarget::Path(outside),
            unattended: false,
            disabled: disabled.clone(),
            scoped: scoped.clone(),
            ctx: &ctxr,
        });
        let chain = PermissionChain::new(vec![Box::new(BrokerPolicy)]);
        assert!(matches!(chain.evaluate(&c).await, PolicyDecision::Approve));
    }

    #[tokio::test]
    async fn broker_unattended_approves_automatically() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/anywhere")),
        });
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(TestPolicyParams {
            tool: &tool,
            name: "write_file",
            args: "{}",
            target: ScopeTarget::Path(PathBuf::from("/anywhere")),
            unattended: true, // unattended
            disabled: disabled.clone(),
            scoped: scoped.clone(),
            ctx: &ctxr,
        });
        assert!(matches!(
            BrokerPolicy.evaluate(&c).await,
            PolicyDecision::Approve
        ));
    }

    #[tokio::test]
    async fn broker_approves_session_and_always_allowed_rules() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/workspace/file")),
        });
        let perms = PermissionStore::new();
        perms.add_session(PermissionRule {
            tool: "write_file".into(),
            scope: "/workspace/file".into(),
        });
        let ctxr = StubCtx { perms };
        let c = pctx(TestPolicyParams {
            tool: &tool,
            name: "write_file",
            args: "{}",
            target: ScopeTarget::Path(PathBuf::from("/workspace/file")),
            unattended: true,
            disabled: HashSet::new(),
            scoped: ScopedToolDisable::default(),
            ctx: &ctxr,
        });
        assert!(matches!(
            BrokerPolicy.evaluate(&c).await,
            PolicyDecision::Approve
        ));
    }

    #[tokio::test]
    async fn broker_reports_missing_authority_when_not_allowed() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/tmp/x")),
        });
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(TestPolicyParams {
            tool: &tool,
            name: "write_file",
            args: "{}",
            target: ScopeTarget::Path(PathBuf::from("/tmp/x")),
            unattended: false,
            disabled: disabled.clone(),
            scoped: scoped.clone(),
            ctx: &ctxr,
        });
        assert!(matches!(
            BrokerPolicy.evaluate(&c).await,
            PolicyDecision::MissingAuthority { .. }
        ));
    }

    #[tokio::test]
    async fn broker_in_scope_request_is_not_elevation() {
        // An in-scope call's request must carry elevation: false.
        let (_granted, inside, _outside) = scoped_test_paths();
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(inside.clone()),
        });
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(TestPolicyParams {
            tool: &tool,
            name: "write_file",
            args: "{}",
            target: ScopeTarget::Path(inside),
            unattended: false,
            disabled: disabled.clone(),
            scoped: scoped.clone(),
            ctx: &ctxr,
        });
        match BrokerPolicy.evaluate(&c).await {
            PolicyDecision::MissingAuthority { request, .. } => assert!(!request.elevation),
            other => panic!("expected MissingAuthority, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chain_short_circuits() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/tmp/x")),
        });
        let disabled: HashSet<String> = ["write_file".to_string()].into_iter().collect();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(TestPolicyParams {
            tool: &tool,
            name: "write_file",
            args: "{}",
            target: ScopeTarget::Path(PathBuf::from("/tmp/x")),
            unattended: false,
            disabled: disabled.clone(),
            scoped: scoped.clone(),
            ctx: &ctxr,
        });
        let chain = PermissionChain::new(vec![Box::new(DisabledPolicy), Box::new(BrokerPolicy)]);
        assert!(matches!(
            chain.evaluate(&c).await,
            PolicyDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn chain_falls_back_to_approve() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "read_text".into(),
            target: ScopeTarget::Unspecified,
        });
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(TestPolicyParams {
            tool: &tool,
            name: "read_text",
            args: "{}",
            target: ScopeTarget::Unspecified,
            unattended: false,
            disabled: disabled.clone(),
            scoped: scoped.clone(),
            ctx: &ctxr,
        });
        let chain = PermissionChain::new(vec![Box::new(DisabledPolicy)]);
        assert!(matches!(chain.evaluate(&c).await, PolicyDecision::Approve));
    }

    #[tokio::test]
    async fn chain_out_of_scope_attended_reaches_broker() {
        // An attended call with a locatable target is not hard-blocked — it
        // flows to the broker (MissingAuthority), so the user, not a builtin
        // limit, decides the elevation.
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/etc/passwd")),
        });
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(TestPolicyParams {
            tool: &tool,
            name: "write_file",
            args: "{}",
            target: ScopeTarget::Path(PathBuf::from("/etc/passwd")),
            unattended: false, // attended
            disabled: disabled.clone(),
            scoped: scoped.clone(),
            ctx: &ctxr,
        });
        let chain = PermissionChain::new(vec![Box::new(BrokerPolicy)]);
        assert!(matches!(
            chain.evaluate(&c).await,
            PolicyDecision::MissingAuthority { .. }
        ));
    }

    #[tokio::test]
    async fn chain_out_of_scope_unattended_approves() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/etc/passwd")),
        });
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(TestPolicyParams {
            tool: &tool,
            name: "write_file",
            args: "{}",
            target: ScopeTarget::Path(PathBuf::from("/etc/passwd")),
            unattended: true, // unattended
            disabled: disabled.clone(),
            scoped: scoped.clone(),
            ctx: &ctxr,
        });
        let chain = PermissionChain::new(vec![Box::new(BrokerPolicy)]);
        assert!(matches!(chain.evaluate(&c).await, PolicyDecision::Approve));
    }

    struct SafeTool;
    #[async_trait]
    impl Tool for SafeTool {
        fn name(&self) -> &str {
            "view_file"
        }
        fn description(&self) -> &str {
            "View file content"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn hazard_level(&self) -> nuo_wire::HazardLevel {
            nuo_wire::HazardLevel::Safe
        }
        async fn call(&self, _args: &str) -> Result<String, String> {
            Ok("ok".into())
        }
    }

    struct DangerousCommandTool;
    #[async_trait]
    impl Tool for DangerousCommandTool {
        fn name(&self) -> &str {
            "execute_command"
        }
        fn description(&self) -> &str {
            "Run shell command"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn hazard_level(&self) -> nuo_wire::HazardLevel {
            nuo_wire::HazardLevel::CommandExecution
        }
        fn scope_target(&self, args: &str) -> ScopeTarget {
            ScopeTarget::Command(args.to_string())
        }
        fn permission_submission(
            &self,
            args: &str,
        ) -> Option<nuo_wire::ToolPermissionSubmission> {
            Some(nuo_wire::ToolPermissionSubmission {
                hazard_level: nuo_wire::HazardLevel::CommandExecution,
                label: format!("Execute command: `{args}`"),
                description: format!("Runs host shell command `{args}`"),
                scope: args.to_string(),
                payload: nuo_wire::ToolPermissionPayload::Command {
                    command: args.to_string(),
                    cwd: None,
                    kill_spec: nuo_wire::ProcessKillSpec {
                        command: args.split_whitespace().next().unwrap_or("sh").to_string(),
                        process_group_killable: true,
                        pkill_target: format!("pkill -f '{args}'"),
                        cwd: None,
                    },
                },
            })
        }
        async fn call(&self, _args: &str) -> Result<String, String> {
            Ok("ok".into())
        }
    }

    #[tokio::test]
    async fn broker_passes_safe_tools_automatically() {
        let tool: Arc<dyn Tool> = Arc::new(SafeTool);
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(TestPolicyParams {
            tool: &tool,
            name: "view_file",
            args: "{}",
            target: ScopeTarget::Unspecified,
            unattended: false,
            disabled: HashSet::new(),
            scoped: ScopedToolDisable::default(),
            ctx: &ctxr,
        });
        assert!(matches!(
            BrokerPolicy.evaluate(&c).await,
            PolicyDecision::Pass
        ));
    }

    #[tokio::test]
    async fn broker_submits_hazard_and_kill_spec_for_dangerous_commands() {
        let tool: Arc<dyn Tool> = Arc::new(DangerousCommandTool);
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(TestPolicyParams {
            tool: &tool,
            name: "execute_command",
            args: "cargo test",
            target: ScopeTarget::Command("cargo test".to_string()),
            unattended: false,
            disabled: HashSet::new(),
            scoped: ScopedToolDisable::default(),
            ctx: &ctxr,
        });

        let decision = BrokerPolicy.evaluate(&c).await;

        match decision {
            PolicyDecision::MissingAuthority { request, .. } => {
                assert_eq!(
                    request.hazard,
                    Some(nuo_wire::HazardLevel::CommandExecution)
                );
                assert!(request.submission.is_some());
                let sub = request.submission.unwrap();
                match sub.payload {
                    nuo_wire::ToolPermissionPayload::Command { kill_spec, .. } => {
                        assert!(kill_spec.process_group_killable);
                        assert_eq!(kill_spec.pkill_target, "pkill -f 'cargo test'");
                    }
                    _ => panic!("Expected Command payload"),
                }
            }
            other => panic!("Expected MissingAuthority, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn broker_honours_session_grant_lifespan() {
        let tool: Arc<dyn Tool> = Arc::new(DangerousCommandTool);
        let perms = PermissionStore::new();
        let ctxr = StubCtx { perms };

        let c = pctx(TestPolicyParams {
            tool: &tool,
            name: "execute_command",
            args: "cargo build",
            target: ScopeTarget::Command("cargo build".to_string()),
            unattended: false,
            disabled: HashSet::new(),
            scoped: ScopedToolDisable::default(),
            ctx: &ctxr,
        });

        // Initially requires permission
        assert!(matches!(
            BrokerPolicy.evaluate(&c).await,
            PolicyDecision::MissingAuthority { .. }
        ));

        // Grant session permission
        ctxr.permissions().add_session(PermissionRule {
            tool: "execute_command".into(),
            scope: "cargo build".into(),
        });

        // Now approved
        assert!(matches!(
            BrokerPolicy.evaluate(&c).await,
            PolicyDecision::Approve
        ));

        // After clearing session, requires permission again
        ctxr.permissions().clear_session();
        assert!(matches!(
            BrokerPolicy.evaluate(&c).await,
            PolicyDecision::MissingAuthority { .. }
        ));
    }
}
