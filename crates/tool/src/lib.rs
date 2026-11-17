//! Canonical tool specification, capability definitions, and execution runtime
//! for the Nuo agent ecosystem.
//!
//! This crate provides zero-agent-runtime contracts allowing independent development
//! and integration of tools, dynamic closures, MCP bridges, and security policies.
//!
//! ## The contract
//!
//! A tool is one trait with **one** execution entry point:
//! [`Tool::execute`], taking validated, normalized JSON arguments and returning
//! a typed [`ToolOutput`]. There is deliberately no second string-based entry
//! point: argument parsing, schema validation, and coercion happen exactly once,
//! in the dispatcher, so every tool — built-in, MCP, or test double — sees the
//! same normalized value and the same error envelope.
//!
//! Streaming and subagent lifecycle events are delivered through
//! [`Tool::execute_streaming`], whose default implementation delegates to
//! [`Tool::execute`]. A tool that does not stream simply does not override it.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

extern crate self as tool;

pub mod access;
pub mod args;
pub mod builtin;
pub mod completion;
pub mod context;
pub mod document;
pub mod effect;
pub mod error;
pub mod events;
pub use events::*;
pub mod exposure;
pub mod guard;
pub mod media;
pub mod mention;
pub mod message;
pub mod output;
pub mod preview;
pub mod residency;
pub mod risk;
pub mod scope;
pub mod skills_config;
pub mod stream;
pub mod todos;
pub mod tokenizer;
pub mod tool_output;
pub mod usage;
pub mod validation;
pub mod vfs;

pub use access::{ToolAccess, ToolAccesses, ToolFileAccessOperation, normalize_access_path};
pub use builtin::BuiltinTool;
pub use completion::{
    CommandAlias, CommandCatalog, CommandEffect, CommandExample, CommandSpec,
    CommandSubcommandSpec, CommandSuggestion, ComposerCompletion, ComposerCompletionKind,
    InputCompletion, InputCompletionKind,
};
pub use context::{ServiceMap, ServiceMapBuilder, ToolContext, ToolInvocation, ToolStreamSink};
pub use document::{DocumentError, DocumentExtractor, ExtractedDocument};
pub use effect::EffectClass;
pub use effect::reconciling;
pub use error::{Result, ToolError};
pub use exposure::ToolExposure;
pub use guard::TrajectoryGuardConfig;
pub use media::{ImageNormalizer, NormalizedImage};
pub use mention::*;
pub use message::{
    HookEventKind, ImagePart, InjectionKind, InjectionOrigin, Message, Role, SubagentMeta,
    ToolCall, ToolResult,
};
pub use output::ToolOutput;
pub use preview::{PREVIEW_TIMEOUT, PreviewResult};
pub use residency::{CaptureEligibility, classify as classify_capture_eligibility};
pub use risk::RiskProfile;
pub use scope::{ScopeTarget, ToolScope};
pub use skills_config::SkillsConfig;
pub use stream::{
    InputContract, InputExpectation, InputHandler, InputPrompt, ShellLine, ShellStream,
    ShellTermination, ToolProgress, ToolStream,
};
pub use todos::{MAX_TODOS, TodoId, TodoItem, TodoList, TodoStatus, unix_now};
pub use tokenizer::{StreamingCounter, Tokenizer, count_tokens, truncate_to_tokens};
pub use tool_output::{PatchOp, WebSearchHit, normalize_carriage_returns, truncate_utf8};
pub use usage::TokenUsage;
pub use validation::{
    CompiledSchema, PreparedArguments, compile_schema, compile_schema_cached,
    prepare_tool_arguments, validate_tool_output,
};
pub use vfs::{
    VFS_ROOT_PREFIX, VirtualFileResolver, extract_salient_signatures, extract_virtual_handle,
    is_virtual_path, spool_canonical_path, spool_root_dir, vfs_canonical_path, vfs_root_dir,
    vfs_short_hash,
};

pub use args::ToolArgs;
pub use tool_derive::{ToolArgs, ToolSchema};

use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

