//! The default prompt as a reviewed artifact (ADR-0217).
//!
//! This is the mechanism that makes a prompt change visible. A structural test
//! can assert that a block exists; only a pinned rendering shows that two blocks
//! say the same thing, that a paragraph grew, or that the frame moved.
//!
//! Six invariants are pinned here:
//!
//! 1. the emission order of the default sources,
//! 2. the stability band of every block,
//! 3. the exact rendered bytes of the cacheable prefix,
//! 4. that mutating `AGENTS.md` moves only the workspace band,
//! 5. that two equal-cost blocks do not carry the same sentence,
//! 6. that the plan and the assembled request agree byte for byte.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use futures::stream::{self, BoxStream};
use nuo_engine::{
    Agent, AgentIdentity, ContextInputs, Message, Provider, Role, Stability, ToolSurface,
};
use nuo_tool::BuiltinTool;
use std::sync::Arc;

struct IdleProvider;

#[async_trait::async_trait]
impl Provider for IdleProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Ok(nuo_protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "done",
        )))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::once(async { Ok("done".to_owned()) })))
    }
}

struct FakeTool(BuiltinTool);

#[async_trait::async_trait]
impl nuo_tool::Tool for FakeTool {
    fn name(&self) -> &str {
        self.0.as_str()
    }
    fn description(&self) -> &str {
        "fake"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn builtin(&self) -> Option<BuiltinTool> {
        Some(self.0)
    }
    async fn execute(
        &self,
        _ctx: &nuo_tool::ToolContext,
        _args: serde_json::Value,
    ) -> nuo_tool::Result<nuo_tool::ToolOutput> {
        Ok(nuo_tool::ToolOutput::text("ok"))
    }
}

/// A fully-populated request: identity, native tools, web tools, project rules,
/// a topology summary, user and project instructions, a skill catalog.
fn rich_inputs() -> ContextInputs {
    ContextInputs {
        identity_preamble: "You are Nuo, a modular session server.".to_string(),
        tool_surface: ToolSurface::new(
            [
                &FakeTool(BuiltinTool::Read) as &dyn nuo_tool::Tool,
                &FakeTool(BuiltinTool::FindFiles),
                &FakeTool(BuiltinTool::SearchText),
                &FakeTool(BuiltinTool::CodeQuery),
                &FakeTool(BuiltinTool::ReadUrl),
            ],
            true,
        ),
        provider_guidance: "Prefer fenced code blocks.",
        project_rules: "Run the project test command before claiming success.".to_string(),
        additional_workspace_roots: vec!["/data/projects/sibling".to_string()],
        workspace_root: Some("/data/projects/example".to_string()),
        workspace_topology: Some("- crates/core\n- crates/app".to_string()),
        user_instructions: vec![nuo_protocol::ScopedInstruction {
            id: "user:0".to_string(),
            source: nuo_protocol::InstructionSource::User,
            ordinal: 0,
            selector: Default::default(),
            text: "Answer in Chinese.".to_string(),
        }],
        project_instructions: vec![nuo_protocol::ScopedInstruction {
            id: ".nuo/instructions/global.md".to_string(),
            source: nuo_protocol::InstructionSource::Project,
            ordinal: 0,
            selector: Default::default(),
            text: "Reject a second request compiler.".to_string(),
        }],
        skill_catalog: vec![("rust-expert".to_string(), "Rust help".to_string())],
        // Every realistic session has a declared language; the golden pins what a
        // declared one renders (ADR-0222).
        response_language: Some(nuo_protocol::ResponseLanguage::parse("Chinese").unwrap()),
    }
}

#[test]
fn the_default_emission_order_is_pinned() {
    let policy = nuo_engine::default_policy();
    assert_eq!(
        policy.source_ids(),
        vec![
            "agent.identity",
            "agent.precedence",
            "agent.coding_baseline",
            "security.secrets",
            "agent.exploration",
            "security.web_untrusted",
            "provider.guidance",
            "user.instructions",
            "agent.language",
            "skills.catalog",
            "workspace.roots",
            "project.rules",
            "project.instructions",
            "workspace.topology",
        ],
        "the frame leads, then the stability bands ascend; changing this order \
         changes the cacheable prefix and must be a reviewed diff"
    );
}

#[test]
fn the_stability_band_of_every_default_block_is_pinned() {
    let policy = nuo_engine::default_policy();
    let band = |id: &str| policy.stability_of(id).expect(id);
    assert_eq!(band("agent.identity"), Stability::Route);
    assert_eq!(band("agent.precedence"), Stability::Frozen);
    assert_eq!(band("agent.coding_baseline"), Stability::Frozen);
    assert_eq!(band("security.secrets"), Stability::Frozen);
    assert_eq!(band("agent.exploration"), Stability::Route);
    assert_eq!(band("security.web_untrusted"), Stability::Route);
    assert_eq!(band("provider.guidance"), Stability::Route);
    assert_eq!(band("user.instructions"), Stability::Session);
    assert_eq!(
        band("agent.language"),
        Stability::Session,
        "a declared language is a session decision: it survives /model and applies to subagents"
    );
    assert_eq!(band("skills.catalog"), Stability::Session);
    assert_eq!(band("workspace.roots"), Stability::Session);
    assert_eq!(band("project.rules"), Stability::Workspace);
    assert_eq!(band("project.instructions"), Stability::Workspace);
    assert_eq!(band("workspace.topology"), Stability::Workspace);
}

#[test]
fn the_bodies_join_back_into_the_rendered_prefix() {
    // The pinned JSON golden carries every block's body now (ADR-0220 §5), so
    // the separate rendered-prefix snapshot is gone: it pinned the same bytes a
    // second time. The invariant it existed for survives here, as an assertion
    // rather than as a second artifact — the bodies a reader sees, joined in
    // emission order, ARE the cacheable prefix the provider receives.
    let plan = nuo_engine::default_policy().plan(&rich_inputs());
    let measurement = nuo_protocol::ContextMeasurement {
        plan,
        estimate: None,
    };
    let view = nuo_engine::project_plan(&measurement, 200_000, 16_000);
    let joined: Vec<&str> = view
        .blocks
        .iter()
        .filter(|block| block.active)
        .filter_map(|block| block.body.as_deref())
        .collect();
    assert_eq!(
        joined.join("\n\n"),
        nuo_protocol::render_blocks(&measurement.plan.fixed),
        "the JSON golden's bodies must be the cacheable prefix, byte for byte"
    );
}

#[test]
fn the_frame_is_byte_stable_across_a_workspace_edit() {
    // The whole point of stability ordering: an `AGENTS.md` edit must not
    // invalidate the head of the cacheable prefix.
    let policy = nuo_engine::default_policy();
    let before = policy.plan(&rich_inputs());
    let mut after_inputs = rich_inputs();
    after_inputs.project_rules = "A completely different rule set.".to_string();
    after_inputs.workspace_topology = Some("- crates/other".to_string());
    let after = policy.plan(&after_inputs);

    let head = |plan: &nuo_protocol::ContextPlan| -> String {
        plan.fixed
            .iter()
            .zip(after.fixed.iter())
            .take_while(|(block, _)| {
                plan.block(&block.id)
                    .is_some_and(|row| row.stability != Stability::Workspace)
            })
            .map(|(block, _)| nuo_protocol::render_blocks(std::slice::from_ref(block)))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    assert_eq!(
        head(&before),
        head(&after),
        "a workspace edit moved the stable head of the prefix"
    );
    assert_ne!(
        nuo_protocol::render_blocks(&before.fixed),
        nuo_protocol::render_blocks(&after.fixed),
        "the workspace band must actually change"
    );
}

#[test]
fn no_two_blocks_state_the_same_rule() {
    // The defect this design exists to prevent shipped twice: the native-tool
    // preference and the parallelization rule each appeared in two sections.
    let plan = nuo_engine::default_policy().plan(&rich_inputs());
    let mut sentences: Vec<(String, String)> = Vec::new();
    for block in &plan.fixed {
        for line in block.body.lines() {
            let line = line.trim();
            if line.len() < 40 || line.starts_with('#') || line.starts_with('<') {
                continue;
            }
            sentences.push((block.id.clone(), line.to_owned()));
        }
    }
    let mut violations = Vec::new();
    for (index, (id, line)) in sentences.iter().enumerate() {
        for (other_id, other) in sentences.iter().skip(index + 1) {
            if line == other {
                violations.push(format!("`{id}` and `{other_id}` both say: {line}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "one topic has one owner, so one rule has one home \
         ([INV-CTXSRC-01], ADR-0215): {violations:#?}"
    );
}

#[test]
fn the_plan_and_the_request_are_the_same_bytes() {
    // `[INV-CTXPLAN-01]`: an inspector must never show a prompt the model did
    // not receive.
    let agent =
        Agent::builder(Arc::new(IdleProvider), Vec::new(), AgentIdentity::default()).build();
    agent.set_project_rules("Run the tests.".to_string());
    let mut messages = vec![Message::new(Role::User, "hello")];
    agent.prepare_request_messages_debug(&mut messages);

    let system = messages
        .iter()
        .find(|message| message.role == Role::System)
        .expect("system message");
    let plan = agent.context_plan().expect("plan recorded");

    assert_eq!(
        system.content,
        nuo_protocol::render_blocks(&plan.fixed),
        "the recorded plan is not the prompt that was sent"
    );
}

#[test]
fn the_context_table_renders_every_declared_source() {
    let agent =
        Agent::builder(Arc::new(IdleProvider), Vec::new(), AgentIdentity::default()).build();
    let mut messages = vec![Message::new(Role::User, "hello")];
    agent.prepare_request_messages_debug(&mut messages);
    let table = agent.render_context_table().expect("table");
    for id in nuo_engine::default_policy().source_ids() {
        assert!(table.contains(id), "the table omits `{id}`:\n{table}");
    }
    assert!(table.contains("stability"));
    assert!(table.contains("drift:"));
}

#[test]
fn an_elided_block_names_its_recovery_route_in_the_table() {
    let policy = nuo_engine::default_policy();
    // A project rules document far past its 8k budget.
    let measurement = nuo_protocol::ContextMeasurement {
        plan: policy.plan(&ContextInputs {
            identity_preamble: "You are Nuo.".to_string(),
            project_rules: "line of a very long rule document\n".repeat(4_000),
            ..Default::default()
        }),
        estimate: None,
    };
    let view = nuo_engine::project_plan(&measurement, 200_000, 16_000);
    let table = nuo_engine::render_context_table(&view);
    assert!(table.contains("project.rules"));
    assert!(
        table.contains("AGENTS.md"),
        "the elision must name the file to read:\n{table}"
    );
    assert!(table.contains("elided"));
}

#[test]
fn the_json_form_is_the_golden_the_test_suite_pins() {
    let measurement = nuo_protocol::ContextMeasurement {
        plan: nuo_engine::default_policy().plan(&rich_inputs()),
        estimate: None,
    };
    let view = nuo_engine::project_plan(&measurement, 200_000, 16_000);
    let json = nuo_engine::render_context_json(&view).expect("json");
    insta::assert_snapshot!("default_prefix_json", json);
}
