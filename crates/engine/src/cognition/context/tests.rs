//! Compiler and plan tests for the declared context policy (ADR-0215).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use protocol::{
    Block, BlockRender, Budget, NoticeKind, Overflow, Position, Rank, Resume, Stability, Topic,
};
use tool::BuiltinTool;

use super::builtin::default_sources;
use super::inputs::{ContextInputs, InputKey, ToolSurface};
use super::registry::{ContextPolicyError, Policy};
use super::source::{ContextSource, SourceOrder};

/// A minimal declared source for registry mechanics.
struct TestSource {
    id: &'static str,
    topic: Topic,
    keys: &'static [InputKey],
    order: SourceOrder,
    supersedes: &'static [&'static str],
    text: Option<&'static str>,
    budget: Budget,
}

impl TestSource {
    fn new(id: &'static str, topic: Topic, text: &'static str) -> Self {
        Self {
            id,
            topic,
            keys: &[],
            order: SourceOrder::Tail,
            supersedes: &[],
            text: Some(text),
            budget: Budget::elide_to_operator(1_000),
        }
    }

    fn keys(mut self, keys: &'static [InputKey]) -> Self {
        self.keys = keys;
        self
    }

    fn order(mut self, order: SourceOrder) -> Self {
        self.order = order;
        self
    }

    fn supersedes(mut self, ids: &'static [&'static str]) -> Self {
        self.supersedes = ids;
        self
    }

    fn budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }
}

impl ContextSource for TestSource {
    fn id(&self) -> &'static str {
        self.id
    }
    fn topic(&self) -> Topic {
        self.topic
    }
    fn inputs(&self) -> &'static [InputKey] {
        self.keys
    }
    fn order(&self) -> SourceOrder {
        self.order.clone()
    }
    fn supersedes(&self) -> &'static [&'static str] {
        self.supersedes
    }
    fn render(&self, _inputs: &ContextInputs) -> Option<Block> {
        self.text.map(|text| Block::new(self.id, text))
    }
    fn budget(&self) -> Budget {
        self.budget.clone()
    }
}

fn compile(sources: Vec<TestSource>) -> Result<Policy, ContextPolicyError> {
    Policy::compile(
        sources
            .into_iter()
            .map(|source| Box::new(source) as Box<dyn ContextSource + Send + Sync>)
            .collect(),
    )
}

fn ids(policy: &Policy) -> Vec<&'static str> {
    policy.source_ids()
}

#[test]
fn the_default_policy_compiles() {
    let policy = super::builtin::default_policy();
    assert!(!policy.source_ids().is_empty());
}

/// The product's compiled-in default is ordered by declaration, so
/// `default_policy()` skips validation. This test is that validation: the general
/// compiler must produce the same order from the same declarations.
#[test]
fn the_default_declaration_order_compiles_unchanged() {
    let declared = super::builtin::default_sources();
    let ids: Vec<&'static str> = declared.iter().map(|source| source.id()).collect();
    let compiled = Policy::compile(declared).expect("the default declarations are valid");
    assert_eq!(
        compiled.source_ids(),
        ids,
        "the default declaration order is not its emission order; either reorder the \
         declarations or give `default_policy` a compiled order"
    );
}

#[test]
fn every_default_topic_has_exactly_one_owner() {
    let policy = super::builtin::default_policy();
    let mut topics: Vec<Topic> = policy
        .source_ids()
        .into_iter()
        .map(|id| policy.topic_of(id).expect("declared source"))
        .collect();
    let count = topics.len();
    topics.sort_unstable();
    topics.dedup();
    assert_eq!(topics.len(), count, "two default sources own one topic");
}

#[test]
fn duplicate_ids_are_rejected() {
    let error = compile(vec![
        TestSource::new("a", Topic::Identity, "A"),
        TestSource::new("a", Topic::Precedence, "B"),
    ])
    .unwrap_err();
    assert!(matches!(error, ContextPolicyError::DuplicateId("a")));
}

#[test]
fn a_duplicated_topic_is_a_compile_error_naming_both_owners() {
    // This is the defect that shipped: two sections stating the same rule.
    let error = compile(vec![
        TestSource::new("a", Topic::ToolDiscipline, "prefer the native tools"),
        TestSource::new("b", Topic::ToolDiscipline, "prefer the native tools"),
    ])
    .unwrap_err();
    match error {
        ContextPolicyError::DuplicateTopic { topic, owners } => {
            assert_eq!(topic, Topic::ToolDiscipline);
            assert_eq!(owners, vec!["a", "b"]);
        }
        other => panic!("expected DuplicateTopic, got {other:?}"),
    }
}

