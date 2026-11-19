//! Public-API tests for embedding-owned request composition (ADR-0215).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use engine::{
    Agent, AgentIdentity, Block, Budget, ContextInputs, ContextPolicyError, ContextSource,
    InputKey, Message, Provider, Role, SourceOrder, ToolSurface, Topic,
};
use futures::stream::{self, BoxStream};
use tool::BuiltinTool;

struct IdleProvider;

#[async_trait]
impl Provider for IdleProvider {
    async fn chat(
        &self,
        _request: &protocol::ModelRequest,
    ) -> Result<protocol::ProviderCompletion, protocol::ProviderError> {
        Ok(protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "done",
        )))
    }

    async fn stream_chat(
        &self,
        _request: &protocol::ModelRequest,
        _telemetry: protocol::TransportTelemetry,
    ) -> Result<BoxStream<'static, Result<String, protocol::ProviderError>>, protocol::ProviderError>
    {
        Ok(Box::pin(stream::once(async { Ok("done".to_owned()) })))
    }
}

/// An embedding-owned context source.
struct ProductPolicy {
    id: &'static str,
    topic: Topic,
    text: &'static str,
    order: SourceOrder,
}

impl ProductPolicy {
    fn new(id: &'static str, topic: Topic, text: &'static str) -> Self {
        Self {
            id,
            topic,
            text,
            order: SourceOrder::Tail,
        }
    }
}

impl ContextSource for ProductPolicy {
    fn id(&self) -> &'static str {
        self.id
    }
    fn topic(&self) -> Topic {
        self.topic
    }
    fn inputs(&self) -> &'static [InputKey] {
        &[]
    }
    fn order(&self) -> SourceOrder {
        self.order.clone()
    }
    fn render(&self, _inputs: &ContextInputs) -> Option<Block> {
        Some(Block::new(self.id, self.text))
    }
    fn budget(&self) -> Budget {
        Budget::elide_to_operator(500)
    }
}

fn builder() -> engine::AgentBuilder {
    Agent::builder(Arc::new(IdleProvider), Vec::new(), AgentIdentity::default())
}

fn system_of(agent: &Agent) -> String {
    let mut messages = vec![Message::new(Role::User, "hello")];
    agent.prepare_request_messages_debug(&mut messages);
    messages
        .into_iter()
        .find(|message| message.role == Role::System)
        .map(|message| message.content)
        .unwrap_or_default()
}

#[test]
fn embedding_can_extend_and_remove_prompt_policy_before_build() {
    let agent = builder()
        .add_context_source(ProductPolicy::new(
            "embedding.test.policy",
            Topic::Memory,
            "PRODUCT-POLICY",
        ))
        .unwrap()
        .disable_context_source("project.rules")
        .unwrap()
        .build();

    let content = system_of(&agent);
    assert!(content.contains("PRODUCT-POLICY"), "{content}");
    assert!(
        agent
            .context_plan()
            .expect("plan")
            .block("project.rules")
            .is_none(),
        "the disabled source is gone from the plan"
    );
}

#[test]
fn embedding_configuration_errors_are_structured() {
    // A duplicate id is rejected, naming the offender.
    let result = builder().add_context_source(ProductPolicy::new(
        "agent.identity",
        Topic::Memory,
        "collision",
    ));
    assert!(matches!(
        result,
        Err(ContextPolicyError::DuplicateId("agent.identity"))
    ));

    // A duplicate topic is the defect the compiler exists to catch.
    let result = builder().add_context_source(ProductPolicy::new(
        "embedding.duplicate",
        Topic::ToolDiscipline,
        "prefer something else",
    ));
    match result {
        Err(ContextPolicyError::DuplicateTopic { topic, owners }) => {
            assert_eq!(topic, Topic::ToolDiscipline);
            assert!(owners.contains(&"embedding.duplicate"));
        }
        Err(other) => panic!("expected DuplicateTopic, got {other:?}"),
        Ok(_) => panic!("a duplicate topic must not compile"),
    }

    let result = builder().disable_context_source("missing.source");
    assert!(matches!(
        result,
        Err(ContextPolicyError::UnknownSource(id)) if id == "missing.source"
    ));
}

#[test]
fn default_prompt_carries_the_coding_baseline() {
    let content = system_of(&builder().build());
    assert!(content.contains("Working Contract"), "{content}");
    assert!(content.contains("verify it"), "{content}");
    assert!(
        !content.contains("You are Nuo"),
        "identity remains opt-in; the baseline is not an identity override"
    );
}

