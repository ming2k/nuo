//! Canonical tool specification, capability definitions, and execution runtime
//! for the Nuo agent ecosystem.
//!
//! This crate provides zero-agent-runtime contracts allowing independent development
//! and integration of tools, dynamic closures, MCP bridges, and security policies.

pub mod access;
pub mod approval;
pub mod builtin;
pub mod command;
pub mod completion;
pub mod context;
pub mod descriptor;
pub mod dynamic;
pub mod error;
pub mod events;
pub mod guard;
pub mod mcp;
pub mod mention;
pub mod message;
pub mod output;
pub mod policy;
pub mod registry;
pub mod risk;
pub mod scope;
pub mod skills_config;
pub mod stream;
pub mod tokenizer;
pub mod todos;
pub mod tool_output;
pub mod usage;
pub mod validation;

pub use access::{ToolAccess, ToolAccesses, ToolFileAccessOperation};
pub use approval::{AlwaysApprove, ApprovalDecision, ApprovalHandler, ToolCallRequest};
pub use builtin::BuiltinTool;
pub use guard::TrajectoryGuardConfig;
pub use command::{CommandTool, ShellKind};
pub use completion::{
    CommandAlias, CommandCatalog, CommandExample, CommandSpec, CommandSubcommandSpec,
    CommandSuggestion, ComposerCompletion, ComposerCompletionKind, InputCompletion,
    InputCompletionKind,
};
pub use context::{ServiceMap, ServiceMapBuilder, ToolContext, ToolInvocation, ToolStreamSink};
pub use descriptor::{ToolDescriptor, ToolDescriptorBuilder};
pub use dynamic::DynamicTool;
pub use error::{Result, ToolError};
pub use mcp::McpTool;
pub use mention::*;
pub use message::{ImagePart, InjectionKind, InjectionOrigin, Message, Role, SubagentMeta, ToolCall, ToolResult};
pub use output::ToolOutput;
pub use policy::ToolPolicy;
pub use registry::ToolRegistry;
pub use risk::RiskProfile;
pub use scope::{ScopeTarget, ToolScope};
pub use skills_config::SkillsConfig;
pub use stream::{InputContract, InputExpectation, InputHandler, InputPrompt, ShellLine, ShellStream, ShellTermination, ToolStream};
pub use tokenizer::{StreamingCounter, Tokenizer, count_tokens, truncate_to_tokens};
pub use todos::{MAX_TODOS, TodoId, TodoItem, TodoList, TodoStatus, unix_now};
pub use usage::TokenUsage;
pub use validation::validate_tool_arguments;

pub use nuo_tool_derive::ToolSchema;