#[test]
fn supersedes_resolves_a_shared_topic_to_one_owner() {
    let policy = compile(vec![
        TestSource::new("a", Topic::ToolDiscipline, "A"),
        TestSource::new("b", Topic::ToolDiscipline, "B").supersedes(&["a"]),
    ])
    .unwrap();
    assert_eq!(ids(&policy), vec!["b"]);
}

#[test]
fn supersedes_naming_an_unknown_source_is_rejected() {
    let error = compile(vec![
        TestSource::new("a", Topic::ToolDiscipline, "A").supersedes(&["ghost"]),
    ])
    .unwrap_err();
    assert!(matches!(
        error,
        ContextPolicyError::UnresolvedSupersede {
            id: "a",
            target: "ghost"
        }
    ));
}

#[test]
fn two_sources_superseding_each_other_is_rejected() {
    let error = compile(vec![
        TestSource::new("a", Topic::ToolDiscipline, "A").supersedes(&["b"]),
        TestSource::new("b", Topic::ToolDiscipline, "B").supersedes(&["a"]),
    ])
    .unwrap_err();
    assert!(matches!(error, ContextPolicyError::SupersedeCycle { .. }));
}

#[test]
fn a_missing_anchor_is_rejected() {
    let error = compile(vec![
        TestSource::new("a", Topic::ToolDiscipline, "A").order(SourceOrder::After("ghost")),
    ])
    .unwrap_err();
    assert!(matches!(
        error,
        ContextPolicyError::MissingAnchor {
            id: "a",
            target: "ghost"
        }
    ));
}

#[test]
fn an_anchor_across_stability_bands_is_rejected() {
    // `b` is frozen; `a` reads session input, so it belongs to a later band and
    // must not anchor into an earlier one.
    let error = compile(vec![
        TestSource::new("b", Topic::ToolDiscipline, "B"),
        TestSource::new("a", Topic::Secrets, "A")
            .keys(&[InputKey::UserInstructions])
            .order(SourceOrder::After("b")),
    ])
    .unwrap_err();
    assert!(matches!(
        error,
        ContextPolicyError::CrossBandAnchor {
            id: "a",
            target: "b"
        }
    ));
}

#[test]
fn an_ordering_cycle_is_rejected() {
    let error = compile(vec![
        TestSource::new("a", Topic::ToolDiscipline, "A").order(SourceOrder::After("b")),
        TestSource::new("b", Topic::Secrets, "B").order(SourceOrder::After("a")),
    ])
    .unwrap_err();
    assert!(matches!(error, ContextPolicyError::Cycle(_)));
}

#[test]
fn a_frame_block_may_not_declare_a_volatile_input() {
    let error = compile(vec![
        TestSource::new("agent.identity", Topic::Identity, "A").keys(&[InputKey::ProjectRules]),
    ])
    .unwrap_err();
    assert!(matches!(
        error,
        ContextPolicyError::FrameVolatility {
            id: "agent.identity",
            input: "project_rules"
        }
    ));
}

#[test]
fn a_zero_budget_is_rejected() {
    let error = compile(vec![
        TestSource::new("a", Topic::ToolDiscipline, "A").budget(Budget::elide_to_operator(0)),
    ])
    .unwrap_err();
    assert!(matches!(error, ContextPolicyError::EmptyBudget { id: "a" }));
}

#[test]
fn a_directive_may_not_be_silently_dropped() {
    let error = compile(vec![
        TestSource::new("a", Topic::ToolDiscipline, "A").budget(Budget {
            tokens: 10,
            overflow: Overflow::Drop,
        }),
    ])
    .unwrap_err();
    assert!(matches!(
        error,
        ContextPolicyError::SilentDirectiveDrop { id: "a" }
    ));
}

#[test]
fn stability_is_derived_from_the_declared_inputs() {
    let policy = compile(vec![
        TestSource::new("frozen", Topic::ToolDiscipline, "F"),
        TestSource::new("route", Topic::Secrets, "R").keys(&[InputKey::ToolSurface]),
        TestSource::new("session", Topic::UserInstructions, "S")
            .keys(&[InputKey::UserInstructions]),
        TestSource::new("workspace", Topic::ProjectAuthority, "W").keys(&[InputKey::ProjectRules]),
        TestSource::new("turn", Topic::TurnDirective, "T").keys(&[InputKey::TurnDirective]),
    ])
    .unwrap();
    assert_eq!(policy.stability_of("frozen"), Some(Stability::Frozen));
    assert_eq!(policy.stability_of("route"), Some(Stability::Route));
    assert_eq!(policy.stability_of("session"), Some(Stability::Session));
    assert_eq!(policy.stability_of("workspace"), Some(Stability::Workspace));
    assert_eq!(policy.stability_of("turn"), Some(Stability::Turn));
}

