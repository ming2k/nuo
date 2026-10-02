//! Unified context planner and request compiler (ADR-0277).
//!
//! Planning owns representation selection. The immutable plan contains the
//! actual selected bytes; compilation cannot substitute the original input or
//! perform a second pruning pass. Production cutover is tracked separately
//! from the correctness of these domain operations.

pub mod checkpoint;

use nuo_contracts::context_lifecycle::{
    AdmissionError, Budget, BudgetComponent, FactId, Representation, RequestBlock, RequestId,
    RequestManifest, Validity,
};
use nuo_contracts::{Message, tokenizer};
use sha2::{Digest, Sha256};

/// The fixed block order of a compiled request (ADR-0277 §2 protection order).
///
/// The order is part of the contract: a stable order keeps provider prefixes
/// stable and makes the request reproducible (`INV-ADMIT-03`).
pub const BLOCK_ORDER: &[&str] = &[
    "system_instructions",
    "tool_schemas",
    "task_state",
    "checkpoint_summary",
    "tail",
    "observations",
    "images",
    "temporary_context",
    "current_user_input",
];

/// The request class. A summary or ephemeral call is **not** a bypass: it takes
/// the same admission path with an explicit scope (`INV-ADMIT-01`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestScope {
    /// An ordinary conversational request.
    Conversational,
    /// A checkpoint-summarization call (ADR-0278).
    Summary,
    /// A test/title/other ephemeral call whose usage is not conversational.
    Ephemeral,
}

impl RequestScope {
    /// Whether usage from this scope counts toward conversational accounting.
    pub const fn counts_as_conversational(self) -> bool {
        matches!(self, RequestScope::Conversational)
    }

    /// The stable scope label used in block roles.
    pub const fn label(self) -> &'static str {
        match self {
            RequestScope::Conversational => "conversational",
            RequestScope::Summary => "summary",
            RequestScope::Ephemeral => "ephemeral",
        }
    }
}

/// A planned representation of one fact in the request (ADR-0275 §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedItem {
    /// The fact this item represents.
    pub fact_id: FactId,
    /// The block role the item contributes to.
    pub block: &'static str,
    /// How the item is represented.
    pub representation: Representation,
    /// Validity relative to the branch.
    pub validity: Validity,
    /// Metered tokens after applying the representation.
    pub tokens: u64,
}

/// A component budget allocation within the input ceiling (ADR-0277 §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentAllocation {
    /// Stable block role.
    pub block: &'static str,
    /// Tokens allocated to this block.
    pub tokens: u64,
    /// Whether the block is mandatory (never degraded to fit).
    pub mandatory: bool,
}

/// The typed, deterministic result of planning one request (ADR-0277 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPlan {
    /// The resolved budget (`W`, `O`, `F`, `I`).
    budget: Budget,
    /// Selected bytes, inaccessible to callers after admission.
    blocks: Vec<CompiledBlock>,
    /// Per-block allocations, in [`BLOCK_ORDER`].
    pub allocations: Vec<ComponentAllocation>,
    /// The planned representations, in deterministic order.
    pub items: Vec<PlannedItem>,
    /// The request class.
    pub scope: RequestScope,
    /// Iterations the planner ran (bounded by
    /// [`nuo_contracts::context_lifecycle::PLANNING_MAX_ITERATIONS`]).
    pub iterations: u32,
}

impl ContextPlan {
    /// Total tokens the plan will send.
    pub fn total_tokens(&self) -> u64 {
        self.allocations.iter().map(|a| a.tokens).sum()
    }

    /// The mandatory component report for a refusal.
    pub fn mandatory_components(&self) -> Vec<BudgetComponent> {
        self.allocations
            .iter()
            .filter(|a| a.mandatory)
            .map(|a| BudgetComponent {
                name: a.block.to_string(),
                tokens: a.tokens,
            })
            .collect()
    }
}