/// Core interface for tools callable by an agent.
///
/// Designed to be decoupled from specific agent loop runtimes and errors,
/// tools only require JSON-compatible schemas and arguments, providing
/// standardized execution, cancellation awareness, and risk declarations.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Distinct identifier of the tool (e.g. `read_file`, `jira_create_issue`).
    fn name(&self) -> &str;

    /// Natural language description of what the tool does and how the model should use it.
    fn description(&self) -> &str;

    /// JSON schema describing the accepted input arguments.
    ///
    /// A tool implements exactly this method. The dispatcher compiles it once,
    /// validates and coerces every call against it, and only then invokes
    /// [`Tool::execute`]; a tool therefore never receives arguments that
    /// violate its declared top-level object shape.
    fn parameters_schema(&self) -> Value;

    /// JSON schema describing the structured result payload, when the tool
    /// produces one.
    ///
    /// Declaring this is a promise: a successful call must return
    /// [`ToolOutput::Structured`] whose `value` validates against the schema.
    /// The dispatcher enforces that promise; a tool that cannot describe its
    /// output leaves this as `None`.
    fn output_schema(&self) -> Option<Value> {
        None
    }

    /// Declared capability and risk profile of the tool for policy reasoning.
    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    /// Declared operational scopes for dynamic tool assembly and stage gating.
    fn scopes(&self) -> Vec<ToolScope> {
        match self.risk_profile() {
            RiskProfile::ReadOnly => vec![ToolScope::ReadOnly],
            RiskProfile::IdempotentMutation => vec![ToolScope::Workspace],
            RiskProfile::NetworkAccess => vec![ToolScope::Network],
            RiskProfile::Destructive => vec![ToolScope::Workspace, ToolScope::Execution],
            RiskProfile::ArbitraryExecution => vec![ToolScope::Execution],
        }
    }

    // ── Metadata surface ─────────────────────────────────────────────────────
    // The trait accessors are the canonical metadata. `ToolSpec` is the
    // provider-facing projection; there is no second descriptor object.

    /// Compatibility aliases that can resolve to this tool during dispatch.
    ///
    /// Aliases exist for model interoperability (`bash` → `execute_command`,
    /// `grep` → `search_text`), not for versioning: an alias must resolve to
    /// exactly one tool, and two tools must never claim the same alias.
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// Whether this tool matches the requested dispatch name.
    fn matches_name(&self, requested: &str) -> bool {
        self.name() == requested || self.aliases().contains(&requested)
    }

    /// If this tool is an official built-in tool, return its typed enum variant.
    fn builtin(&self) -> Option<BuiltinTool> {
        BuiltinTool::from_name(self.name())
    }

    /// The variant id distinguishing this implementation from other variants of
    /// the same capability.
    fn variant(&self) -> &str {
        "default"
    }

    /// Whether this tool is currently available/configured.
    fn is_available(&self) -> bool {
        true
    }

    /// How this tool is exposed to the model.
    ///
    /// `Direct` tools are declared in every request. `Deferred` tools are
    /// reachable only after `tool_search` activates them, which keeps a large
    /// MCP catalog out of the prompt until it is needed. `Hidden` tools are
    /// reachable only through a harness-internal call (never by the model).
    fn exposure(&self) -> ToolExposure {
        ToolExposure::Direct
    }

    /// Wall-clock budget for one call.
    ///
    /// `None` means "no dispatcher-imposed deadline": the tool either owns its
    /// own bound (the command tool) or is expected to run for a long time (a
    /// subagent). A tool that can hang without one must declare a budget here;
    /// the dispatcher enforces it with a cancellation-aware timeout.
    fn timeout(&self) -> Option<Duration> {
        None
    }

    /// Whether executing this tool may block awaiting a live human decision.
    fn requires_user(&self) -> bool {
        false
    }

    /// Whether this tool only functions on a model that can perceive images.
    fn requires_vision(&self) -> bool {
        false
    }

    /// Whether invoking this tool spawns a nested sub-agent.
    fn spawns_subagent(&self) -> bool {
        false
    }

    /// Whether this tool exercises control over the harness itself.
    fn affects_control_flow(&self) -> bool {
        false
    }

    /// Whether this tool cooperates with turn cancellation via [`Tool::request_cancel`].
    fn supports_cooperative_cancel(&self) -> bool {
        false
    }

    /// Whether the tool takes a filesystem path that must be confined to the
    /// session's admitted workspace roots.
    ///
    /// This is a declared capability, not a name list: the dispatcher refuses a
    /// path-bound call whose model-supplied path escapes the workspace, and
    /// the architecture tests assert that every tool declaring a file access
    /// also declares this.
    fn path_bound(&self) -> bool {
        false
    }

    /// Best-effort cooperative cancellation of an in-flight call.
    fn request_cancel(&self, _call_id: &str) -> bool {
        false
    }

    /// The operation target this call acts on (for the scope gate).
    fn scope_target(&self, _arguments: &Value) -> ScopeTarget {
        ScopeTarget::Unspecified
    }

    /// What this call touches (for the concurrency scheduler). Defaults to a
    /// conservative declaration derived from [`Tool::scope_target`].
    fn accesses(&self, arguments: &Value) -> ToolAccesses {
        match self.scope_target(arguments) {
            ScopeTarget::Unspecified => ToolAccesses::none(),
            ScopeTarget::Path(path) => {
                ToolAccesses::read_write_file(path.to_string_lossy().into_owned())
            }
            ScopeTarget::Command(_) => ToolAccesses::all(),
        }
    }

    /// Reversibility classification of this tool's *ceiling* — the most
    /// irreversible effect any call to it can produce (ADR-0125).
    ///
    /// This is deliberately a separate axis from
    /// [`risk_profile`](Self::risk_profile): risk answers "must a human
    /// authorize this?", effect answers "can this be undone?". The default
    /// derivation reads only the declared [`RiskProfile`], which is the
    /// conservative floor; the journal reconciles it with the call's declared
    /// file accesses before promising a reversal.
    fn effect_class(&self) -> EffectClass {
        match self.risk_profile() {
            RiskProfile::ReadOnly => EffectClass::None,
            RiskProfile::IdempotentMutation => EffectClass::FileBytes,
            RiskProfile::Destructive => EffectClass::Opaque,
            RiskProfile::NetworkAccess => EffectClass::ExternalEffect,
            RiskProfile::ArbitraryExecution => EffectClass::Opaque,
        }
    }

    /// Threat / hazard classification of this tool.
    fn hazard_level(&self) -> HazardLevel {
        HazardLevel::from(self.risk_profile())
    }

    /// Short, human-friendly label for the permission prompt.
    fn permission_label(&self) -> String {
        self.name().to_string()
    }

    /// User-facing description for the permission prompt body.
    fn permission_description(&self) -> String {
        self.description().to_string()
    }

    /// Build the tool-specific permission submission for a given set of arguments.
    fn permission_submission(&self, arguments: &Value) -> Option<ToolPermissionSubmission> {
        if !self.hazard_level().requires_permission() {
            return None;
        }
        Some(ToolPermissionSubmission {
            hazard_level: self.hazard_level(),
            label: self.permission_label(),
            description: self.permission_description(),
            scope: match self.scope_target(arguments) {
                ScopeTarget::Command(c) => c,
                ScopeTarget::Path(p) => {
                    crate::normalize_access_path(p.to_string_lossy().into_owned())
                }
                ScopeTarget::Unspecified => self.name().to_string(),
            },
            payload: ToolPermissionPayload::Generic {
                summary: format!("Execute tool '{}'", self.name()),
                details: arguments.clone(),
            },
        })
    }

    /// A **pure** preview of what this call would do, when the tool can compute one
    /// without performing it (ADR-0125).
    ///
    /// "Pure" is the contract: this method may read the workspace, but it must not
    /// mutate it, spend tokens, spawn a process, or emit events. It exists so a
    /// human can be shown the concrete effect of a call *before* authorizing it —
    /// a diff instead of a path — and so an unattended round can be reviewed after
    /// the fact without a second execution.
    ///
    /// The default is `None` and that is honest, not lazy: a shell command, a
    /// network fetch, or a subagent spawn has no effect that can be computed
    /// without being caused. A tool returning `None` is telling the truth; a tool
    /// that *tried* to predict an unpredictable effect would be manufacturing
    /// confidence in exactly the place it must not.
    ///
    /// Returning `None` for arguments that are also invalid is required: a preview
    /// is only meaningful if the call would succeed, and producing a diff for a call
    /// that will then be rejected would make the preview the lie.
    ///
    /// The method is async so a tool with file I/O can offload it. The dispatcher
    /// and the permission gate await it at their respective choke points, before
    /// the call is authorized or declared ready.
    async fn preview(&self, _arguments: &Value) -> Option<PreviewResult> {
        None
    }

    /// Executes the tool with validated, normalized JSON arguments.
    ///
    /// This is the **only** execution entry point. Implementations receive a
    /// JSON object that already satisfies their declared top-level schema
    /// (types, required fields, enums, nested objects) and may assume the
    /// dispatcher has applied bounded, schema-directed coercion.
    async fn execute(&self, ctx: &ToolContext, arguments: Value) -> Result<ToolOutput>;

    /// Structured, event-emitting execution — the method the harness invokes so
    /// typed output reaches the transcript and long-running tools can stream.
    ///
    /// The default delegates to [`Tool::execute`] and emits no events. Tools
    /// that stream (e.g. `execute_command`) or spawn subagents override this.
    async fn execute_streaming<'a>(
        &self,
        ctx: &ToolContext,
        invocation: ToolInvocation<'a>,
        _on_event: Box<dyn FnMut(crate::events::SubagentEvent) + Send + 'a>,
        _on_stream: &mut (dyn FnMut(ToolStream) + Send + 'a),
    ) -> Result<ToolOutput> {
        self.execute(ctx, invocation.arguments.clone()).await
    }

    /// Convenient execution helper using a default context.
    async fn execute_simple(&self, arguments: Value) -> Result<ToolOutput> {
        self.execute(&ToolContext::default(), arguments).await
    }

    /// Prepare and execute a raw JSON argument string in one call.
    ///
    /// This is the canonical one-shot helper for tests and programmatic
    /// callers: it runs the same parse/coerce/validate pipeline as the
    /// dispatcher, so a raw string can never bypass the contract.
    async fn execute_json(&self, ctx: &ToolContext, raw: &str) -> Result<ToolOutput> {
        let prepared = crate::validation::prepare_tool_arguments(&self.parameters_schema(), raw)
            .map_err(|reason| ToolError::invalid_args(self.name(), reason))?;
        self.execute(ctx, prepared.value).await
    }

    /// The text projection of [`Tool::execute_json`], for callers that only
    /// need the model-facing string. Validation still happens exactly once.
    async fn execute_text(
        &self,
        ctx: &ToolContext,
        raw: &str,
    ) -> std::result::Result<String, String> {
        self.execute_json(ctx, raw)
            .await
            .map(|output| output.to_text())
            .map_err(|error| error.to_string())
    }

    /// This tool's OpenAI-family function schema.
    ///
    /// Delegates to the crate's single envelope definition rather than
    /// re-spelling it: three verbatim copies of this shape existed in this
    /// crate, and the copies are what drift.
    ///
    /// The protocol-neutral form is `ToolSpec`; this method exists only for the
    /// OpenAI-family envelope every compatible surface consumes.
    fn to_openai_function(&self) -> Value {
        function_envelope(self.name(), self.description(), self.parameters_schema())
    }
}

