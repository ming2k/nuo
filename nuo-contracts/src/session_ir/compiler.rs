//! Multi-pass request compilation pipeline for Session IR (ADR-0241).
//!
//! Compiles an in-memory [`SessionIR`] into a canonical [`ModelRequest`]
//! through four optimizing passes:
//! - **Pass 1: Active Branch Projection** (`active_leaf` walkback to root/compaction horizon)
//! - **Pass 2: Context Projection & Budgeting** (token budgeting & tool output folding)
//! - **Pass 3: Cache Boundary Analysis** (KV-cache prefix stabilization & SHA256 fingerprinting)
//! - **Pass 4: Target Lowering** (emitting a canonical [`ModelRequest`])

use super::types::{CausalNode, NodePayload, SessionIR};
use crate::capability::{ModelRequest, ToolSpec};
use crate::instructions::{InstructionBundle, InstructionSlice, InstructionTier};
use crate::message::{Message, Role};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Configuration options provided to the Session IR compiler.
#[derive(Debug, Clone, Default)]
pub struct CompilerOptions {
    /// Declarations of tools available for this generation attempt.
    pub tool_specs: Vec<ToolSpec>,
    /// Ephemeral request-local temporary context (`E_n`, ADR-0213/0217).
    pub temporary_context: Vec<Message>,
    /// Additional ephemeral instruction slice (e.g. dynamic scratchpad or task hint).
    pub ephemeral_instruction: Option<String>,
    /// Target provider label (e.g. "anthropic", "openai", "gemini").
    pub target_dialect: Option<String>,
    /// Explicit target wire protocol (ADR-0161, ADR-0297).
    pub target_protocol: Option<crate::WireProtocol>,
}

/// The result produced by the Session IR compilation pipeline.
#[derive(Debug, Clone)]
pub struct CompilationArtifact {
    /// The lowered provider-neutral [`ModelRequest`].
    pub request: ModelRequest,
    /// Forensic details of the computed KV-cache boundary.
    pub cache_boundary: CacheBoundary,
    /// Metrics and statistics about the compilation passes.
    pub stats: CompilationStats,
}

/// Identification of the cacheable static prefix vs. volatile suffix (ADR-0217).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheBoundary {
    /// Deterministic SHA256 hexadecimal hash of the static instruction & historical prefix.
    pub prefix_fingerprint: String,
    /// Number of conversation messages included in the stable cacheable prefix.
    pub stable_message_count: usize,
    /// Number of conversation messages classified as ephemeral dynamic tail.
    pub volatile_message_count: usize,
    /// Byte length of the static system and workspace instructions.
    pub static_instruction_bytes: usize,
}

/// Diagnostic telemetry captured during request compilation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilationStats {
    /// Total nodes traversed in the active lineage.
    pub nodes_traversed: usize,
    /// Dialogue messages retained in the model window.
    pub messages_retained: usize,
    /// Number of oversized tool execution results folded or truncated.
    pub tool_results_truncated: usize,
    /// Rough heuristic token estimate of the total input payload.
    pub estimated_tokens: usize,
}

/// Error encountered during request compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompilerError {
    /// The session contains no active branch to project.
    EmptySession,
    /// State active_leaf pointer references a non-existent node.
    InvalidActiveLeaf(String),
}

impl std::fmt::Display for CompilerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptySession => write!(f, "Session IR has no active nodes or instructions"),
            Self::InvalidActiveLeaf(id) => {
                write!(f, "Active leaf '{id}' not found in causal graph")
            }
        }
    }
}

impl std::error::Error for CompilerError {}

fn role_as_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    }
}

/// Main entry point: compile a [`SessionIR`] into a [`ModelRequest`].
pub fn compile_session_request(
    ir: &SessionIR,
    options: CompilerOptions,
) -> Result<CompilationArtifact, CompilerError> {
    // -------------------------------------------------------------------------
    // Pass 1: Active Branch Projection
    // -------------------------------------------------------------------------
    let active_nodes = pass1_active_branch_projection(ir)?;

    // -------------------------------------------------------------------------
    // Pass 2: Context Projection & Budget Allocation
    // -------------------------------------------------------------------------
    let (messages, truncated_tools) = pass2_context_budgeting(&active_nodes, ir)?;

    // -------------------------------------------------------------------------
    // Pass 3: Cache Boundary Analysis & Prefix Stabilization
    // -------------------------------------------------------------------------
    let (instructions, cache_boundary) = pass3_cache_boundary_analysis(ir, &messages, &options);

    // -------------------------------------------------------------------------
    // Pass 4: Lowering to ModelRequest (ADR-0241, ADR-0297)
    // -------------------------------------------------------------------------
    let messages = pass4_target_lowering(
        messages,
        options.target_protocol,
        options.target_dialect.as_deref(),
    );
    let mut request = ModelRequest::new(messages);
    request.instructions = instructions;
    request.tool_specs = options.tool_specs;
    request.temporary_context = options.temporary_context;
    request.turn_context = Arc::default();
    request.prompt_cache_preference = crate::PromptCachePreference::default();

    let estimated_tokens = estimate_total_tokens(&request);

    let stats = CompilationStats {
        nodes_traversed: active_nodes.len(),
        messages_retained: request.messages.len(),
        tool_results_truncated: truncated_tools,
        estimated_tokens,
    };

    Ok(CompilationArtifact {
        request,
        cache_boundary,
        stats,
    })
}

