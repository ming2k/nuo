//! Public-API tests for embedding-owned request composition.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use engine::{
    Agent, AgentIdentity, Block, InstructionContext, InstructionRegistryError, InstructionSection,
    Message, Provider, Role,
};
use futures::stream::{self, BoxStream};

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

struct ProductPolicy {
    id: &'static str,
    text: &'static str,
}

impl InstructionSection for ProductPolicy {
    fn id(&self) -> &'static str {
        self.id
    }

    fn render(&self, _ctx: &InstructionContext) -> Option<Block> {
        Some(Block::new(self.id, self.text))
    }
}

fn builder() -> engine::AgentBuilder {
    Agent::builder(Arc::new(IdleProvider), Vec::new(), AgentIdentity::default())
}

#[test]
fn embedding_can_extend_and_disable_prompt_policy_before_build() {
    let agent = builder()
        .register_instruction_section(ProductPolicy {
            id: "embedding.test.policy",
            text: "PRODUCT-POLICY",
        })
        .unwrap()
        .disable_instruction_section("project.rules")
        .unwrap()
        .build();

    let mut messages = vec![Message::new(Role::User, "hello")];
    agent.prepare_request_messages_debug(&mut messages);

    assert_eq!(messages[0].role, Role::System);
    assert!(messages[0].content.contains("PRODUCT-POLICY"));
}

#[test]
fn embedding_configuration_errors_are_structured() {
    let result = builder().register_instruction_section(ProductPolicy {
        id: "agent.identity",
        text: "collision",
    });
    assert!(matches!(
        result,
        Err(InstructionRegistryError::DuplicateId("agent.identity"))
    ));

    let result = builder().disable_instruction_section("missing.section");
    assert!(matches!(
        result,
        Err(InstructionRegistryError::UnknownId(id)) if id == "missing.section"
    ));
}

#[test]
fn default_prompt_carries_the_coding_baseline() {
    let agent = builder().build();
    let mut messages = vec![Message::new(Role::User, "test prompt")];
    agent.prepare_request_messages_debug(&mut messages);

    // A bare session still gets the working contract: tool discipline,
    // verification, and the todo rule are not optional identity decoration.
    let system = messages
        .iter()
        .find(|message| message.role == Role::System)
        .expect("default request must carry the coding baseline");
    assert!(system.content.contains("Working Contract"));
    assert!(system.content.contains("verify it"));
    assert!(
        !system.content.contains("You are Nuo"),
        "identity remains opt-in; the baseline is not an identity override"
    );
}

#[test]
fn embedding_can_order_sections_semantically() {
    let agent = builder()
        .register_instruction_section(ProductPolicy {
            id: "embedding.first",
            text: "FIRST-GUIDANCE",
        })
        .unwrap()
        .register_instruction_section(ProductPolicy {
            id: "embedding.second",
            text: "SECOND-GUIDANCE",
        })
        .unwrap()
        .order_instruction_section(
            "embedding.first",
            engine::InstructionOrder::Before("embedding.second"),
        )
        .unwrap()
        .build();

    let mut messages = vec![Message::new(Role::User, "test prompt")];
    agent.prepare_request_messages_debug(&mut messages);

    let content = &messages[0].content;
    let first_pos = content.find("FIRST-GUIDANCE").unwrap();
    let second_pos = content.find("SECOND-GUIDANCE").unwrap();
    assert!(
        first_pos < second_pos,
        "first section must appear before second section"
    );
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

    let mut messages = vec![Message::new(Role::User, "test")];
    agent.prepare_request_messages_debug(&mut messages);

    let content = &messages[0].content;
    assert!(
        content.contains("Speak natural plain Chinese, avoiding bureaucratic cliches."),
        "request text must render user instructions: {content}"
    );

    // Dynamic runtime update (e.g. on model switch)
    agent.set_user_instructions(vec![instruction(
        "Updated user instruction: ultra-concise mode.",
    )]);
    let mut messages2 = vec![Message::new(Role::User, "test2")];
    agent.prepare_request_messages_debug(&mut messages2);

    let content2 = &messages2[0].content;
    assert!(
        content2.contains("Updated user instruction: ultra-concise mode."),
        "request text must reflect dynamic update: {content2}"
    );
    assert!(!content2.contains("avoiding bureaucratic cliches."));
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

    let mut messages = vec![Message::new(Role::User, "delegate")];
    subagent.prepare_request_messages_debug(&mut messages);
    assert!(
        messages[0]
            .content
            .contains("Inherited user instruction from parent."),
        "{}",
        messages[0].content
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

    let mut messages = vec![Message::new(Role::User, "delegate")];
    subagent.prepare_request_messages_debug(&mut messages);
    let prompt = &messages[0].content;
    assert!(prompt.contains("Global project rule."), "{prompt}");
    assert!(prompt.contains("Explore role rule."), "{prompt}");
    assert!(prompt.contains("Provider rule."), "{prompt}");
    assert!(
        !prompt.contains("Reviewer rule."),
        "a sibling role must not leak into the child: {prompt}"
    );
}

#[test]
fn workspace_topology_guidance_adapts_to_parallel_support_and_tool_names() {
    let reg = engine::default_instruction_registry();

    // 1. Channel without parallel tool calls and without code_query:
    let ctx_no_parallel = InstructionContext {
        workspace_topology: Some("- crates/*\n- tools/*".to_string()),
        supports_parallel_tool_calls: false,
        tool_names: vec!["execute_command".to_string(), "read".to_string()],
        ..Default::default()
    };
    let regions1 = reg.build_regions(&ctx_no_parallel).unwrap();
    let rendered1 = protocol::render_blocks(&regions1.fixed);
    assert!(rendered1.contains("Workspace"));
    assert!(rendered1.contains("crates/*"));
    assert!(!rendered1.contains("Batch independent inspection"));
    assert!(!rendered1.contains("code_query"));

    // 2. Channel with parallel tool calls and with code_query admitted:
    let ctx_full = InstructionContext {
        workspace_topology: Some("- crates/*\n- tools/*".to_string()),
        supports_parallel_tool_calls: true,
        tool_names: vec!["code_query".to_string(), "execute_command".to_string()],
        ..Default::default()
    };
    let regions2 = reg.build_regions(&ctx_full).unwrap();
    let rendered2 = protocol::render_blocks(&regions2.fixed);
    assert!(rendered2.contains("Workspace"));
    assert!(rendered2.contains("Batch independent inspection and file examination operations"));
    assert!(rendered2.contains("Prefer structural AST queries (`code_query` outline/symbol)"));
}