#[test]
fn the_identity_lines_lead_the_prompt() {
    let agent = builder().build();
    agent.set_identity(AgentIdentity::new("Nuo", "a modular session server"));
    let content = system_of(&agent);
    let identity = content.find("Nuo").expect("identity rendered");
    let contract = content.find("Working Contract").expect("contract rendered");
    assert!(
        identity < contract,
        "identity is the frame and must lead: {content}"
    );
}

#[test]
fn embedding_can_order_sources_semantically() {
    let agent = builder()
        .add_context_source(ProductPolicy::new(
            "embedding.first",
            Topic::Memory,
            "FIRST-GUIDANCE",
        ))
        .unwrap()
        .add_context_source(ProductPolicy::new(
            "embedding.second",
            Topic::Retrieval,
            "SECOND-GUIDANCE",
        ))
        .unwrap()
        .build();

    let content = system_of(&agent);
    let first = content.find("FIRST-GUIDANCE").expect("first");
    let second = content.find("SECOND-GUIDANCE").expect("second");
    assert!(first < second, "declaration order is the default order");
}

#[test]
fn a_before_anchor_is_honored_across_declaration_order() {
    let mut second = ProductPolicy::new("embedding.second", Topic::Retrieval, "SECOND-GUIDANCE");
    second.order = SourceOrder::Before("embedding.first");
    let agent = builder()
        .add_context_source(ProductPolicy::new(
            "embedding.first",
            Topic::Memory,
            "FIRST-GUIDANCE",
        ))
        .unwrap()
        .add_context_source(second)
        .unwrap()
        .build();
    let content = system_of(&agent);
    let first = content.find("FIRST-GUIDANCE").expect("first");
    let second = content.find("SECOND-GUIDANCE").expect("second");
    assert!(second < first, "the anchor reordered the pair: {content}");
}

#[test]
fn user_instructions_are_rendered_dynamically() {
    let instruction = |text: &str| protocol::ScopedInstruction {
        id: "user:0".to_string(),
        source: protocol::InstructionSource::User,
        ordinal: 0,
        selector: Default::default(),
        text: text.to_string(),
    };
    let agent = builder()
        .with_user_instructions(vec![instruction(
            "Speak natural plain Chinese, avoiding bureaucratic cliches.",
        )])
        .build();

    let content = system_of(&agent);
    assert!(
        content.contains("Speak natural plain Chinese, avoiding bureaucratic cliches."),
        "request text must render user instructions: {content}"
    );

    // Dynamic runtime update (e.g. on model switch)
    agent.set_user_instructions(vec![instruction(
        "Updated user instruction: ultra-concise mode.",
    )]);
    let content = system_of(&agent);
    assert!(
        content.contains("Updated user instruction: ultra-concise mode."),
        "request text must reflect dynamic update: {content}"
    );
    assert!(!content.contains("avoiding bureaucratic cliches."));
}

#[test]
fn subagent_inherits_parent_user_instructions() {
    let instruction = protocol::ScopedInstruction {
        id: "user:0".to_string(),
        source: protocol::InstructionSource::User,
        ordinal: 0,
        selector: Default::default(),
        text: "Inherited user instruction from parent.".to_string(),
    };
    let parent = builder().with_user_instructions(vec![instruction]).build();

    let tool = engine::SubagentTool::new(
        Arc::new(IdleProvider),
        protocol::ToolSet::from_tools(Vec::new()),
        &protocol::SubAgentProfile::EXPLORE,
    );
    tool.bind_user_instructions(parent.user_instructions_handle());

    let mut subagent = builder().build();
    subagent.bind_user_instructions(parent.user_instructions_handle());
    subagent.set_active_role(Some("explore".to_string()));

    let content = system_of(&subagent);
    assert!(
        content.contains("Inherited user instruction from parent."),
        "{content}"
    );
}