/// One assembled input, the planner's raw material. The caller assembles this
/// from committed facts and the active view; the planner itself reads no
/// storage.
#[derive(Debug, Clone)]
pub struct AssembledContext {
    /// System/project instructions (mandatory).
    pub system_instructions: String,
    /// Tool schemas rendered to text (mandatory).
    pub tool_schemas: String,
    /// Active task state (mandatory when non-empty).
    pub task_state: String,
    /// A checkpoint summary, when the view carries one.
    pub checkpoint_summary: Option<String>,
    /// The tail messages after the view's `tail_after_fact_id`.
    pub tail: Vec<Message>,
    /// Observations, already reduced to previews with their fact ids.
    pub observations: Vec<ObservationPreview>,
    /// Temporary context injections (mandatory protocol blocks).
    pub temporary_context: Vec<String>,
    /// The current user input (mandatory).
    pub current_user_input: String,
}

/// A tool observation reduced to a preview for the request (ADR-0276 §1).
#[derive(Debug, Clone)]
pub struct ObservationPreview {
    /// The fact the observation belongs to.
    pub fact_id: FactId,
    /// The deterministic preview text.
    pub preview: String,
    /// Validity relative to the branch.
    pub validity: Validity,
}

/// Why planning failed (ADR-0277 §2, `INV-ADMIT-04`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// A structured protocol payload could not be encoded.
    Encoding(String),
    /// The budget could not be resolved (window too small).
    Budget(AdmissionError),
    /// Mandatory blocks exceed the input ceiling.
    Admission(AdmissionError),
    /// The planner exhausted its bounded iterations without progress.
    NoProgress {
        /// Iterations attempted.
        iterations: u32,
    },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::Encoding(e) => write!(f, "context encoding failed: {e}"),
            PlanError::Budget(e) | PlanError::Admission(e) => write!(f, "{e}"),
            PlanError::NoProgress { iterations } => write!(
                f,
                "context planning made no progress after {iterations} iterations"
            ),
        }
    }
}

impl std::error::Error for PlanError {}

/// The unified planner (ADR-0277). Pure: it reads no storage and calls no
/// provider.
#[derive(Debug, Clone)]
pub struct ContextPlanner {
    /// The resolved input budget.
    budget: Budget,
    /// Per-item observation preview ceiling.
    observation_preview_tokens: u64,
}

impl ContextPlanner {
    /// Build a planner from a resolved budget.
    pub fn new(budget: Budget) -> Self {
        Self {
            budget,
            observation_preview_tokens: budget.observation_preview_tokens(),
        }
    }

    /// The resolved budget.
    pub fn budget(&self) -> Budget {
        self.budget
    }