/// Pass 1: Walk backwards from `state.active_leaf` along `parent_id` pointers.
fn pass1_active_branch_projection(ir: &SessionIR) -> Result<Vec<&CausalNode>, CompilerError> {
    match &ir.state.active_leaf {
        Some(leaf_id) => {
            if !ir.history.nodes.contains_key(leaf_id) {
                return Err(CompilerError::InvalidActiveLeaf(leaf_id.clone()));
            }
            Ok(ir
                .history
                .linear_path_with_horizon(leaf_id, ir.state.compaction_horizon.as_deref()))
        }
        None => Ok(Vec::new()),
    }
}

/// Pass 2: Apply token budgets and fold oversized or stale tool results.
fn pass2_context_budgeting(
    nodes: &[&CausalNode],
    ir: &SessionIR,
) -> Result<(Vec<Message>, usize), CompilerError> {
    let mut messages = Vec::with_capacity(nodes.len());
    let mut truncated_count = 0;
    let max_tool_chars = ir.policy.budget.max_tool_output_tokens.saturating_mul(4);

    for node in nodes {
        match &node.payload {
            NodePayload::Message { message } => {
                let mut msg = message.clone();
                // Apply tool result budget folding if output exceeds threshold (ADR-0262, ADR-0264)
                if msg.role == Role::Tool && msg.content.len() > max_tool_chars {
                    let call_id = msg.tool_call_id.as_deref().unwrap_or(node.id.as_str());
                    let truncated_text = format!(
                        "{}...\n[Tool output truncated by Session IR compiler: {} bytes omitted]\n[Epistemic virtual memory: inspect full output with handle \"call:{call_id}\"]",
                        &msg.content[..max_tool_chars],
                        msg.content.len() - max_tool_chars
                    );
                    msg.content = truncated_text;
                    truncated_count += 1;
                }
                messages.push(msg);
            }
            NodePayload::Observation {
                call_id,
                tool_name,
                lifecycle,
                ..
            } => {
                let msg = lifecycle.lower_to_message(call_id, tool_name);
                messages.push(msg);
            }
            NodePayload::Compaction {
                summary,
                read_files,
                modified_files,
                ..
            } => {
                // Compaction nodes inject a system-role compaction summary into dialogue view (ADR-0262)
                let mut content = format!(
                    "[Conversation Summary Checkpoint — inspect with handle \"fold:{}\"]:\n{summary}",
                    node.id
                );
                if !read_files.is_empty() || !modified_files.is_empty() {
                    content.push_str("\n\n### Tracked Files:\n");
                    if !modified_files.is_empty() {
                        content.push_str(&format!("- Modified: {}\n", modified_files.join(", ")));
                    }
                    if !read_files.is_empty() {
                        content.push_str(&format!("- Consulted: {}\n", read_files.join(", ")));
                    }
                }
                let summary_msg = Message::new(Role::System, content);
                messages.push(summary_msg);
            }
            NodePayload::Termination {
                reason,
                partial_output,
                ..
            } => {
                // Interrupted turns emit a synthetic notification informing the model
                let reason_str = match reason {
                    super::types::TerminationReason::UserInterrupt => "Interrupted by user",
                    super::types::TerminationReason::Timeout => "Execution timed out",
                    super::types::TerminationReason::FatalError { error } => error.as_str(),
                    super::types::TerminationReason::Superseded => "Superseded by user message",
                };
                let content = match partial_output {
                    Some(out) => {
                        format!("[Execution stopped: {reason_str}]\nPartial output:\n{out}")
                    }
                    None => format!("[Execution stopped: {reason_str}]"),
                };
                messages.push(Message::new(Role::System, content));
            }
            NodePayload::SystemNotice {
                source,
                notice_type,
                content,
            } => {
                let notice_msg = Message::new(
                    Role::System,
                    format!("[System Notice from {source} ({notice_type})]: {content}"),
                );
                messages.push(notice_msg);
            }
        }
    }

    // ADR-0254: Prune superseded build/test results and evict stale companion image payloads
    if let Some(outcome) = crate::pressure::prune_tool_results(&mut messages, 4_000, 100) {
        truncated_count += outcome.cleared_count;
    }

    Ok((messages, truncated_count))
}