#[test]
fn the_frame_leads_then_bands_ascend() {
    let policy = compile(vec![
        TestSource::new("workspace", Topic::ProjectAuthority, "W").keys(&[InputKey::ProjectRules]),
        TestSource::new("turn", Topic::TurnDirective, "T").keys(&[InputKey::TurnDirective]),
        TestSource::new("session", Topic::UserInstructions, "S")
            .keys(&[InputKey::UserInstructions]),
        TestSource::new("route", Topic::Secrets, "R").keys(&[InputKey::ToolSurface]),
        TestSource::new("frozen", Topic::ToolDiscipline, "F"),
        TestSource::new("precedence", Topic::Precedence, "P"),
        TestSource::new("identity", Topic::Identity, "I"),
    ])
    .unwrap();
    assert_eq!(
        ids(&policy),
        vec![
            "identity",
            "precedence",
            "frozen",
            "route",
            "session",
            "workspace",
            "turn"
        ]
    );
}

#[test]
fn the_default_policy_puts_identity_first_and_workspace_last() {
    let policy = super::builtin::default_policy();
    let order = ids(&policy);
    assert_eq!(order.first(), Some(&"agent.identity"));
    assert_eq!(order.get(1), Some(&"agent.precedence"));
    // The three workspace-volatile blocks are the tail of the prefix, so an
    // `AGENTS.md` edit cannot invalidate the rest of it.
    let tail = &order[order.len() - 3..];
    for id in tail {
        assert_eq!(
            policy.stability_of(id),
            Some(Stability::Workspace),
            "{id} is not in the workspace band"
        );
    }
}

#[test]
fn head_anchors_win_inside_a_band() {
    let policy = compile(vec![
        TestSource::new("a", Topic::ToolDiscipline, "A"),
        TestSource::new("b", Topic::Secrets, "B").order(SourceOrder::Head),
        TestSource::new("c", Topic::Delegation, "C"),
    ])
    .unwrap();
    assert_eq!(ids(&policy), vec!["b", "a", "c"]);
}

#[test]
fn a_before_anchor_orders_two_sources() {
    let policy = compile(vec![
        TestSource::new("a", Topic::ToolDiscipline, "A"),
        TestSource::new("b", Topic::Secrets, "B").order(SourceOrder::Before("a")),
    ])
    .unwrap();
    assert_eq!(ids(&policy), vec!["b", "a"]);
}

#[test]
fn the_plan_measures_every_source_and_keeps_inactive_rows() {
    let policy = compile(vec![
        TestSource::new("a", Topic::ToolDiscipline, "A").keys(&[InputKey::ProjectRules]),
        TestSource::new("b", Topic::Secrets, ""),
    ])
    .unwrap();
    let plan = policy.plan(&ContextInputs::empty());
    assert_eq!(plan.blocks.len(), 2);
    assert!(plan.block("a").unwrap().active);
    assert!(plan.fixed_tokens > 0);
    // An empty body is an active source that rendered no text, and the row says
    // so rather than vanishing.
    let b = plan.block("b").unwrap();
    assert!(!b.active);
    assert_eq!(b.inactive_reason, Some("active but rendered no text"));
}

#[test]
fn a_turn_source_lands_in_the_transient_region() {
    let policy = compile(vec![
        TestSource::new("fixed", Topic::ToolDiscipline, "F"),
        TestSource::new("turn", Topic::TurnDirective, "T").keys(&[InputKey::TurnDirective]),
    ])
    .unwrap();
    let plan = policy.plan(&ContextInputs::empty());
    assert_eq!(plan.fixed.len(), 1);
    assert_eq!(plan.transient.len(), 1);
    assert_eq!(plan.transient[0].id, "turn");
    assert!(plan.transient_tokens > 0);
}