    /// Plan one request.
    ///
    /// Mandatory blocks (authority/protocol, the current user input, and any
    /// active task constraints) are reserved first; if they alone exceed `I`,
    /// the plan is refused with a per-component report rather than deleting
    /// constraints. Degradable blocks are then filled within the remaining
    /// ceiling in the fixed [`BLOCK_ORDER`].
    pub fn plan(
        &self,
        ctx: &AssembledContext,
        scope: RequestScope,
    ) -> Result<ContextPlan, PlanError> {
        // Keep structured messages byte-exact. Without an explicit closed-group
        // projection, the entire tail is mandatory: role-based slicing would
        // separate calls/results or erase images and reasoning signatures.
        let tail =
            serde_json::to_string(&ctx.tail).map_err(|e| PlanError::Encoding(e.to_string()))?;
        let mut contents = vec![
            ("system_instructions", ctx.system_instructions.clone(), true),
            ("tool_schemas", ctx.tool_schemas.clone(), true),
            ("task_state", ctx.task_state.clone(), true),
            (
                "checkpoint_summary",
                ctx.checkpoint_summary.clone().unwrap_or_default(),
                true,
            ),
            ("tail", tail, true),
            ("observations", String::new(), false),
            ("images", String::new(), true),
            ("temporary_context", ctx.temporary_context.join("\n"), true),
            ("current_user_input", ctx.current_user_input.clone(), true),
        ];
        let mandatory: Vec<_> = contents
            .iter()
            .filter(|(_, _, mandatory)| *mandatory)
            .map(|(name, text, _)| BudgetComponent {
                name: (*name).into(),
                tokens: tokens_of(text),
            })
            .collect();
        self.budget
            .admit(&mandatory)
            .map_err(PlanError::Admission)?;
        let mandatory_total = mandatory.iter().map(|c| c.tokens).sum::<u64>();
        let remaining = self.budget.input_ceiling - mandatory_total;
        let mut observations = String::new();
        let mut items = Vec::new();
        for obs in &ctx.observations {
            let preview = tokenizer::truncate_str_to_tokens(
                &obs.preview,
                self.observation_preview_tokens.min(usize::MAX as u64) as usize,
            );
            let rendered = format!(
                "[observation {}; {:?}]\n{}\n",
                obs.fact_id.as_str(),
                obs.validity,
                preview
            );
            let candidate = format!("{observations}{rendered}");
            if tokens_of(&candidate) > remaining {
                break;
            }
            items.push(PlannedItem {
                fact_id: obs.fact_id.clone(),
                block: "observations",
                representation: if preview.len() == obs.preview.len() {
                    Representation::Full
                } else {
                    Representation::Excerpt
                },
                validity: obs.validity,
                tokens: tokens_of(preview),
            });
            observations = candidate;
        }
        contents[5].1 = observations;
        let mut allocations = Vec::with_capacity(contents.len());
        let mut blocks = Vec::with_capacity(contents.len());
        for (role, content, mandatory) in contents {
            let tokens = tokens_of(&content);
            allocations.push(ComponentAllocation {
                block: role,
                tokens,
                mandatory,
            });
            blocks.push(CompiledBlock {
                role: role.into(),
                hash: format!("{:x}", Sha256::digest(content.as_bytes())),
                content,
                tokens,
            });
        }
        Ok(ContextPlan {
            budget: self.budget,
            blocks,
            allocations,
            items,
            scope,
            iterations: 1,
        })
    }
}

fn tokens_of(text: &str) -> u64 {
    tokenizer::count_tokens(text) as u64
}

/// An immutable compiled request (ADR-0277 §5). The provider port accepts only
/// this, never a caller-constructed history array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSnapshot {
    /// The immutable manifest.
    pub manifest: RequestManifest,
    /// The ordered, hashed blocks.
    pub blocks: Vec<CompiledBlock>,
}

/// One compiled block with its serialized bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledBlock {
    /// The block role.
    pub role: String,
    /// The serialized block content.
    pub content: String,
    /// SHA-256 of `content`.
    pub hash: String,
    /// Metered tokens.
    pub tokens: u64,
}

/// The pure request compiler (ADR-0277). No I/O, no history mutation, no second
/// trimming pass.
#[derive(Debug, Clone, Default)]
pub struct RequestCompiler;

impl RequestCompiler {
    /// A new compiler.
    pub fn new() -> Self {
        Self
    }

