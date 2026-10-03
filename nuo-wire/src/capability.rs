//! Foundational capability traits: how the harness talks to a model
//! ([`Provider`]) and to tools ([`Tool`]), the stream events a provider emits
//! ([`ProviderStreamEvent`]).

use std::collections::HashMap;

/// Transient provider-owned state shared by requests in one user round.
/// Each provider namespaces its slots by route. Never serialized or included
/// in prompt fingerprints; dropping the round releases its routing tokens.
pub use nuo_model_codec::capability::ProviderTurnContext;

/// Per-model (and per-subagent-profile) variant selection: a map from a
/// capability name (a [`Tool::name`]) to the [`Tool::variant`] id chosen for
/// it. When the agent resolves its toolset for the active model, a capability
/// listed here is realized by its named variant; capabilities absent from the
/// map fall back to their default variant. This is how one logical toolset can
/// hand different models a genuinely different *implementation* of a tool
/// (different description, schema, and behaviour) rather than a re-worded copy
/// of a single impl.
///
/// Configured per model id under `[tool_variants."<model-id>"]` in
/// `config.toml`; the agent selects the map matching `Provider::model()`.
/// Subagent profiles carry their own static selection (see
/// [`crate::SubAgentProfile::variant_pins`]).
pub type VariantSelection = HashMap<String, String>;

/// Narrow prompt hints exposed by a concrete provider implementation.
///
/// The provider owns protocol facts (for example, how tool results or
/// thinking replay are represented on its wire surface), while the agent owns
/// whether and where those facts are inserted into model context. Empty by
pub use nuo_model_codec::capability::ProviderPromptHints;

/// One immutable, provider-agnostic model request.
///
/// A provider-neutral tool declaration. This is the canonical, vendor-agnostic
/// shape the harness carries: adapters translate it into each provider's wire
/// format (OpenAI `{type:"function", function:{...}}`, Anthropic
/// `{name, description, input_schema}`, Google `functionDeclarations`, etc.).
///
/// Replacing the previous OpenAI-shape `serde_json::Value` canonical form
/// removes the coupling where every adapter had to reverse-engineer the OpenAI
/// nesting (`spec["function"]["name"]`) — adapters now read typed fields.
pub use nuo_model_codec::ToolSpec;
pub use nuo_model_codec::capability::ModelRequest;


/// A shared empty [`VariantSelection`] map, handy as a default borrow target so
/// callers can always hand out `&VariantSelection` without an `Option`.
pub fn empty_variant_selection() -> &'static VariantSelection {
    static EMPTY: std::sync::LazyLock<VariantSelection> =
        std::sync::LazyLock::new(VariantSelection::new);
    &EMPTY
}

pub use nuo_model_codec::capability::{
    Provider, ProviderEventStream, ProviderStreamEvent, ProviderTextStream,
};


/// Runtime input supervisor for a supervised command invocation. Relocated to
/// the tool leaf (`nuo_tool::stream::InputHandler`) per ADR-0008; re-exported
/// here for existing call sites.
pub use nuo_tool::stream::InputHandler;

/// Everything a tool needs for one invocation beyond its own state. Relocated to
/// the tool leaf (`nuo_tool::context::ToolInvocation`) per ADR-0008; re-exported
/// here for existing call sites.
pub use nuo_tool::context::ToolInvocation;

// `nuo_wire::Tool` is now an alias of the single tool contract in the leaf
// (`nuo_tool::Tool`) per ADR-0008 §1. All vocabulary it needs (ToolOutput,
// ScopeTarget, ToolAccesses, HazardLevel, ToolInvocation, InputHandler,
// SubagentEvent, ToolStream) lives in `nuo-tool`; this re-export keeps every
// existing `impl nuo_wire::Tool` and `dyn nuo_wire::Tool` site compiling.
pub use nuo_tool::Tool;

/// What a tool call acts on, so the operation-scope gate can match it against
/// the agent's granted scope. Relocated to the tool leaf (`nuo_tool::ScopeTarget`)
/// per ADR-0008; re-exported here for existing call sites.
pub use nuo_tool::ScopeTarget;

#[cfg(test)]
mod tests {
    use super::Tool;

    #[test]
    fn provider_turn_context_is_transient_and_not_prompt_content() {
        let request = super::ModelRequest::new(Vec::new());
        let before = serde_json::to_value(&request).unwrap();
        let token = request.turn_context.slot("route".into());
        token.set("opaque-token".into()).unwrap();
        let retry = request.clone();
        assert!(std::sync::Arc::ptr_eq(
            &request.turn_context,
            &retry.turn_context
        ));
        assert_eq!(serde_json::to_value(&retry).unwrap(), before);
        assert!(!format!("{request:?}").contains("opaque-token"));
        assert!(
            super::ModelRequest::ephemeral(Vec::new())
                .turn_context
                .slot("route".into())
                .get()
                .is_none()
        );
    }

    /// A minimal [`Tool`] stand-in so the schema tests can run without pulling
    /// in the whole tool crate.
    struct DummyTool {
        name: &'static str,
        variant: &'static str,
        desc: &'static str,
    }

    #[async_trait::async_trait]
    impl super::Tool for DummyTool {
        fn name(&self) -> &str {
            self.name
        }
        fn variant(&self) -> &str {
            self.variant
        }
        fn description(&self) -> &str {
            self.desc
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok(String::new())
        }
    }

    fn desc_of(schema: &serde_json::Value) -> &str {
        schema["function"]["description"].as_str().unwrap_or("")
    }

    #[test]
    fn variant_defaults_to_default() {
        let tool = DummyTool {
            name: "read_text",
            variant: "default",
            desc: "built-in",
        };
        assert_eq!(tool.variant(), "default");
    }

    #[test]
    fn function_schema_uses_the_variant_own_description() {
        // A variant's own description is authoritative: the function schema
        // carries it verbatim, keyed by the shared capability name.
        let terse = DummyTool {
            name: "read_text",
            variant: "terse",
            desc: "terse wording",
        };
        let schema = terse.to_openai_function();
        assert_eq!(schema["function"]["name"], "read_text");
        assert_eq!(desc_of(&schema), "terse wording");
    }
}