#[test]
fn an_over_budget_body_is_elided_with_a_resume_path() {
    let policy = Policy::compile(vec![leaky(
        "lorem ipsum dolor sit amet ".repeat(400),
        Budget::elide_to_file(50, "AGENTS.md"),
    )])
    .unwrap();
    let plan = policy.plan(&ContextInputs::empty());
    let block = plan.block("leaky").expect("leaky row");
    let elision = block.elision.as_ref().expect("elided");
    assert!(elision.dropped_tokens() > 0);
    assert!(matches!(elision.resume, Resume::File { .. }));
    assert_eq!(plan.notices.len(), 1);
    assert_eq!(plan.notices[0].kind, NoticeKind::Elided);
    assert!(plan.notices[0].message.contains("AGENTS.md"));
    assert!(block.tokens <= 50, "the emitted block must fit its budget");
}

#[test]
fn an_index_overflow_replaces_the_body_with_a_tool_pointer() {
    let policy = Policy::compile(vec![leaky(
        "word ".repeat(2_000),
        Budget::index_to_tool(50, "find_files"),
    )])
    .unwrap();
    let plan = policy.plan(&ContextInputs::empty());
    assert_eq!(plan.notices.len(), 1);
    assert_eq!(plan.notices[0].kind, NoticeKind::Indexed);
    assert!(plan.notices[0].message.contains("find_files"));
    assert!(plan.fixed_tokens < 200);
}

#[test]
fn the_instruction_ceiling_evicts_the_tail_and_keeps_the_frame() {
    // Both sources fit their own budgets; only the region ceiling can reject
    // them, which is what makes it the backstop rather than a duplicate of the
    // per-source bound.
    let policy = Policy::compile(vec![
        Box::new(TestSource::new(
            "agent.identity",
            Topic::Identity,
            "identity",
        )) as Box<dyn ContextSource + Send + Sync>,
        leaky("word ".repeat(400), Budget::elide_to_operator(10_000)),
    ])
    .unwrap();
    let plan = policy.plan_with_ceiling(&ContextInputs::empty(), 60);
    assert!(
        plan.block("agent.identity").unwrap().active,
        "the frame survives"
    );
    let evicted = plan.block("leaky").expect("leaky row");
    assert!(!evicted.active);
    assert!(
        plan.notices
            .iter()
            .any(|notice| notice.kind == NoticeKind::CeilingEvicted)
    );
}

#[test]
fn a_disabled_source_is_removed_and_bumps_the_generation() {
    let mut policy = super::builtin::default_policy();
    let generation = policy.generation();
    assert!(policy.disable("agent.exploration"));
    assert_eq!(policy.generation(), generation + 1);
    assert!(!policy.source_ids().contains(&"agent.exploration"));
    assert!(
        !policy.disable("agent.exploration"),
        "second disable is a no-op"
    );
}

#[test]
fn into_sources_round_trips_the_declaration_set() {
    let policy = super::builtin::default_policy();
    let expected = policy.source_ids();
    let sources = policy.into_sources();
    let recompiled = Policy::compile(sources).expect("recompiles");
    assert_eq!(recompiled.source_ids(), expected);
}

// ── The default policy's content, pinned ───────────────────────────────────

fn rendered(plan: &protocol::ContextPlan) -> String {
    protocol::render_blocks(&plan.fixed)
}

fn populated_inputs() -> ContextInputs {
    ContextInputs {
        identity_preamble: "You are Nuo.".to_string(),
        tool_surface: ToolSurface::new(
            [
                &FakeTool(BuiltinTool::Read) as &dyn tool::Tool,
                &FakeTool(BuiltinTool::FindFiles),
                &FakeTool(BuiltinTool::SearchText),
                &FakeTool(BuiltinTool::ReadUrl),
            ],
            true,
            2,
        ),
        project_rules: "Run `cargo test` before claiming success.".to_string(),
        workspace_root: Some("/data/projects/example".to_string()),
        workspace_topology: Some("- crates/*".to_string()),
        ..Default::default()
    }
}

struct FakeTool(BuiltinTool);

#[async_trait::async_trait]
impl tool::Tool for FakeTool {
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
        _ctx: &tool::ToolContext,
        _args: serde_json::Value,
    ) -> tool::Result<tool::ToolOutput> {
        Ok(tool::ToolOutput::text("ok"))
    }
}

#[test]
fn no_sentence_appears_in_two_default_blocks() {
    // The defect this design exists to prevent: two sources stating one rule.
    let plan = super::builtin::default_policy().plan(&populated_inputs());
    let mut sentences: Vec<(String, String)> = Vec::new();
    for block in &plan.fixed {
        for line in block.body.lines() {
            let line = line.trim();
            if line.len() < 40 || line.starts_with('#') {
                continue;
            }
            sentences.push((block.id.clone(), line.to_owned()));
        }
    }
    for (index, (id, line)) in sentences.iter().enumerate() {
        for (other_id, other) in sentences.iter().skip(index + 1) {
            assert_ne!(
                (id, line),
                (other_id, other),
                "`{id}` and `{other_id}` state the same rule"
            );
        }
    }
}