    /// Compile a planned context into an immutable snapshot.
    ///
    /// Blocks are emitted in the canonical [`BLOCK_ORDER`] and hashed over
    /// their serialized bytes, so identical inputs yield byte-identical output
    /// (`INV-ADMIT-03`).
    pub fn compile(&self, plan: &ContextPlan, request_id: &RequestId) -> RequestSnapshot {
        let blocks = plan.blocks.clone();

        let manifest = RequestManifest {
            request_id: request_id.clone(),
            branch_id: nuo_contracts::context_lifecycle::BranchId::from(""),
            basis_revision: 0,
            policy_revision: 0,
            blocks: blocks
                .iter()
                .map(|b| RequestBlock {
                    role: b.role.clone(),
                    hash: b.hash.clone(),
                    tokens: b.tokens,
                })
                .collect(),
            input_ceiling: plan.budget.input_ceiling,
        };

        RequestSnapshot { manifest, blocks }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_contracts::Role;
    use nuo_contracts::context_lifecycle::{FramingReserve, ModelWindow, OutputReserve};

    fn budget(window: u64, visible: u64, reasoning: u64, framing: u64) -> Budget {
        Budget::resolve(
            ModelWindow::tokens(window),
            OutputReserve::Additive {
                visible_tokens: visible,
                reasoning_tokens: reasoning,
            },
            FramingReserve::tokens(framing),
        )
        .unwrap()
    }

    fn assembled() -> AssembledContext {
        AssembledContext {
            system_instructions: "You are a careful agent.".into(),
            tool_schemas: "read: read a file".into(),
            task_state: "objective: fix the failing test".into(),
            checkpoint_summary: None,
            tail: vec![
                Message::new(Role::User, "hello"),
                Message::new(Role::Assistant, "hi there"),
            ],
            observations: vec![],
            temporary_context: vec![],
            current_user_input: "please continue".into(),
        }
    }

    #[test]
    fn compilation_emits_only_admitted_bytes_and_counts_actual_content() {
        let planner = ContextPlanner::new(budget(8_000, 1_000, 0, 1_000));
        let mut ctx = assembled();
        ctx.observations = (0..30)
            .map(|i| ObservationPreview {
                fact_id: format!("f{i}").into(),
                preview: "日本語😀secret-detail ".repeat(1_000),
                validity: Validity::Current,
            })
            .collect();
        let plan = planner.plan(&ctx, RequestScope::Conversational).unwrap();
        // Mutating source material after admission cannot alter the snapshot.
        ctx.system_instructions = "unadmitted replacement".into();
        let compiled = RequestCompiler::new().compile(&plan, &RequestId::from("r"));
        assert!(
            compiled
                .blocks
                .iter()
                .all(|b| b.tokens == tokens_of(&b.content))
        );
        assert!(compiled.blocks.iter().map(|b| b.tokens).sum::<u64>() <= plan.budget.input_ceiling);
        assert!(
            !compiled
                .blocks
                .iter()
                .any(|b| b.content.contains("unadmitted replacement"))
        );
        assert!(
            compiled
                .blocks
                .iter()
                .find(|b| b.role == "observations")
                .unwrap()
                .content
                .len()
                < ctx
                    .observations
                    .iter()
                    .map(|o| o.preview.len())
                    .sum::<usize>()
        );
    }

    #[test]
    fn oversized_structured_tail_is_refused_without_partial_tool_groups() {
        let planner = ContextPlanner::new(budget(8_000, 1_000, 0, 1_000));
        let mut ctx = assembled();
        ctx.tail
            .push(Message::new(Role::Tool, "result ".repeat(20_000)));
        assert!(matches!(
            planner.plan(&ctx, RequestScope::Conversational),
            Err(PlanError::Admission(_))
        ));
    }

    #[test]
    fn mandatory_blocks_are_reserved_first_and_metered() {
        let planner = ContextPlanner::new(budget(200_000, 4_000, 0, 4_000));
        let plan = planner
            .plan(&assembled(), RequestScope::Conversational)
            .unwrap();
        let system = plan
            .allocations
            .iter()
            .find(|a| a.block == "system_instructions")
            .unwrap();
        assert!(system.mandatory);
        let user = plan
            .allocations
            .iter()
            .find(|a| a.block == "current_user_input")
            .unwrap();
        assert!(user.mandatory);
        assert!(plan.total_tokens() <= plan.budget.input_ceiling);
    }

    #[test]
    fn oversized_mandatory_blocks_refuse_with_a_component_report() {
        // A tiny window with a large mandatory system prompt.
        let planner = ContextPlanner::new(budget(8_000, 4_000, 0, 1_024));
        let mut ctx = assembled();
        ctx.system_instructions = "x ".repeat(20_000);
        match planner
            .plan(&ctx, RequestScope::Conversational)
            .unwrap_err()
        {
            PlanError::Admission(AdmissionError::ContextAdmissionExceeded {
                components,
                mandatory_tokens,
                input_ceiling,
            }) => {
                assert!(mandatory_tokens > input_ceiling);
                assert!(components.iter().any(|c| c.name == "system_instructions"));
                assert!(components.iter().any(|c| c.name == "current_user_input"));
            }
            other => panic!("expected admission refusal, got {other:?}"),
        }
    }

    #[test]
    fn compilation_is_deterministic_and_ordered() {
        let planner = ContextPlanner::new(budget(200_000, 4_000, 0, 4_000));
        let ctx = assembled();
        let plan = planner.plan(&ctx, RequestScope::Conversational).unwrap();
        let compiler = RequestCompiler::new();
        let id = RequestId::from("req-1");
        let a = compiler.compile(&plan, &id);
        let b = compiler.compile(&plan, &id);
        assert_eq!(
            a, b,
            "same inputs must produce identical blocks (INV-ADMIT-03)"
        );
        // Blocks follow the canonical order.
        let roles: Vec<&str> = a.blocks.iter().map(|b| b.role.as_str()).collect();
        let expected: Vec<&str> = BLOCK_ORDER.to_vec();
        assert_eq!(roles, expected);
        // Every block carries a stable hash of its content.
        for block in &a.blocks {
            let mut h = Sha256::new();
            h.update(block.content.as_bytes());
            assert_eq!(block.hash, format!("{:x}", h.finalize()));
        }
    }

    #[test]
    fn a_reasoning_heavy_call_shrinks_the_plan_ceiling() {
        let base = ContextPlanner::new(budget(100_000, 8_000, 0, 3_000));
        let heavy = ContextPlanner::new(budget(100_000, 8_000, 40_000, 3_000));
        assert!(heavy.budget().input_ceiling < base.budget().input_ceiling);
        assert_eq!(
            base.budget().input_ceiling - heavy.budget().input_ceiling,
            40_000
        );
    }

    #[test]
    fn observation_previews_are_per_item_capped_and_stop_at_the_ceiling() {
        let planner = ContextPlanner::new(budget(40_000, 2_000, 0, 1_024));
        let mut ctx = assembled();
        ctx.observations = (0..50)
            .map(|i| ObservationPreview {
                fact_id: FactId::from(format!("f{i}")),
                preview: "word ".repeat(5_000),
                validity: Validity::Current,
            })
            .collect();
        let plan = planner.plan(&ctx, RequestScope::Conversational).unwrap();
        let obs_tokens = plan
            .allocations
            .iter()
            .find(|a| a.block == "observations")
            .unwrap()
            .tokens;
        assert!(obs_tokens <= plan.budget.input_ceiling);
        // Not every observation fits; the plan records only what it kept.
        assert!(plan.items.len() < 50);
        for item in &plan.items {
            assert!(item.tokens <= plan.budget.observation_preview_tokens());
        }
    }

    #[test]
    fn every_scope_takes_the_same_path_but_only_conversational_counts() {
        let planner = ContextPlanner::new(budget(200_000, 4_000, 0, 4_000));
        let ctx = assembled();
        for scope in [
            RequestScope::Conversational,
            RequestScope::Summary,
            RequestScope::Ephemeral,
        ] {
            let plan = planner.plan(&ctx, scope).unwrap();
            assert_eq!(plan.scope, scope);
        }
        assert!(RequestScope::Conversational.counts_as_conversational());
        assert!(!RequestScope::Summary.counts_as_conversational());
        assert!(!RequestScope::Ephemeral.counts_as_conversational());
    }
}
