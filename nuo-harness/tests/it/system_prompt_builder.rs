//! Public-API tests for embedding-owned prompt composition.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use nuo_harness::{
    Agent, AgentIdentity, Message, Provider, Role, SystemPromptContext, SystemPromptRegistryError,
    SystemPromptSection,
};

struct IdleProvider;

#[async_trait]
impl Provider for IdleProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Ok(nuo_wire::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "done",
        )))
    }

    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::once(async { Ok("done".to_owned()) })))
    }
}

struct ProductPolicy {
    id: &'static str,
    tier: Option<nuo_harness::InstructionTier>,
    rank: u32,
    text: &'static str,
}

impl SystemPromptSection for ProductPolicy {
    fn id(&self) -> &'static str {
        self.id
    }

    fn tier(&self) -> nuo_harness::InstructionTier {
        self.tier.unwrap_or(nuo_harness::InstructionTier::Session)
    }

    fn rank(&self) -> u32 {
        self.rank
    }

    fn render(&self, _ctx: &SystemPromptContext) -> Option<String> {
        Some(self.text.to_owned())
    }
}

fn builder() -> nuo_harness::AgentBuilder {
    Agent::builder(Arc::new(IdleProvider), Vec::new(), AgentIdentity::default())
}

#[test]
fn embedding_can_extend_and_disable_prompt_policy_before_build() {
    let agent = builder()
        .register_system_prompt_section(ProductPolicy {
            id: "system.embedding.test.policy",
            tier: None,
            rank: 15,
            text: "PRODUCT-POLICY",
        })
        .unwrap()
        .disable_system_prompt_section("system.project_rules")
        .unwrap()
        .build();

    let mut messages = vec![Message::new(Role::User, "hello")];
    agent.prepare_request_messages_debug(&mut messages);

    assert_eq!(messages[0].role, Role::System);
    assert!(messages[0].content.contains("PRODUCT-POLICY"));
}

#[test]
fn embedding_configuration_errors_are_structured() {
    let result = builder().register_system_prompt_section(ProductPolicy {
        id: "system.identity_preamble",
        tier: None,
        rank: 1,
        text: "collision",
    });
    assert!(matches!(
        result,
        Err(SystemPromptRegistryError::DuplicateId("system.identity_preamble"))
    ));

    let result = builder().disable_system_prompt_section("system.missing");
    assert!(matches!(
        result,
        Err(SystemPromptRegistryError::UnknownId(id)) if id == "system.missing"
    ));
}

#[test]
fn default_prompt_is_free_of_nanny_prompting() {
    let agent = builder().build();
    let mut messages = vec![Message::new(Role::User, "test prompt")];
    agent.prepare_request_messages_debug(&mut messages);

    // Shipped default builder has no identity or project rules, so it renders
    // no system message, ensuring zero prompt clutter and zero attention theft.
    if let Some(system_msg) = messages.iter().find(|m| m.role == Role::System) {
        assert!(
            !system_msg.content.contains("ALWAYS prefer built-in tools"),
            "system message must not contain tool micromanagement"
        );
        assert!(
            !system_msg.content.contains("Finite Foreground Execution Axiom"),
            "system message must not contain execution axioms"
        );
        assert!(
            !system_msg.content.contains("See the task through to a real result"),
            "system message must not contain persistence lecturing"
        );
    }
}

#[test]
fn embedding_can_order_sections_semantically() {
    let agent = builder()
        .register_system_prompt_section(ProductPolicy {
            id: "system.embedding.first",
            tier: Some(nuo_harness::InstructionTier::Base),
            rank: 0,
            text: "FIRST-GUIDANCE",
        })
        .unwrap()
        .register_system_prompt_section(ProductPolicy {
            id: "system.embedding.second",
            tier: Some(nuo_harness::InstructionTier::Base),
            rank: 1,
            text: "SECOND-GUIDANCE",
        })
        .unwrap()
        .order_system_prompt_section(
            "system.embedding.first",
            nuo_harness::InstructionOrder::Before("system.embedding.second"),
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