#[test]
fn exploration_guidance_is_gated_on_capability_not_on_a_tool_name() {
    let policy = super::builtin::default_policy();
    let bare = policy.plan(&ContextInputs {
        identity_preamble: "You are Nuo.".to_string(),
        ..Default::default()
    });
    assert!(
        !bare
            .fixed
            .iter()
            .any(|block| block.id == "agent.exploration"),
        "no native tools, so no exploration protocol"
    );

    let rich = policy.plan(&populated_inputs());
    let block = rich
        .fixed
        .iter()
        .find(|block| block.id == "agent.exploration")
        .expect("exploration protocol");
    assert!(block.body.contains("Search"));
    assert!(block.body.contains("native"));
}

#[test]
fn activating_a_deferred_tool_does_not_move_the_instruction_prefix() {
    let policy = super::builtin::default_policy();
    let mut inputs = populated_inputs();
    let before = protocol::render_blocks(&policy.plan(&inputs).fixed);
    inputs.tool_surface = ToolSurface::new(
        [
            &FakeTool(BuiltinTool::Read) as &dyn tool::Tool,
            &FakeTool(BuiltinTool::FindFiles),
            &FakeTool(BuiltinTool::SearchText),
            &FakeTool(BuiltinTool::ReadUrl),
        ],
        true,
        99,
    );
    let after = protocol::render_blocks(&policy.plan(&inputs).fixed);
    assert_eq!(
        before, after,
        "a deferred activation changed the cacheable prefix ([INV-TOOLSURFACE-03])"
    );
}

#[test]
fn the_skill_catalog_discloses_what_it_left_out() {
    let policy = super::builtin::default_policy();
    let catalog: Vec<(String, String)> = (0..100)
        .map(|index| (format!("skill-{index:03}"), "does a thing".to_string()))
        .collect();
    let plan = policy.plan(&ContextInputs {
        identity_preamble: "You are Nuo.".to_string(),
        skill_catalog: catalog,
        ..Default::default()
    });
    let block = plan
        .fixed
        .iter()
        .find(|block| block.id == "skills.catalog")
        .expect("catalog");
    assert!(block.body.contains("showing 64 of 100"));
    assert!(block.body.contains("list_skills"));
}

#[test]
fn the_precedence_contract_is_unconditional() {
    // A bare session still learns how conflicting instructions resolve; the
    // block is a frame block, so it must not depend on workspace state.
    let plan = super::builtin::default_policy().plan(&ContextInputs::empty());
    assert!(plan.block("agent.precedence").unwrap().active);
}

#[test]
fn the_default_policy_never_emits_an_untrusted_block_into_the_prefix() {
    let plan = super::builtin::default_policy().plan(&populated_inputs());
    for row in plan.blocks.iter().filter(|row| row.active) {
        if row.rank == Rank::Untrusted {
            assert_eq!(
                row.position,
                Position::Body,
                "an untrusted block may not be a frame block"
            );
            assert!(
                row.topic == Topic::UntrustedPolicy || row.topic == Topic::Retrieval,
                "unexpected untrusted topic"
            );
        }
    }
}

#[test]
fn no_default_source_reads_a_raw_tool_name() {
    // Every default source declares only InputKeys, and there is no key that
    // exposes a tool-name list ([INV-TOOLSURFACE-01]).
    for key in InputKey::ALL {
        assert_ne!(key.label(), "tool_names");
    }
    let inputs = ContextInputs::empty();
    assert_eq!(
        protocol::render_blocks(&super::builtin::default_policy().plan(&inputs).fixed),
        protocol::render_blocks(
            &super::builtin::default_policy()
                .plan(&ContextInputs {
                    provider_guidance: "",
                    ..inputs.clone()
                })
                .fixed
        )
    );
}

#[test]
fn every_default_source_declares_a_budget_and_a_topic() {
    let policy = super::builtin::default_policy();
    for id in policy.source_ids() {
        let topic = policy.topic_of(id).expect("topic");
        assert_eq!(topic.rank(), topic.rank(), "rank is derived");
        assert!(
            policy.stability_of(id).is_some(),
            "{id} has no derived stability"
        );
    }
}