/// The canonical OpenAI-family function envelope for one tool.
///
/// Single definition on purpose: this shape was previously re-spelled in three
/// places, which is exactly how copies drift — one gains a field and the
/// others keep emitting the old contract while every caller keeps compiling.
pub fn function_envelope(name: &str, description: &str, parameters: Value) -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters,
        }
    })
}

pub mod hazard;
pub use hazard::{
    HazardLevel, HazardTier, ProcessKillSpec, ToolPermissionPayload, ToolPermissionSubmission,
};

#[cfg(test)]
mod contract_tests {
    use super::*;
    use serde_json::json;

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo"
        }
        fn parameters_schema(&self) -> Value {
            json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
        }
        async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput> {
            Ok(ToolOutput::text(
                arguments["text"].as_str().unwrap_or_default(),
            ))
        }
    }

    #[tokio::test]
    async fn execute_is_the_only_entry_point() {
        let out = EchoTool
            .execute(&ToolContext::default(), json!({"text": "hi"}))
            .await
            .unwrap();
        assert_eq!(out.to_text(), "hi");
    }

    #[test]
    fn defaults_are_conservative() {
        assert_eq!(EchoTool.exposure(), ToolExposure::Direct);
        assert!(!EchoTool.path_bound());
        assert!(EchoTool.timeout().is_none());
        assert!(EchoTool.output_schema().is_none());
        assert_eq!(EchoTool.variant(), "default");
        assert!(EchoTool.aliases().is_empty());
    }

    #[test]
    fn alias_matching_is_part_of_dispatch_identity() {
        struct Aliased;
        #[async_trait]
        impl Tool for Aliased {
            fn name(&self) -> &str {
                "execute_command"
            }
            fn description(&self) -> &str {
                "shell"
            }
            fn parameters_schema(&self) -> Value {
                json!({"type": "object"})
            }
            fn aliases(&self) -> &'static [&'static str] {
                &["bash", "shell"]
            }
            async fn execute(&self, _ctx: &ToolContext, _arguments: Value) -> Result<ToolOutput> {
                Ok(ToolOutput::text("ok"))
            }
        }
        assert!(Aliased.matches_name("bash"));
        assert!(Aliased.matches_name("execute_command"));
        assert!(!Aliased.matches_name("zsh"));
    }

    #[test]
    fn envelope_is_the_openai_function_contract() {
        let envelope = function_envelope("read", "Read", json!({"type": "object"}));
        assert_eq!(
            envelope,
            json!({
                "type": "function",
                "function": {
                    "name": "read",
                    "description": "Read",
                    "parameters": {"type": "object"},
                }
            })
        );
    }
}
