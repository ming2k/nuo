//! Request-scoped model-request assembly (ADR-0056).
//!
//! The assembler is intentionally independent of [`crate::Agent`]. The agent
//! owns when assembly occurs and supplies a plain state snapshot; this module
//! owns the pure projection from a live conversation window to one immutable
//! [`nuo_protocol::ModelRequest`].
pub(crate) mod instruction;
pub mod policies;

pub use policies::default_instruction_registry;

use std::sync::Arc;

use crate::{InstructionContext, InstructionRegistry, Message, Role, Tool};

/// Pure request projector configured with the system-prompt policy for one
/// agent. It owns no live agent state and performs no persistence.
pub(crate) struct ModelRequestAssembler {
    instruction_registry: InstructionRegistry,
}

impl ModelRequestAssembler {
    pub(crate) fn new(instruction_registry: InstructionRegistry) -> Self {
        Self {
            instruction_registry,
        }
    }

    pub(crate) fn registry_mut(&mut self) -> &mut InstructionRegistry {
        &mut self.instruction_registry
    }

    pub(crate) fn replace_registry(&mut self, registry: InstructionRegistry) {
        self.instruction_registry = registry;
    }

    /// Project a conversation window without mutating historical nodes.
    /// Test/secondary callers; the hot path uses [`Self::assemble_prepared`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn assemble(
        &self,
        window: &[Message],
        context: &InstructionContext,
        tools: &[Arc<dyn Tool>],
    ) -> nuo_protocol::ModelRequest {
        let mut messages = window.to_vec();
        crate::cognition::agent::remove_empty_assistant_messages(&mut messages);
        messages.retain(|message| message.role != Role::System && !message.is_command_echo());
        self.assemble_prepared(messages, Vec::new(), context, tools)
    }

    /// Assemble from an already-filtered, owned message list. The hot path
    /// (`Agent::model_request`) builds that list with a single clone and
    /// hands it over — no second copy of the window per turn.
    ///
    /// `temporary_context` is the request-local `E_n` tail:
    /// it is attached to the snapshot but excluded from the cacheable prefix and
    /// never enters durable history.
    // A section table with a missing anchor or a cycle is a programming error
    // in the compiled policy, not a recoverable runtime condition. Failing hard
    // here keeps a corrupt layout from silently misordering a request.
    #[allow(clippy::panic)]
    pub(crate) fn assemble_prepared(
        &self,
        messages: Vec<Message>,
        temporary_context: Vec<Message>,
        context: &InstructionContext,
        tools: &[Arc<dyn Tool>],
    ) -> nuo_protocol::ModelRequest {
        let regions = self
            .instruction_registry
            .build_regions(context)
            .unwrap_or_else(|error| panic!("invalid instruction layout: {error}"));
        let tools = tools
            .iter()
            .map(|t| {
                nuo_provider::capability::ToolSpec::new(t.name(), t.description(), t.parameters())
            })
            .collect();
        nuo_protocol::RequestBuilder {
            tools,
            fixed: regions.fixed,
            messages,
            transient: regions.transient,
        }
        .build()
        .with_temporary_context(temporary_context)
    }

    /// Assemble a request from a durable thread projection.
    ///
    /// This is the replay/branch-switch entry point: the fact window is
    /// projected to a `RequestView`, then the one `RequestBuilder` renders the
    /// final request. It never reads or mutates `SessionIR`.
    #[allow(clippy::panic)]
    pub(crate) fn assemble_thread(
        &self,
        thread: &nuo_protocol::Thread,
        branch: &nuo_protocol::BranchId,
        temporary_context: Vec<Message>,
        context: &InstructionContext,
        tools: &[Arc<dyn Tool>],
    ) -> Result<nuo_protocol::ModelRequest, nuo_protocol::ThreadError> {
        let window = nuo_protocol::ThreadWindow::from_thread(thread, branch, usize::MAX)?;
        let view = nuo_protocol::RequestView::from_window(&window);
        let regions = self
            .instruction_registry
            .build_regions(context)
            .unwrap_or_else(|error| panic!("invalid instruction layout: {error}"));
        let tools = tools
            .iter()
            .map(|t| {
                nuo_provider::capability::ToolSpec::new(t.name(), t.description(), t.parameters())
            })
            .collect();
        let request_context =
            nuo_protocol::RequestContext::new(regions.fixed, regions.transient, tools);
        Ok(
            nuo_protocol::RequestBuilder::from_view(&view, &request_context)
                .build()
                .with_temporary_context(temporary_context),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestTool;

    #[async_trait::async_trait]
    impl Tool for TestTool {
        fn name(&self) -> &str {
            "inspect"
        }

        fn description(&self) -> &str {
            "Inspect the request boundary"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("ok".to_string())
        }
    }

    #[test]
    fn assemble_projects_window_deterministically() {
        let tool: Arc<dyn Tool> = Arc::new(TestTool);
        let assembler = ModelRequestAssembler::new(InstructionRegistry::new());
        let window = vec![
            Message::new(Role::User, "hello"),
            Message::new(Role::Assistant, "hi"),
        ];
        let context = InstructionContext::empty();
        let request = assembler.assemble(&window, &context, &[Arc::clone(&tool)]);
        // Transcript turns are pure conversation messages.
        assert_eq!(request.messages[0].role, Role::User);
        assert_eq!(request.messages[0].content, "hello");
        assert_eq!(request.messages[1].role, Role::Assistant);
        assert_eq!(request.messages[1].content, "hi");
        // Tool spec travels with the request.
        assert_eq!(request.tool_specs.len(), 1);
        assert_eq!(request.tool_specs[0].name, "inspect");
        // Repeat assembly of the same window yields identical bytes.
        let again = assembler.assemble(&window, &context, &[tool]);
        assert_eq!(
            request.instructions, again.instructions,
            "system prompt instructions must be byte-stable across assemblies"
        );
    }

    #[test]
    fn assemble_thread_projects_a_branch_and_adds_tools() {
        let tool: Arc<dyn Tool> = Arc::new(TestTool);
        let assembler = ModelRequestAssembler::new(InstructionRegistry::new());
        let mut thread = nuo_protocol::Thread::new(nuo_protocol::ThreadId::new("t"), 1000);
        thread
            .append(
                nuo_protocol::FactPayload::Message {
                    message: Message::new(Role::User, "compile this"),
                },
                nuo_protocol::FactContext::default(),
                1001,
            )
            .unwrap();

        let request = assembler
            .assemble_thread(
                &thread,
                &nuo_protocol::BranchId::main(),
                Vec::new(),
                &InstructionContext::empty(),
                &[tool],
            )
            .expect("thread projection must succeed");

        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.messages[0].content, "compile this");
        assert_eq!(request.tool_specs.len(), 1);
        assert_eq!(request.tool_specs[0].name, "inspect");
    }
}