/// Pass 3: Construct tiered instruction bundle and compute static KV-cache fingerprint.
fn pass3_cache_boundary_analysis(
    ir: &SessionIR,
    messages: &[Message],
    options: &CompilerOptions,
) -> (InstructionBundle, CacheBoundary) {
    let mut slices = Vec::new();
    let mut static_bytes = 0;

    // 1. Base Tier: System Persona
    if let Some(persona) = &ir.policy.rules.system_persona
        && !persona.trim().is_empty()
    {
        static_bytes += persona.len();
        slices.push(InstructionSlice::new(
            "base_persona",
            InstructionTier::Base,
            persona.clone(),
        ));
    }

    // 2. Session Tier: Workspace rules (AGENTS.md, conventions)
    for (i, rule) in ir.policy.rules.project_rules.iter().enumerate() {
        if !rule.trim().is_empty() {
            static_bytes += rule.len();
            slices.push(InstructionSlice::new(
                format!("project_rule_{i}"),
                InstructionTier::Session,
                rule.clone(),
            ));
        }
    }

    // 3. Ephemeral Tier: Dynamic scratchpad or task-scoped hints
    if let Some(ephemeral) = &options.ephemeral_instruction
        && !ephemeral.trim().is_empty()
    {
        slices.push(InstructionSlice::new(
            "ephemeral_hint",
            InstructionTier::Ephemeral,
            ephemeral.clone(),
        ));
    }

    let bundle = InstructionBundle::new(slices);

    // Compute cache boundary: all messages except the very last turn are considered stable prefix
    let total_messages = messages.len();
    let (stable_count, volatile_count) = if total_messages > 1 {
        (total_messages - 1, 1)
    } else {
        (0, total_messages)
    };

    // Calculate deterministic SHA256 prefix fingerprint
    let mut hasher = Sha256::new();
    for slice in bundle.slices_by_tier(InstructionTier::Base) {
        hasher.update(slice.content.as_bytes());
    }
    for slice in bundle.slices_by_tier(InstructionTier::Session) {
        hasher.update(slice.content.as_bytes());
    }
    for msg in &messages[..stable_count] {
        hasher.update(role_as_str(msg.role).as_bytes());
        hasher.update(msg.content.as_bytes());
    }
    let fingerprint = format!("{:x}", hasher.finalize());

    let boundary = CacheBoundary {
        prefix_fingerprint: fingerprint,
        stable_message_count: stable_count,
        volatile_message_count: volatile_count,
        static_instruction_bytes: static_bytes,
    };

    (bundle, boundary)
}

/// Heuristic token estimator (~4 chars per token).
fn estimate_total_tokens(req: &ModelRequest) -> usize {
    let mut chars = 0;
    for slice in &req.instructions.slices {
        chars += slice.content.len();
    }
    for msg in &req.messages {
        chars += msg.content.len();
    }
    for tmp in &req.temporary_context {
        chars += tmp.content.len();
    }
    chars / 4
}

/// Pass 4: Lower the budgeted conversation messages into target wire format invariants (ADR-0241, ADR-0297).
fn pass4_target_lowering(
    messages: Vec<Message>,
    target_protocol: Option<crate::WireProtocol>,
    target_dialect: Option<&str>,
) -> Vec<Message> {
    let is_google = target_protocol == Some(crate::WireProtocol::GoogleGemini)
        || target_dialect.is_some_and(|dialect| {
            dialect.eq_ignore_ascii_case("google")
                || dialect.eq_ignore_ascii_case("google-gemini")
                || dialect.eq_ignore_ascii_case("gemini")
                || dialect.contains("gemini")
                || dialect.contains("google")
        });

    if is_google {
        lower_messages_for_google(messages)
    } else {
        messages
    }
}