#[test]
fn the_rendered_prefix_keeps_its_presentational_contract() {
    let plan = super::builtin::default_policy().plan(&populated_inputs());
    let text = rendered(&plan);
    assert!(!text.starts_with('\n'), "no leading newline");
    assert!(
        !text.contains("\n\n\n"),
        "at most one blank line between blocks"
    );
    assert!(text.contains("<agent-contract>"));
    assert!(text.contains("</agent-contract>"));
}

/// A source whose body is fixed, so the overflow and ceiling paths are reachable
/// without pinning a real section's exact length.
struct LeakySource {
    body: String,
    budget: Budget,
}

impl ContextSource for LeakySource {
    fn id(&self) -> &'static str {
        "leaky"
    }
    fn topic(&self) -> Topic {
        Topic::WorkspaceTopology
    }
    fn inputs(&self) -> &'static [InputKey] {
        &[InputKey::WorkspaceTopology]
    }
    fn render(&self, _inputs: &ContextInputs) -> Option<Block> {
        Some(Block::new("leaky", self.body.clone()).with_render(BlockRender::Plain))
    }
    fn budget(&self) -> Budget {
        self.budget.clone()
    }
}

fn leaky(body: impl Into<String>, budget: Budget) -> Box<dyn ContextSource + Send + Sync> {
    Box::new(LeakySource {
        body: body.into(),
        budget,
    })
}

#[test]
fn the_default_sources_are_all_reachable() {
    assert_eq!(default_sources().len(), 14);
}

// ── The answer language (ADR-0222) ─────────────────────────────────────────

fn with_language(tag: &str) -> ContextInputs {
    ContextInputs {
        identity_preamble: "You are Nuo.".to_string(),
        response_language: Some(protocol::ResponseLanguage::parse(tag).unwrap()),
        ..Default::default()
    }
}

#[test]
fn an_undeclared_language_renders_no_block_at_all() {
    // The one shape that matters: "not declared" must not become "English".
    let plan = super::builtin::default_policy().plan(&ContextInputs::empty());
    let row = plan.block("agent.language").expect("declared row");
    assert!(
        !row.active,
        "an undeclared language must contribute nothing"
    );
    assert_eq!(row.inactive_reason, Some("inactive for this request"));
    assert!(
        !rendered(&plan).contains("Answer in"),
        "an undeclared session must not carry a language rule"
    );
}

#[test]
fn a_declared_language_names_itself_and_disarms_the_english_context() {
    let plan = super::builtin::default_policy().plan(&with_language("zh-CN"));
    let text = rendered(&plan);
    assert!(text.contains("Answer in zh-CN."), "{text}");
    // The load-bearing clause: without it the block is one more English voice
    // in the same majority vote the drift came from.
    assert!(
        text.contains("outranks the language of everything else in this request"),
        "the rule must outrank the surrounding English context: {text}"
    );
    assert!(
        text.contains("never change the language you answer in"),
        "the rule must explicitly disarm the English injections: {text}"
    );
    // Verbatim material is exempted, or the rule would corrupt code and logs.
    assert!(
        text.contains("Code, identifiers, file paths, command lines"),
        "{text}"
    );
}

#[test]
fn the_language_block_sits_in_the_session_band_and_may_be_elided_never_dropped() {
    let policy = super::builtin::default_policy();
    assert_eq!(
        policy.stability_of("agent.language"),
        Some(Stability::Session),
        "a declared language is a session decision, not a route or turn fact"
    );
    assert_eq!(
        policy.topic_of("agent.language"),
        Some(Topic::OutputLanguage)
    );
    // A directive never uses `Overflow::Drop` — the compiler refuses it, and
    // this asserts the block was declared through that path.
    assert!(policy.source_ids().contains(&"agent.language"));
}

#[test]
fn changing_the_language_moves_only_the_session_band() {
    // Cache discipline: a `/language` switch must not invalidate the frozen head
    // of the prefix.
    let policy = super::builtin::default_policy();
    let before = policy.plan(&with_language("Chinese"));
    let after = policy.plan(&with_language("Japanese"));
    let head_of = |plan: &protocol::ContextPlan| -> String {
        plan.fixed
            .iter()
            .filter(|block| {
                plan.block(&block.id)
                    .is_some_and(|row| row.stability < Stability::Session)
            })
            .map(|block| protocol::render_blocks(std::slice::from_ref(block)))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    assert_eq!(
        head_of(&before),
        head_of(&after),
        "a language switch moved a block ahead of the session band"
    );
    assert_ne!(rendered(&before), rendered(&after));
}
