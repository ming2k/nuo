//! Foundational capability traits: how the harness talks to a model
//! ([`Provider`]) and to tools ([`Tool`]), the stream events a provider emits
//! ([`ProviderStreamEvent`]).

use crate::tool_access::ToolAccesses;
use crate::{SubagentEvent, ToolOutput, ToolStream};
use async_trait::async_trait;
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


/// Runtime input supervisor for a supervised command invocation. Implemented
/// by the agent layer (which owns the human-input channel) and handed to the
/// command tool through [`Tool::input_handler`]. Kept as a trait so
/// `muta-contracts` stays free of agent/async-runtime coupling: the command
/// tool's examiner calls it, the agent fulfils it.
#[async_trait]
pub trait InputHandler: Send + Sync {
    /// Resolve one runtime input prompt. `Ok(Some(line))` — the operator's
    /// answer, written into the reported channel. `Ok(None)` — no answer
    /// (declined, or no reachable human channel); the caller kills the child
    /// with [`ShellTermination::InputUnanswered`](crate::ShellTermination::InputUnanswered).
    async fn resolve(&self, prompt: crate::tool_output::InputPrompt) -> Option<String>;
}

/// Everything a tool needs for one invocation beyond its own state: the call
/// identity, the raw arguments, the input-execution contract, and the runtime
/// input supervisor (when the dispatch layer supplied one). Bundled so the
/// trait method stays stable as per-call context grows.
pub struct ToolInvocation<'a> {
    /// The dispatch-generated call id (keys live streams and subagent views).
    pub call_id: &'a str,
    /// Raw JSON tool arguments exactly as the model emitted them.
    pub arguments: &'a str,
    /// How the child's input channels are provisioned (command tool only;
    /// other tools ignore it).
    pub input: crate::tool_output::InputContract,
    /// Runtime input supervisor for a supervised child. The dispatch layer
    /// builds it per invocation (it captures the live event channel), so it is
    /// borrowed rather than owned for the tool's lifetime.
    pub input_handler: Option<&'a dyn InputHandler>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;

    /// Compatibility aliases that can resolve to this tool during dispatch.
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// Whether this tool matches the requested dispatch name.
    fn matches_name(&self, requested: &str) -> bool {
        self.name() == requested || self.aliases().contains(&requested)
    }

    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;

    /// The variant id distinguishing this implementation from other variants of
    /// the same capability ([`name`](Self::name)). A capability with a single
    /// implementation uses the default; multiple variants of one capability
    /// share `name()` and differ only in `variant()`. The variant id never
    /// reaches the model — it is the selection key under
    /// `[tool_variants."<model-id>"]` config and in subagent profiles, by which
    /// a model or profile picks which implementation of a capability it sees.
    fn variant(&self) -> &str {
        "default"
    }

    /// Whether this tool is currently available/configured and should be admitted to
    /// model requests. Returning `false` hides the tool definition from prompts (saving
    /// tool schema tokens) and prevents model dispatch.
    fn is_available(&self) -> bool {
        true
    }

    /// Whether executing this tool may block awaiting a live human decision
    /// (e.g. `ask_user`, an approval-gated mode switch). Non-interactive
    /// execution contexts — subagents spawned for autonomous research — have
    /// no user reachable to answer, so a [`crate::subagent::ToolPolicy`] with
    /// `allow_user_interaction: false` excludes these. See ADR-0011.
    fn requires_user(&self) -> bool {
        false
    }

    /// Whether invoking this tool spawns a nested sub-agent (ADR-0183).
    fn spawns_subagent(&self) -> bool {
        false
    }

    /// Whether this tool cooperates with the harness's turn cancellation by
    /// observing [`Tool::request_cancel`] and draining its in-flight call to a
    /// terminal result instead of requiring the harness to drop the future.
    ///
    /// The default is `false`: most tools (bash, file I/O, web) cannot stop
    /// mid-call, so the harness keeps its fast drop-based cancellation for
    /// them. A tool that runs a nested agent (e.g. `task`) opts in because its
    /// in-flight call *owns a partial transcript* worth preserving — dropping
    /// it would discard real work the user may want to resume.
    fn supports_cooperative_cancel(&self) -> bool {
        false
    }

    /// Best-effort cooperative cancellation of an in-flight call identified by
    /// the harness-assigned `call_id`. The harness calls this when the user
    /// interrupts a turn, *then* waits a bounded grace period for the call to
    /// return a terminal result. Returns `true` if the tool accepted the
    /// request (it will stop at its next safe boundary); `false` if the call
    /// is unknown or already finished — the harness then falls back to
    /// dropping the future.
    ///
    /// Only consulted when [`Tool::supports_cooperative_cancel`] is `true`.
    /// The default rejects every request so non-cooperative tools keep their
    /// unchanged drop semantics.
    fn request_cancel(&self, _call_id: &str) -> bool {
        false
    }

    /// Whether this tool only functions on a model that can see images
    /// (vision). A vision-only tool (e.g. `read_image`, which feeds the model
    /// an image part) is useless — or actively misleading — on a text-only
    /// model, which strips image parts before the request hits the wire. This
    /// is a **model-capability requirement**, the symmetric counterpart of
    /// [`requires_user`](Self::requires_user): where that gates on whether a
    /// human is reachable, this gates on whether the model can perceive the
    /// tool's output.
    ///
    /// The pool resolver ([`crate::ToolSet::resolve_for`]) treats it as a
    /// **hard** filter: a variant whose `requires_vision()` a model cannot
    /// satisfy is never selectable for that model — it is simply absent from
    /// the resolved set, so no agent-side override can reinstate it. This is
    /// why model capability limits live on the scope/pool axis, not the soft
    /// override axis.
    fn requires_vision(&self) -> bool {
        false
    }

    /// Whether this tool exercises control over the harness itself (e.g. the
    /// abort/exit escape hatch), as opposed to the workspace/filesystem. This
    /// is orthogonal to [`Tool::scope_target`]: `scope_target` classifies *what
    /// the call touches*, while this classifies *process control*. Subagent
    /// profiles exclude control tools unconditionally — a spawned agent must
    /// never be able to tear down the whole program. A control tool bypasses
    /// the permission broker and scope gate entirely: it declares no
    /// [`ScopeTarget`] (the default [`ScopeTarget::Unspecified`]), so neither
    /// the scope gate nor the broker fires for it — it is gated solely by this
    /// flag.
    fn affects_control_flow(&self) -> bool {
        false
    }

    /// The operation target this call acts on, so the operation-scope gate can
    /// decide whether the call falls inside the agent's granted scope.
    ///
    /// Tools return a typed [`ScopeTarget`]: a file path for `write_file`/
    /// `edit_text`, the command string for `bash`, etc. The scope gate
    /// dispatches on the variant — `Path` targets are checked against the
    /// granted directory prefixes, `Command` targets against a command
    /// allowlist. [`ScopeTarget::Unspecified`] (the default) is admitted
    /// without a scope check, since the tool declares no locatable target.
    ///
    /// Like [`permission_label`](Self::permission_label), this never reaches
    /// the model.
    fn scope_target(&self, _arguments: &str) -> ScopeTarget {
        ScopeTarget::Unspecified
    }

    /// What this call touches, so the scheduler can decide whether it may run
    /// concurrently with the other calls in its batch. Returns a declarative
    /// [`ToolAccesses`] list consumed by the harness's concurrency scheduler.
    ///
    /// The **default** derives a *conservative* declaration from
    /// [`scope_target`](Self::scope_target), so existing tools get correct
    /// (if coarse) concurrency without override:
    ///
    /// | `scope_target` | derived `accesses` | concurrency effect |
    /// |---|---|---|
    /// | `Unspecified` | `none()` | freely parallelizable |
    /// | `Path(p)` | `read_write_file(p)` | serializes with any access to `p` |
    /// | `Command(_)` | `all()` | serializes with everything in the batch |
    ///
    /// Tools override this to declare a **precise** access (e.g. `read_file`
    /// for read-only tools, `search_tree` for content search, `read_tree` for a
    /// directory listing). Like [`scope_target`](Self::scope_target), this
    /// never reaches the model.
    fn accesses(&self, arguments: &str) -> ToolAccesses {
        match self.scope_target(arguments) {
            ScopeTarget::Unspecified => ToolAccesses::none(),
            ScopeTarget::Path(path) => {
                ToolAccesses::read_write_file(path.to_string_lossy().into_owned())
            }
            ScopeTarget::Command(_) => ToolAccesses::all(),
        }
    }

    /// Short, human-friendly label shown as the title of the permission
    /// prompt for `Write` tools. Defaults to the raw [`Tool::name`], which is
    /// fine when the name itself reads as a label (e.g. `bash`, `write_file`).
    /// Override when the name is a synthetic identifier whose meaning is not
    /// obvious to a user. Only consulted for tools that actually trigger a
    /// permission prompt.
    ///
    /// This is purely a UI string; it never reaches the model and is not
    /// part of the function schema sent to providers.
    fn permission_label(&self) -> String {
        self.name().to_string()
    }

    /// User-facing description shown in the body of the permission prompt
    /// (the "Details" section). Defaults to [`Tool::description`], which is
    /// appropriate when that text is written for humans. Override when
    /// [`Tool::description`] is model-facing instruction prose (constraints
    /// aimed at the model rather than a description of the call's effect)
    /// that would confuse a user reading the prompt. Keep overrides to one
    /// or two plain sentences describing *what the call does*, not *when
    /// the model should call it*.
    ///
    /// Like [`permission_label`](Self::permission_label), this never reaches
    /// the model.
    fn permission_description(&self) -> String {
        self.description().to_string()
    }

    /// Threat / hazard classification of this tool.
    ///
    /// Read-only / inspection tools return `HazardLevel::Safe` (default).
    /// Destructive or executing tools return their specific `HazardLevel`.
    fn hazard_level(&self) -> crate::hazard::HazardLevel {
        crate::hazard::HazardLevel::Safe
    }

    /// Build the tool-specific submission to the permission handler for a given set of arguments.
    ///
    /// Safe tools return `None` (no permission evaluation or prompt needed).
    /// Dangerous tools (file modification, command execution) submit their structured
    /// intent payload (file paths, command line + process kill spec).
    fn permission_submission(
        &self,
        arguments: &str,
    ) -> Option<crate::hazard::ToolPermissionSubmission> {
        if !self.hazard_level().requires_permission() {
            return None;
        }
        Some(crate::hazard::ToolPermissionSubmission {
            hazard_level: self.hazard_level(),
            label: self.permission_label(),
            description: self.permission_description(),
            scope: match self.scope_target(arguments) {
                crate::ScopeTarget::Command(c) => c,
                crate::ScopeTarget::Path(p) => p.to_string_lossy().into_owned(),
                crate::ScopeTarget::Unspecified => self.name().to_string(),
            },
            payload: crate::hazard::ToolPermissionPayload::Generic {
                summary: format!("Execute tool '{}'", self.name()),
                details: serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null),
            },
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String>;

    /// Structured result. Default delegates to [`call`](Self::call), wrapping
    /// the text as [`ToolOutput::Text`]. Tools override this to return richer
    /// variants (e.g. a shell exit code, a file patch) so callers render from
    /// data instead of string-sniffing. See ADR-0001. Migration is additive:
    /// unmigrated tools keep working through this default.
    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        self.call(arguments).await.map(ToolOutput::text)
    }

    /// Structured, event-emitting execution — the method the harness actually
    /// invokes so typed output reaches the transcript. Default delegates to
    /// [`call_structured`](Self::call_structured) and emits no events. Tools
    /// that spawn subagents (e.g. `task`) override this to forward child
    /// events while still returning a [`ToolOutput`] (typically [`ToolOutput::Text`]).
    ///
    /// [`ToolInvocation::input`] is the **execution contract** for how a child's
    /// input channels are provisioned ([`InputContract`]). It is decided
    /// *before* spawn by the agent dispatch layer (never from the model's
    /// arguments). The default [`InputContract::Sealed`] keeps tools that
    /// ignore input correct: a child that blocks on `read(stdin)` gets instant
    /// EOF instead of hanging silently until the wall-clock timeout.
    async fn call_structured_with_events<'a>(
        &self,
        invocation: ToolInvocation<'a>,
        _on_event: Box<dyn FnMut(SubagentEvent) + Send + 'a>,
        _on_stream: &mut (dyn FnMut(ToolStream) + Send + 'a),
    ) -> Result<ToolOutput, String> {
        self.call_structured(invocation.arguments).await
    }

    /// Execute the tool while optionally emitting events (e.g. subagent steps).
    ///
    /// The default implementation simply calls `call()` and emits no events.
    /// Tools that spawn subagents can override this to stream child events back
    /// to the parent harness.
    async fn call_with_events<'a>(
        &self,
        _call_id: &str,
        arguments: &str,
        _on_event: Box<dyn FnMut(SubagentEvent) + Send + 'a>,
    ) -> Result<String, String> {
        self.call(arguments).await
    }

    /// Generate an OpenAI-compatible function schema for this tool. This is the
    /// authoritative schema for the variant; per-model differences are expressed
    /// by selecting a different variant, not by patching this output.
    fn to_openai_function(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name(),
                "description": self.description(),
                "parameters": self.parameters(),
            }
        })
    }
}

/// What a tool call acts on, so the operation-scope gate can match it against
/// the agent's granted scope. Tools report this via [`Tool::scope_target`];
/// each variant names a locatable target a tool may report. [`ScopeTarget::Unspecified`] is the default for tools with no locatable target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeTarget {
    /// A filesystem path the tool writes or reads (e.g. `write_file`, `edit_text`).
    /// Checked against the scope's granted directory prefixes.
    Path(std::path::PathBuf),
    /// A shell command string (e.g. `bash`). Checked against the scope's command
    /// allowlist, when one is set.
    Command(String),
    /// The tool declares no locatable target (e.g. `search_text`, `list_dir`). Admitted
    /// by the scope gate without a dimension check.
    Unspecified,
}

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
