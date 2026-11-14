#![allow(clippy::unwrap_used)]
use super::*;
use async_trait::async_trait;
use nuo_tool::ToolAccesses;
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
    let granted = base.join("nuo-policy-granted");
    let inside = granted.join("notes.md");
    let outside = base.join("nuo-policy-outside").join("secret.txt");
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
        name: "read".into(),
        target: ScopeTarget::Unspecified,
    });
    let disabled = HashSet::new();
    let scoped = ScopedToolDisable::default();
    let ctxr = StubCtx {
        perms: PermissionStore::new(),
    };
    let c = pctx(TestPolicyParams {
        tool: &tool,
        name: "read",
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
    fn hazard_level(&self) -> nuo_tool::HazardLevel {
        nuo_tool::HazardLevel::Safe
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
    fn hazard_level(&self) -> nuo_tool::HazardLevel {
        nuo_tool::HazardLevel::CommandExecution
    }
    fn scope_target(&self, args: &str) -> ScopeTarget {
        ScopeTarget::Command(args.to_string())
    }
    fn permission_submission(&self, args: &str) -> Option<nuo_tool::ToolPermissionSubmission> {
        Some(nuo_tool::ToolPermissionSubmission {
            hazard_level: nuo_tool::HazardLevel::CommandExecution,
            label: format!("Execute command: `{args}`"),
            description: format!("Runs host shell command `{args}`"),
            scope: args.to_string(),
            payload: nuo_tool::ToolPermissionPayload::Command {
                command: args.to_string(),
                cwd: None,
                kill_spec: nuo_tool::ProcessKillSpec {
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
                Some(nuo_tool::HazardLevel::CommandExecution)
            );
            assert!(request.submission.is_some());
            let sub = request.submission.unwrap();
            match sub.payload {
                nuo_tool::ToolPermissionPayload::Command { kill_spec, .. } => {
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
