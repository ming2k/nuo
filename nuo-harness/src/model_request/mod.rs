//! Request-scoped model-request assembly (ADR-0056 / ADR-0160).
//!
//! The assembler is intentionally independent of [`crate::Agent`]. The agent
//! owns when assembly occurs and supplies a plain state snapshot; this module
//! owns the pure projection from a live conversation window to one immutable
//! [`nuo_contracts::ModelRequest`].

pub mod policies;
pub(crate) mod system_prompt;

pub(crate) use policies::default_system_prompt_registry;

use std::sync::Arc;

use crate::{Message, Role, SystemPromptContext, SystemPromptRegistry, Tool};

/// Pure request projector configured with the system-prompt policy for one
/// agent. It owns no live agent state and performs no persistence.
pub(crate) struct ModelRequestAssembler {
    system_prompt_registry: SystemPromptRegistry,
}

impl ModelRequestAssembler {
    pub(crate) fn new(system_prompt_registry: SystemPromptRegistry) -> Self {
        Self {
            system_prompt_registry,
        }
    }

    pub(crate) fn registry_mut(&mut self) -> &mut SystemPromptRegistry {
        &mut self.system_prompt_registry
    }

    pub(crate) fn replace_registry(&mut self, registry: SystemPromptRegistry) {
        self.system_prompt_registry = registry;
    }

    /// Project a conversation window without mutating historical nodes.
    /// Test/secondary callers; the hot path uses [`Self::assemble_prepared`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn assemble(
        &self,
        window: &[Message],
        context: &SystemPromptContext,
        tools: &[Arc<dyn Tool>],
    ) -> nuo_contracts::ModelRequest {
        let mut messages = window.to_vec();
        crate::agent::remove_empty_assistant_messages(&mut messages);
        messages.retain(|message| message.role != Role::System && !message.is_command_echo());
        self.assemble_prepared(messages, Vec::new(), context, tools)
    }

    /// Assemble from an already-filtered, owned message list. The hot path
    /// (`Agent::model_request`) builds that list with a single clone and
    /// hands it over — no second copy of the window per turn (ADR-0187).
    ///
    /// `temporary_context` is the request-local `E_n` tail (ADR-0213/ADR-0217):
    /// it is attached to the snapshot but excluded from the cacheable prefix and
    /// never enters durable history.
    pub(crate) fn assemble_prepared(
        &self,
        messages: Vec<Message>,
        temporary_context: Vec<Message>,
        context: &SystemPromptContext,
        tools: &[Arc<dyn Tool>],
    ) -> nuo_contracts::ModelRequest {
        let instructions = self.system_prompt_registry.build_bundle(context);
        nuo_contracts::ModelRequest::with_instructions_and_tools(instructions, messages, tools)
            .with_temporary_context(temporary_context)
    }

    /// Compile a [`nuo_contracts::ModelRequest`] directly from a canonical [`nuo_contracts::SessionIR`]
    /// via the 4-pass compiler pipeline (ADR-0241/ADR-0249, INV-EXEC-02).
    pub fn compile_from_ir(
        &self,
        ir: &nuo_contracts::SessionIR,
        temporary_context: Vec<Message>,
        tools: &[Arc<dyn Tool>],
        dialect: Option<String>,
        protocol: Option<nuo_contracts::WireProtocol>,
    ) -> Result<nuo_contracts::CompilationArtifact, nuo_contracts::CompilerError> {
        let tool_specs = tools
            .iter()
            .map(|t| nuo_contracts::ToolSpec::from_tool(t.as_ref()))
            .collect();
        let options = nuo_contracts::CompilerOptions {
            tool_specs,
            temporary_context,
            ephemeral_instruction: None,
            target_dialect: dialect,
            target_protocol: protocol,
        };
        nuo_contracts::compile_session_request(ir, options)
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
        let assembler = ModelRequestAssembler::new(SystemPromptRegistry::new());
        let window = vec![
            Message::new(Role::User, "hello"),
            Message::new(Role::Assistant, "hi"),
        ];
        let context = SystemPromptContext::empty();
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
        let assembler = ModelRequestAssembler::new(SystemPromptRegistry::new());
        let policy = nuo_contracts::SessionPolicy::default();
        let mut ir = nuo_contracts::SessionIR::new("test-session", policy, 1000);
        ir.append_message("node-1", 1001, Message::new(Role::User, "compile this"));

        let artifact = assembler
            .compile_from_ir(
                &ir,
                vec![],
                &[tool],
                Some("anthropic".into()),
                Some(nuo_contracts::WireProtocol::AnthropicMessages),
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
