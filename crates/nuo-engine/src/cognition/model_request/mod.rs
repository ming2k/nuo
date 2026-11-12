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
    pub(crate) fn assemble_prepared(
        &self,
        messages: Vec<Message>,
        temporary_context: Vec<Message>,
        context: &InstructionContext,
        tools: &[Arc<dyn Tool>],
    ) -> nuo_protocol::ModelRequest {
        let instructions = self.instruction_registry.build_bundle(context);
        let tool_specs = tools
            .iter()
            .map(|t| {
                nuo_provider::capability::ToolSpec::new(t.name(), t.description(), t.parameters())
            })
            .collect();
        nuo_protocol::ModelRequest::with_instructions_and_tool_specs(
            instructions,
            messages,
            tool_specs,
        )
        .with_temporary_context(temporary_context)
    }

    /// Compile a [`nuo_protocol::ModelRequest`] directly from a canonical [`nuo_protocol::SessionIR`]
    /// via the 4-pass compiler pipeline (INV-EXEC-02).
    pub fn compile_from_ir(
        &self,
        ir: &nuo_protocol::SessionIR,
        temporary_context: Vec<Message>,
        tools: &[Arc<dyn Tool>],
        dialect: Option<String>,
        protocol: Option<nuo_protocol::WireProtocol>,
    ) -> Result<nuo_protocol::CompilationArtifact, nuo_protocol::CompilerError> {
        let tool_specs = tools
            .iter()
            .map(|t| {
                nuo_provider::capability::ToolSpec::new(t.name(), t.description(), t.parameters())
            })
            .collect();
        let options = nuo_protocol::CompilerOptions {
            tool_specs,
            temporary_context,
            ephemeral_instruction: None,
            target_dialect: dialect,
            target_protocol: protocol,
        };
        nuo_protocol::compile_session_request(ir, options)
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
    fn compile_from_ir_executes_compiler_passes() {
        let tool: Arc<dyn Tool> = Arc::new(TestTool);
        let assembler = ModelRequestAssembler::new(InstructionRegistry::new());
        let policy = nuo_protocol::SessionPolicy::default();
        let mut ir = nuo_protocol::SessionIR::new("test-session", policy, 1000);
        ir.append_message("node-1", 1001, Message::new(Role::User, "compile this"));

        let artifact = assembler
            .compile_from_ir(
                &ir,
                vec![],
                &[tool],
                Some("anthropic".into()),
                Some(nuo_protocol::WireProtocol::AnthropicMessages),
            )
            .expect("SessionIR compilation must succeed");

        assert_eq!(artifact.request.messages.len(), 1);
        assert_eq!(artifact.request.messages[0].content, "compile this");
        assert_eq!(artifact.request.tool_specs.len(), 1);
        assert_eq!(artifact.request.tool_specs[0].name, "inspect");
        assert!(!artifact.cache_boundary.prefix_fingerprint.is_empty());
        assert_eq!(artifact.stats.nodes_traversed, 1);
    }
}