use async_trait::async_trait;

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

    /// JSON schema describing the accepted input arguments (rich-trait name).
    ///
    /// A tool implements **exactly one** of `parameters` / `parameters_schema`;
    /// each defaults to the other. `parameters_schema` is the leaf-native
    /// spelling, `parameters` the former rich-trait spelling.
    fn parameters(&self) -> serde_json::Value {
        self.parameters_schema()
    }

    /// JSON schema describing the accepted input arguments (leaf-native name).
    fn parameters_schema(&self) -> serde_json::Value {
        self.parameters()
    }

    /// Static metadata for this tool, as data (ADR-0008 `[INV-TOOL-09]`).
    ///
    /// The **canonical** source of a tool's identity, schema, risk, scopes, and
    /// capability flags. During migration the default derives a descriptor from
    /// the legacy per-method accessors, so tools that have not yet been
    /// converted keep working unchanged; once a tool implements `descriptor`
    /// directly, the other accessors delegate to it.
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters_schema: self.parameters_schema(),
            variant: self.variant().to_string(),
            aliases: self.aliases().iter().map(|a| a.to_string()).collect(),
            risk: self.risk_profile(),
            scopes: self.scopes(),
            requires_user: self.requires_user(),
            requires_vision: self.requires_vision(),
            spawns_subagent: self.spawns_subagent(),
            affects_control_flow: self.affects_control_flow(),
            available: self.is_available(),
        }
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

    // ── Metadata surface (ADR-0008 §1: defaulted from `descriptor`) ──────────
    // These give the leaf trait parity with the former rich harness trait so
    // that harness tools can migrate to this one contract. Every method derives
    // from `descriptor()` (or a runtime default), so implementors override only
    // the descriptor.

    /// Compatibility aliases that can resolve to this tool during dispatch.
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

    /// Best-effort cooperative cancellation of an in-flight call.
    fn request_cancel(&self, _call_id: &str) -> bool {
        false
    }

    /// The operation target this call acts on (for the scope gate).
    fn scope_target(&self, _arguments: &str) -> ScopeTarget {
        ScopeTarget::Unspecified
    }

    /// What this call touches (for the concurrency scheduler). Defaults to a
    /// conservative declaration derived from [`Tool::scope_target`].
    fn accesses(&self, arguments: &str) -> ToolAccesses {
        match self.scope_target(arguments) {
            ScopeTarget::Unspecified => ToolAccesses::none(),
            ScopeTarget::Path(path) => {
                ToolAccesses::read_write_file(path.to_string_lossy().into_owned())
            }
            ScopeTarget::Command(_) => ToolAccesses::all(),
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
    fn permission_submission(
        &self,
        arguments: &str,
    ) -> Option<ToolPermissionSubmission> {
        if !self.hazard_level().requires_permission() {
            return None;
        }
        Some(ToolPermissionSubmission {
            hazard_level: self.hazard_level(),
            label: self.permission_label(),
            description: self.permission_description(),
            scope: match self.scope_target(arguments) {
                ScopeTarget::Command(c) => c,
                ScopeTarget::Path(p) => p.to_string_lossy().into_owned(),
                ScopeTarget::Unspecified => self.name().to_string(),
            },
            payload: ToolPermissionPayload::Generic {
                summary: format!("Execute tool '{}'", self.name()),
                details: serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null),
            },
        })
    }

    /// Whether invocation of this tool under `ctx` with `arguments` requires explicit approval.
    fn requires_approval(&self, _ctx: &ToolContext, _arguments: &serde_json::Value) -> bool {
        false
    }

    /// Executes the tool with the supplied JSON arguments within the given context.
    ///
    /// This is the **structured** execution entry point. Its default delegates
    /// to the string-based [`Tool::call`], so a tool that implements only
    /// `call` (the former rich-trait style) works unchanged. A tool that
    /// implements `execute` directly should also leave `call` at its default.
    ///
    /// Exactly one of `execute` / `call` must be overridden; a tool that
    /// overrides neither recurses (guarded by a debug assertion in the default).
    async fn execute(&self, _ctx: &ToolContext, arguments: serde_json::Value) -> Result<ToolOutput> {
        debug_assert!(
            false,
            "tool `{}` overrides neither `execute` nor `call`; one is required",
            self.name()
        );
        let args = serde_json::to_string(&arguments).unwrap_or_default();
        self.call(&args)
            .await
            .map(ToolOutput::text)
            .map_err(ToolError::custom)
    }

    /// Executes the tool from raw JSON arguments (the former rich-trait name).
    ///
    /// Defaults to [`Tool::execute`], so a tool that implements only `execute`
    /// (the leaf-native style) works unchanged. Returns a string for
    /// compatibility with the harness dispatch's legacy signature.
    async fn call(&self, arguments: &str) -> std::result::Result<String, String> {
        let value: serde_json::Value =
            serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
        self.execute(&ToolContext::default(), value)
            .await
            .map(|out| out.to_text())
            .map_err(|err| err.to_string())
    }

    /// Convenient execution helper using a default context.
    async fn execute_simple(&self, arguments: serde_json::Value) -> Result<ToolOutput> {
        self.execute(&ToolContext::default(), arguments).await
    }

    /// Structured result from raw JSON arguments (former rich-trait name).
    ///
    /// Defaults to [`Tool::call`], wrapping the text as [`ToolOutput::Text`].
    /// Tools that return richer variants override this directly.
    async fn call_structured(&self, arguments: &str) -> std::result::Result<ToolOutput, String> {
        self.call(arguments).await.map(ToolOutput::text)
    }

    /// Structured, event-emitting execution — the method the harness invokes so
    /// typed output reaches the transcript. Default delegates to
    /// [`Tool::call_structured`] and emits no events. Tools that stream (e.g.
    /// `execute_command`) or spawn subagents (e.g. `spawn_agent`) override this.
    async fn call_structured_with_events<'a>(
        &self,
        invocation: ToolInvocation<'a>,
        _on_event: Box<dyn FnMut(crate::events::SubagentEvent) + Send + 'a>,
        _on_stream: &mut (dyn FnMut(ToolStream) + Send + 'a),
    ) -> std::result::Result<ToolOutput, String> {
        self.call_structured(invocation.arguments).await
    }

    /// Generate an OpenAI-compatible function schema for this tool.
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

pub mod hazard;
pub use hazard::{HazardLevel, HazardTier, ProcessKillSpec, ToolPermissionPayload, ToolPermissionSubmission};
