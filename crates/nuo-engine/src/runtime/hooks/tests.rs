use super::*;
use nuo_protocol::RestorePoint;

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
    ) -> Option<nuo_tool::ToolPermissionSubmission> {
        Some(nuo_tool::ToolPermissionSubmission {
            hazard_level: nuo_tool::HazardLevel::CommandExecution,
            label: "test hook".to_string(),
            description: "test command hook".to_string(),
            scope: "echo gated".to_string(),
            payload: nuo_tool::ToolPermissionPayload::Generic {
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
    let request = nuo_tool::PermissionRequest {
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
    let bash_req = nuo_tool::PermissionRequest {
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
    let edit_req = nuo_tool::PermissionRequest {
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
    let request = nuo_tool::UserQuestionRequest {
        id: "ask_user_x".into(),
        questions: vec![nuo_tool::UserQuestion {
            header: Some("Pick one".into()),
            question: "Which?".into(),
            options: vec![nuo_tool::UserQuestionOption {
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
