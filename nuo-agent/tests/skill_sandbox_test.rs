#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::{MockProvider, ModelResponse};
use nuo_agent::skill::{Skill, SkillStack};
use nuo_tool::{RiskProfile, Tool, ToolContext, ToolOutput, ToolScope};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct ReadOnlyInspector;
#[async_trait::async_trait]
impl Tool for ReadOnlyInspector {
    fn name(&self) -> &str {
        "read_inspection"
    }
    fn description(&self) -> &str {
        "Read-only inspection of source code"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }
    async fn execute(
        &self,
        _ctx: &ToolContext,
        _args: serde_json::Value,
    ) -> nuo_tool::Result<ToolOutput> {
        Ok(ToolOutput::success("all code verified clean"))
    }
}

struct DestructiveMutator {
    called: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl Tool for DestructiveMutator {
    fn name(&self) -> &str {
        "destructive_patch"
    }
    fn description(&self) -> &str {
        "Mutates files destructively"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ArbitraryExecution
    }
    async fn execute(
        &self,
        _ctx: &ToolContext,
        _args: serde_json::Value,
    ) -> nuo_tool::Result<ToolOutput> {
        self.called.store(true, Ordering::SeqCst);
        Ok(ToolOutput::success("mutated"))
    }
}

#[tokio::test]
async fn test_skill_zero_footprint_and_catalog_summary() {
    let mock = MockProvider::new();
    mock.push_text("Catalog acknowledged.").await;

    let review_skill = Skill::builder("code-audit")
        .name("Strict Code Audit")
        .description("Applies rigorous architectural review without disk mutations.")
        .instructions("MANDATORY SOP: Verify all invariants. Check CHESTERTON FENCE.")
        .scope(ToolScope::ReadOnly)
        .build();

    let agent = Agent::builder("agent://local/auditor")
        .provider(mock.clone())
        .skill(review_skill)
        .build()
        .await
        .unwrap();

    let sys_prompt = agent.system_prompt().await;

    // [INV-SKILL-01]: Unmounted skill shows ONLY catalog summary, NOT full SOP
    assert!(sys_prompt.contains("Available procedural skills"));
    assert!(sys_prompt.contains("- code-audit: Applies rigorous architectural review without disk mutations."));
    assert!(!sys_prompt.contains("MANDATORY SOP: Verify all invariants"));

    // Manifest parity: skill ID is exposed for P2P discovery
    assert_eq!(agent.manifest().skills, vec!["code-audit"]);
}

#[tokio::test]
async fn test_ephemeral_capability_sandboxing_and_reversibility() {
    let mock = MockProvider::new();
    let mutator_invoked = Arc::new(AtomicBool::new(false));

    // Turn 1: Mount the skill
    mock.push_response(ModelResponse::tool_call(
        "call_mount",
        "mount_skill",
        serde_json::json!({ "skill_id": "safe-review" }),
        10,
        10,
    ))
    .await;

    // Turn 2: Inside the skill, verify read_inspection tool works
    mock.push_response(ModelResponse::tool_call(
        "call_inspect",
        "read_inspection",
        serde_json::json!({}),
        10,
        10,
    ))
    .await;

    // Turn 3: Unmount skill and finalize answer
    mock.push_response(ModelResponse::tool_call(
        "call_unmount",
        "unmount_skill",
        serde_json::json!({}),
        10,
        10,
    ))
    .await;

    // Turn 4: Final response
    mock.push_text("Review complete and skill cleanly unmounted.").await;

    let safe_skill = Skill::builder("safe-review")
        .description("Confines agent strictly to read-only tools")
        .instructions("Adhere to read-only safety boundary.")
        .scope(ToolScope::ReadOnly)
        .build();

    let agent = Agent::builder("agent://local/security-agent")
        .provider(mock.clone())
        .skill(safe_skill)
        .tool(ReadOnlyInspector)
        .tool(DestructiveMutator {
            called: mutator_invoked.clone(),
        })
        .build()
        .await
        .unwrap();

    // Baseline: Both tools are available before mounting
    assert!(agent.advertised_tool_names().contains(&"read_inspection".to_string()));
    assert!(agent.advertised_tool_names().contains(&"destructive_patch".to_string()));
    assert!(agent.advertised_tool_names().contains(&"mount_skill".to_string()));

    let answer = agent.prompt("Audit our safety posture.").await.unwrap();
    assert_eq!(answer, "Review complete and skill cleanly unmounted.");

    // Verify the recorded requests in the cognitive loop
    let requests = mock.requests().await;
    assert!(requests.len() >= 3);

    // [INV-SKILL-02]: While safe-review was mounted, destructive_patch was excluded from model specs!
    let turn_2_request = &requests[1];
    let turn_2_tool_names: Vec<String> = turn_2_request
        .tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
        .collect();

    assert!(turn_2_tool_names.contains(&"read_inspection".to_string()));
    assert!(!turn_2_tool_names.contains(&"destructive_patch".to_string()));

    // [INV-SKILL-03]: SOP was injected into turn 2's request messages!
    let turn_2_sop_message = turn_2_request
        .messages
        .iter()
        .find(|m| m.content.contains("[Active Procedural Skill SOP]"));
    assert!(turn_2_sop_message.is_some());
    assert!(turn_2_sop_message.unwrap().content.contains("Adhere to read-only safety boundary."));

    // [INV-SKILL-04]: After unmounting, active skill is cleared
    assert!(agent.active_skill().is_none());
    assert!(!mutator_invoked.load(Ordering::SeqCst));
}

#[test]
fn test_skill_stack_nesting_and_scope_resolution() {
    let mut stack = SkillStack::new();
    assert!(!stack.is_active());
    assert_eq!(stack.depth(), 0);

    let baseline_scopes = vec![ToolScope::Workspace, ToolScope::Network];

    let l1_skill = Skill::builder("l1-analysis")
        .scope(ToolScope::ReadOnly)
        .instructions("L1 instructions")
        .build();

    let l2_skill = Skill::builder("l2-review")
        .scope(ToolScope::custom("specialized_audit"))
        .instructions("L2 instructions")
        .build();

    // Push L1
    stack.push(l1_skill, Some(baseline_scopes.clone()));
    assert!(stack.is_active());
    assert_eq!(stack.depth(), 1);
    assert_eq!(
        stack.effective_scopes(Some(&baseline_scopes)),
        Some(vec![ToolScope::ReadOnly])
    );

    // Push nested L2
    stack.push(l2_skill, stack.effective_scopes(Some(&baseline_scopes)));
    assert_eq!(stack.depth(), 2);
    assert_eq!(
        stack.effective_scopes(Some(&baseline_scopes)),
        Some(vec![ToolScope::custom("specialized_audit")])
    );

    let instructions = stack.active_instructions();
    assert_eq!(instructions.len(), 2);
    assert_eq!(instructions[0].0, "L1 instructions");
    assert_eq!(instructions[1].0, "L2 instructions");

    // Pop L2 (unmount)
    let popped_l2 = stack.pop().unwrap();
    assert_eq!(popped_l2.skill.id, "l2-review");
    assert_eq!(stack.depth(), 1);
    assert_eq!(
        stack.effective_scopes(Some(&baseline_scopes)),
        Some(vec![ToolScope::ReadOnly])
    );

    // Pop L1 (unmount)
    let popped_l1 = stack.pop().unwrap();
    assert_eq!(popped_l1.skill.id, "l1-analysis");
    assert_eq!(stack.depth(), 0);
    assert_eq!(
        stack.effective_scopes(Some(&baseline_scopes)),
        Some(baseline_scopes)
    );
}
