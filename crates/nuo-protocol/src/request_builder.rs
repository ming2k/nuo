//! The only conversation-to-request builder.
//!
//! `RequestBuilder` turns one [`RequestView`] plus one request-scoped
//! [`RequestContext`] into a provider-neutral [`ModelRequest`]. It is shared by
//! live execution, resume, replay, and branch switching; there is no second
//! instruction pipeline.
//!
//! The builder's containers are the request regions: `tools` and `fixed` form
//! the cacheable prefix, `messages` is the growing conversation, and
//! `transient` is request-local text emitted after the final cache breakpoint.
//! Tools and messages are moved into the prepared request exactly once. They
//! never enter the text blocks, so a provider cannot receive a duplicated tool
//! declaration or a message rendered as prose.
use crate::thread_view::RequestView;
use crate::{Block, InstructionText, Message, ModelRequest, ToolSpec, render_blocks};

/// Request-scoped inputs that are not conversation facts.
///
/// This value is assembled by the server/agent for one request and is never
/// persisted. It carries identity/rules text, provider guidance, and the tool
/// declarations visible to this request.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    /// Route-stable text blocks (identity, rules, project instructions).
    pub fixed: Vec<Block>,
    /// Request-local text blocks (steering notes, transient guidance).
    pub transient: Vec<Block>,
    /// Protocol tool declarations.
    pub tools: Vec<ToolSpec>,
}

impl RequestContext {
    pub fn new(fixed: Vec<Block>, transient: Vec<Block>, tools: Vec<ToolSpec>) -> Self {
        Self {
            fixed,
            transient,
            tools,
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_fixed(mut self, fixed: Vec<Block>) -> Self {
        self.fixed = fixed;
        self
    }

    pub fn with_transient(mut self, transient: Vec<Block>) -> Self {
        self.transient = transient;
        self
    }
}

/// One provider-neutral request, ready for projection.
#[derive(Debug, Clone, Default)]
pub struct RequestBuilder {
    /// Protocol tool declarations.
    pub tools: Vec<ToolSpec>,
    /// Route-stable text blocks.
    pub fixed: Vec<Block>,
    /// Conversation turns.
    pub messages: Vec<Message>,
    /// Request-local text blocks.
    pub transient: Vec<Block>,
}

impl RequestBuilder {
    /// Assemble the request from the only two request-time inputs.
    pub fn from_view(view: &RequestView, context: &RequestContext) -> Self {
        Self {
            tools: context.tools.clone(),
            fixed: context.fixed.clone(),
            messages: view.messages.clone(),
            transient: context.transient.clone(),
        }
    }

    /// Render the two text regions and hand every element to the prepared
    /// request exactly once.
    pub fn build(self) -> ModelRequest {
        let instructions =
            InstructionText::new(render_blocks(&self.fixed), render_blocks(&self.transient));
        ModelRequest::with_instructions_and_tool_specs(instructions, self.messages, self.tools)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread_view::RequestView;
    use crate::{BlockRender, Role, ThreadRevision};

    #[test]
    fn tools_and_messages_are_moved_once_and_never_rendered_as_text() {
        let view = RequestView {
            messages: vec![Message::new(Role::User, "hello")],
            revision: ThreadRevision(1),
            folded: Vec::new(),
        };
        let context = RequestContext::new(
            vec![
                Block::new("project.rules", "Follow the project rules.")
                    .with_title("Project Rules")
                    .with_render(BlockRender::Xml("policy")),
            ],
            vec![Block::new("request.transient", "current turn note")],
            vec![ToolSpec::new(
                "read",
                "read a file",
                serde_json::json!({"type": "object"}),
            )],
        );
        let request = RequestBuilder::from_view(&view, &context).build();

        assert_eq!(request.tool_specs.len(), 1);
        assert_eq!(request.tool_specs[0].name, "read");
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.messages[0].content, "hello");
        assert!(request.instructions.fixed.contains("<policy>"));
        assert!(
            request
                .instructions
                .fixed
                .contains("Follow the project rules.")
        );
        assert!(request.instructions.transient.contains("current turn note"));
        assert!(!request.instructions.fixed.contains("read"));
        assert!(!request.instructions.fixed.contains("hello"));
    }

    #[test]
    fn an_empty_context_still_produces_a_provider_neutral_request() {
        let view = RequestView {
            messages: vec![Message::new(Role::User, "hello")],
            revision: ThreadRevision(2),
            folded: Vec::new(),
        };
        let request = RequestBuilder::from_view(&view, &RequestContext::default()).build();
        assert!(request.instructions.is_empty());
        assert_eq!(request.messages.len(), 1);
        assert!(request.tool_specs.is_empty());
    }
}