/// Transform messages for Google Gemini, ensuring that any unsigned / cross-provider tool calls
/// are safely degraded to objective dialogue facts rather than emitting bare functionCalls (ADR-0297).
fn lower_messages_for_google(messages: Vec<Message>) -> Vec<Message> {
    let mut lowered = Vec::with_capacity(messages.len());
    let mut degraded_call_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for mut message in messages {
        match message.role {
            Role::Assistant => {
                if let Some(calls) = message.tool_calls.take() {
                    let mut retained_signed_calls = Vec::new();
                    let mut degraded_call_prose = Vec::new();

                    for call in calls {
                        if message.has_gemini_thought_signature_for(&call) {
                            retained_signed_calls.push(call);
                        } else {
                            // ADR-0297: Unsigned / cross-provider tool call.
                            // Google Gemini strictly requires thought_signature on every functionCall part.
                            // Convert this unsigned call into an objective dialogue fact.
                            let args_str = if call.arguments.trim().is_empty() {
                                "{}"
                            } else {
                                call.arguments.trim()
                            };
                            degraded_call_names.insert(call.id.clone(), call.name.clone());
                            degraded_call_prose.push(format!(
                                "[Executed tool \"{}\" with arguments: {}]",
                                call.name, args_str
                            ));
                        }
                    }

                    if !degraded_call_prose.is_empty() {
                        let prose = degraded_call_prose.join("\n");
                        if message.content.trim().is_empty() {
                            message.content = prose;
                        } else {
                            message.content = format!("{}\n\n{}", message.content.trim(), prose);
                        }
                    }

                    if !retained_signed_calls.is_empty() {
                        message.tool_calls = Some(retained_signed_calls);
                    }
                }
                lowered.push(message);
            }
            Role::Tool => {
                let call_id = message.tool_call_id.as_deref().unwrap_or_default();
                if let Some(name) = degraded_call_names.get(call_id) {
                    // ADR-0297: The corresponding assistant tool call was degraded to dialogue facts.
                    // Lower this tool result into an objective user-role dialogue fact.
                    message.role = Role::User;
                    message.tool_call_id = None;
                    message.content = format!("[Tool result for \"{name}\"]:\n{}", message.content);
                }
                lowered.push(message);
            }
            _ => {
                lowered.push(message);
            }
        }
    }

    lowered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_ir::types::{BudgetPolicy, RuleSet, SessionPolicy, TerminationReason};

    #[test]
    fn test_compiler_pipeline_full_flow() {
        let policy = SessionPolicy {
            rules: RuleSet {
                system_persona: Some("You are Muta AI".to_string()),
                workspace_root: Some("/workspace".to_string()),
                project_rules: vec!["Rule 1: Be fast".to_string()],
            },
            budget: BudgetPolicy {
                max_context_tokens: 100_000,
                compaction_trigger_tokens: 80_000,
                max_tool_output_tokens: 10, // 40 chars limit for test
            },
            ..Default::default()
        };

        let mut ir = SessionIR::new("session-compile-test", policy, 1000);

        // Turn 1: User
        ir.append_message("u1", 1_000_000, Message::new(Role::User, "Hello"));
        // Turn 2: Assistant with normal output
        ir.append_message(
            "a1",
            1_001_000,
            Message::new(Role::Assistant, "I will run a tool"),
        );
        // Turn 3: Tool with very long output exceeding budget
        let huge_tool_output = "a".repeat(200);
        ir.append_message("t1", 1_002_000, Message::new(Role::Tool, huge_tool_output));
        // Turn 4: Assistant interrupted
        ir.record_termination(
            "term1",
            1_003_000,
            TerminationReason::UserInterrupt,
            Some("Working on it".to_string()),
            None,
            Some(1000),
        );

        let options = CompilerOptions {
            ephemeral_instruction: Some("Focus on speed".to_string()),
            ..Default::default()
        };

        let artifact = compile_session_request(&ir, options).expect("compilation should succeed");

        // Assert Pass 1 & 2
        assert_eq!(artifact.stats.nodes_traversed, 4);
        assert_eq!(artifact.stats.tool_results_truncated, 1);
        assert_eq!(artifact.request.messages.len(), 4);

        // Tool output must be truncated and contain canonical inspect handle (ADR-0264)
        let tool_msg = &artifact.request.messages[2];
        assert!(
            tool_msg
                .content
                .contains("Tool output truncated by Session IR compiler")
        );
        assert!(
            tool_msg.content.contains(
                "[Epistemic virtual memory: inspect full output with handle \"call:t1\"]"
            )
        );

        // Interrupted turn must yield synthetic notification
        let term_msg = &artifact.request.messages[3];
        assert!(
            term_msg
                .content
                .contains("Execution stopped: Interrupted by user")
        );

        // Assert Pass 3 Cache Boundary
        assert_eq!(artifact.cache_boundary.stable_message_count, 3);
        assert_eq!(artifact.cache_boundary.volatile_message_count, 1);
        assert!(!artifact.cache_boundary.prefix_fingerprint.is_empty());

        // Assert Pass 4 ModelRequest Lowering
        assert_eq!(artifact.request.instructions.len(), 3); // Base persona, Project rule, Ephemeral
    }

    #[test]
    fn test_prefix_fingerprint_stability() {
        let mut policy = SessionPolicy::default();
        policy.rules.system_persona = Some("Fixed Persona".to_string());
        policy.rules.project_rules = vec!["Rule A".to_string()];

        let mut ir = SessionIR::new("session-fp-test", policy, 1000);
        ir.append_message("u1", 1_000_000, Message::new(Role::User, "Msg 1"));
        ir.append_message("a1", 1_001_000, Message::new(Role::Assistant, "Resp 1"));

        let art1 = compile_session_request(&ir, CompilerOptions::default()).unwrap();
        let art2 = compile_session_request(&ir, CompilerOptions::default()).unwrap();

        // Fingerprint must be strictly deterministic across identical compilations
        assert_eq!(
            art1.cache_boundary.prefix_fingerprint,
            art2.cache_boundary.prefix_fingerprint
        );
    }

    #[test]
    fn test_compiler_pass4_lowers_unsigned_tool_calls_for_google_dialect() {
        use crate::message::ToolCall;

        let mut ir = SessionIR::new("session-google-test", SessionPolicy::default(), 1000);
        ir.append_message("u1", 1_000_000, Message::new(Role::User, "run command"));

        // Assistant turn from foreign provider (no thought signatures)
        let call = ToolCall::new("call_run_1", "default_api:run_command", r#"{"command":"ls"}"#);
        let mut foreign_assistant = Message::new(Role::Assistant, "Running command");
        foreign_assistant.tool_calls = Some(vec![call.clone()]);
        ir.append_message("a1", 1_001_000, foreign_assistant);

        // Tool result turn
        let tool_msg = Message::tool_result(&call, "file1.txt\nfile2.txt");
        ir.append_message("t1", 1_002_000, tool_msg);

        // Turn from Gemini with valid thought signature
        let gemini_call = ToolCall::new("call_gemini_1", "list_dir", r#"{"path":"."}"#);
        let mut gemini_assistant = Message::new(Role::Assistant, "Listing files");
        gemini_assistant.tool_calls = Some(vec![gemini_call.clone()]);
        let mut meta = serde_json::Map::new();
        meta.insert(
            "gemini_thought_signatures".to_string(),
            serde_json::json!({ "call_gemini_1": "sig-valid" }),
        );
        gemini_assistant.provider_meta = Some(meta);
        ir.append_message("a2", 1_003_000, gemini_assistant);

        let gemini_tool = Message::tool_result(&gemini_call, "file1.txt");
        ir.append_message("t2", 1_004_000, gemini_tool);

        // Compile targeting WireProtocol::GoogleGemini directly
        let options = CompilerOptions {
            target_protocol: Some(crate::WireProtocol::GoogleGemini),
            ..Default::default()
        };
        let artifact = compile_session_request(&ir, options).expect("compilation should succeed");
        let messages = &artifact.request.messages;

        // Foreign assistant message: tool_calls stripped and converted to dialogue fact
        assert_eq!(messages[1].role, Role::Assistant);
        assert!(messages[1].tool_calls.is_none());
        assert!(messages[1].content.contains("Running command"));
        assert!(messages[1].content.contains(r#"[Executed tool "default_api:run_command" with arguments: {"command":"ls"}]"#));

        // Foreign tool message: lowered to User role dialogue fact
        assert_eq!(messages[2].role, Role::User);
        assert!(messages[2].tool_call_id.is_none());
        assert!(messages[2].content.contains(r#"[Tool result for "default_api:run_command"]:"#));

        // Gemini assistant message: retains native tool_calls because it has valid thought signature
        assert_eq!(messages[3].role, Role::Assistant);
        assert!(messages[3].tool_calls.is_some());
        assert_eq!(messages[3].tool_calls.as_ref().unwrap()[0].id, "call_gemini_1");

        // Gemini tool message: retains native Tool role and tool_call_id
        assert_eq!(messages[4].role, Role::Tool);
        assert_eq!(messages[4].tool_call_id.as_deref(), Some("call_gemini_1"));
    }
}