#[test]
fn subagent_inherits_parent_project_instructions_and_provider_kind() {
    let parent = builder().build();
    parent.set_project_instructions(vec![
        protocol::ScopedInstruction {
            id: ".nuo/instructions/global.md".to_string(),
            source: protocol::InstructionSource::Project,
            ordinal: 0,
            selector: Default::default(),
            text: "Global project rule.".to_string(),
        },
        protocol::ScopedInstruction {
            id: ".nuo/instructions/explore.md".to_string(),
            source: protocol::InstructionSource::Project,
            ordinal: 1,
            selector: protocol::InstructionSelector {
                role: Some("explore".to_string()),
                ..Default::default()
            },
            text: "Explore role rule.".to_string(),
        },
        protocol::ScopedInstruction {
            id: ".nuo/instructions/provider.md".to_string(),
            source: protocol::InstructionSource::Project,
            ordinal: 2,
            selector: protocol::InstructionSelector {
                provider: Some("commandcode-goat".to_string()),
                ..Default::default()
            },
            text: "Provider rule.".to_string(),
        },
        protocol::ScopedInstruction {
            id: ".nuo/instructions/reviewer.md".to_string(),
            source: protocol::InstructionSource::Project,
            ordinal: 3,
            selector: protocol::InstructionSelector {
                role: Some("reviewer".to_string()),
                ..Default::default()
            },
            text: "Reviewer rule.".to_string(),
        },
    ]);
    parent.set_instruction_provider_kind(Some("commandcode-goat".to_string()));

    let mut subagent = builder().build();
    subagent.bind_project_instructions(parent.project_instructions_handle());
    subagent.bind_instruction_provider_kind(parent.instruction_provider_kind_handle());
    subagent.set_active_role(Some("explore".to_string()));

    let prompt = system_of(&subagent);
    assert!(prompt.contains("Global project rule."), "{prompt}");
    assert!(prompt.contains("Explore role rule."), "{prompt}");
    assert!(prompt.contains("Provider rule."), "{prompt}");
    assert!(
        !prompt.contains("Reviewer rule."),
        "a sibling role must not leak into the child: {prompt}"
    );
}

#[test]
fn exploration_guidance_follows_the_declared_tool_surface() {
    // The registry is read directly: this is the vocabulary an embedder or a
    // provider asks about, and it must not depend on tool *names*.
    let policy = engine::default_policy();

    let command_only = policy.plan(&ContextInputs {
        workspace_topology: Some("- crates/*".to_string()),
        ..Default::default()
    });
    let bare = protocol::render_blocks(&command_only.fixed);
    assert!(bare.contains("crates/*"));
    assert!(
        !bare.contains("Batch independent inspection"),
        "no native tools, no batching guidance: {bare}"
    );

    let native = policy.plan(&ContextInputs {
        workspace_topology: Some("- crates/*".to_string()),
        tool_surface: ToolSurface::new(
            [
                &FakeTool(BuiltinTool::Read) as &dyn tool::Tool,
                &FakeTool(BuiltinTool::CodeQuery),
            ],
            true,
            0,
        ),
        ..Default::default()
    });
    let rich = protocol::render_blocks(&native.fixed);
    assert!(
        rich.contains("Batch independent inspection"),
        "parallel channel batches independent reads: {rich}"
    );
    assert!(rich.contains("code query tool"), "{rich}");
}

struct FakeTool(BuiltinTool);

#[async_trait]
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

/// A subagent answers in the session's language (ADR-0222, `[INV-LANG-05]`).
///
/// The child's identity *is* its English task prompt, and its tool output is
/// English, so without inheritance it would answer in English and hand the parent
/// a report the human cannot read.
#[test]
fn subagent_inherits_the_parent_answer_language() {
    let parent = builder()
        .with_response_language(Some(
            protocol::ResponseLanguage::parse("zh-CN").expect("valid tag"),
        ))
        .build();
    assert_eq!(
        parent.response_language().as_ref().map(|tag| tag.as_str()),
        Some("zh-CN"),
        "the builder-declared language must reach the agent"
    );

    let tool = engine::SubagentTool::new(
        Arc::new(IdleProvider),
        protocol::ToolSet::from_tools(Vec::new()),
        &protocol::SubAgentProfile::EXPLORE,
    );
    tool.bind_response_language(parent.response_language_handle());

    let mut subagent = builder().build();
    subagent.bind_response_language(parent.response_language_handle());
    subagent.set_active_role(Some("explore".to_string()));

    let prompt = system_of(&subagent);
    assert!(prompt.contains("Answer in zh-CN."), "{prompt}");

    // Clearing the parent's declaration clears the child's next request: the
    // handle is shared, not copied, so the two cannot drift.
    parent.set_response_language(None);
    let cleared = system_of(&subagent);
    assert!(!cleared.contains("Answer in"), "{cleared}");
}
