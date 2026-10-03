//! Bounded checkpoint builder and CAS-guarded view commit (ADR-0278).
//!
//! This is the Phase D substrate: it turns a fact interval into a derived
//! [`Checkpoint`] and a branch-local [`ContextView`], and commits the pair under
//! a compare-and-swap on the branch revision. It realizes:
//!
//! - `INV-CKPT-01`: the cut point is validated against execution-group closure —
//!   an open group is refused, not split.
//! - `INV-CKPT-02`: the checkpoint references a hash-verified source manifest and
//!   the prior checkpoint; it never inserts a parent edge and never reparents.
//! - `INV-CKPT-03`: mandatory state (objective, constraints, unresolved items,
//!   confirmed effects, evidence IDs) is preserved, and a failed summary falls
//!   back to a deterministic template without recursive growth.
//! - `INV-CKPT-04`: generation runs outside the transaction with per-call and
//!   total time/token bounds, and the commit is a bounded-retry CAS.
//!
//! The builder is pure: the LLM is injected through [`Summarizer`] and time
//! through [`Clock`], so the whole flow is deterministic and testable without a
//! provider or a database. The commit helper takes an already-open `Connection`
//! (ADR-0231 one-door) and never opens its own.

use nuo_wire::context_lifecycle::{
    Checkpoint, CheckpointId, ContextView, ExecutionGroup, FactId, FactNode, Representation,
    RepresentationEntry, SourceAuthority, SourceInterval, SourceManifest, Validity, ViewId,
};
use nuo_wire::tokenizer;
use sha2::{Digest, Sha256};

/// Wall-clock access, injected so the builder stays pure and testable.
pub trait Clock {
    /// Epoch milliseconds.
    fn now_ms(&self) -> u64;
}

/// A real clock backed by the system time.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// A fixed clock for tests.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    /// The fixed instant.
    pub now_ms: u64,
}

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.now_ms
    }
}

/// One shard of the map phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryShard {
    /// The facts covered by this shard.
    pub fact_ids: Vec<FactId>,
    /// The rendered source text.
    pub rendered: String,
    /// Token count of `rendered`.
    pub tokens: u64,
}

/// Why a summarization call failed (ADR-0278 §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryFailure {
    /// The call exceeded its deadline.
    Timeout,
    /// The model returned more than the requested ceiling.
    TooLong {
        /// The tokens returned.
        tokens: u64,
        /// The ceiling requested.
        ceiling: u64,
    },
    /// The model returned nothing usable.
    Empty,
    /// The provider returned an error.
    Error(String),
}

/// The injected summarizer. The real implementation calls the provider; tests
/// inject a stub. The builder never calls a model itself.
#[async_trait::async_trait]
pub trait Summarizer: Send + Sync {
    /// Summarize one shard into at most `max_tokens` tokens, honoring `deadline_ms`.
    async fn summarize(
        &self,
        shard: &SummaryShard,
        max_tokens: u64,
        deadline_ms: u64,
        now_ms: u64,
    ) -> Result<String, SummaryFailure>;
}

/// A summarizer that always fails — the deterministic-fallback path.
#[derive(Debug, Clone, Copy, Default)]
pub struct FailingSummarizer;

#[async_trait::async_trait]
impl Summarizer for FailingSummarizer {
    async fn summarize(
        &self,
        _shard: &SummaryShard,
        _max_tokens: u64,
        _deadline_ms: u64,
        _now_ms: u64,
    ) -> Result<String, SummaryFailure> {
        Err(SummaryFailure::Error("no summarizer configured".into()))
    }
}

/// The required sections of a checkpoint summary (ADR-0278 §3).
pub const REQUIRED_SECTIONS: &[&str] = &[
    "## Objective",
    "## Completed",
    "## Unresolved",
    "## Evidence",
];

/// The input to a checkpoint build.
#[derive(Debug, Clone)]
pub struct CheckpointPlan {
    /// Target branch.
    pub branch_id: String,
    /// The fact interval to summarize, in ascending sequence order.
    pub facts: Vec<FactNode>,
    /// The execution group immediately after the cut; it must be closed.
    pub boundary_group: ExecutionGroup,
    /// The prior checkpoint, if the branch already had one.
    pub prior_checkpoint_id: Option<CheckpointId>,
    /// Facts the summary must keep referenced (constraints, open items).
    pub mandatory_fact_refs: Vec<FactId>,
    /// The fact revision this checkpoint was built from.
    pub basis_revision: u64,
    /// The policy revision in force.
    pub policy_revision: u32,
    /// The checkpoint identity to mint.
    pub checkpoint_id: CheckpointId,
    /// The view identity to mint.
    pub view_id: ViewId,
}

/// Why a checkpoint build failed (ADR-0278 §2, §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointError {
    /// The cut would split an open execution group (`INV-CKPT-01`).
    OpenExecutionGroup {
        /// The outstanding call IDs.
        open_calls: Vec<String>,
    },
    /// There is nothing to summarize.
    EmptySource,
    /// A mandatory fact reference is absent from the summary text.
    MissingMandatoryRef {
        /// The missing fact.
        fact_id: FactId,
    },
    /// A required section is absent from the summary text.
    MissingSection {
        /// The missing section heading.
        section: String,
    },
    /// The total build budget was exceeded.
    DeadlineExceeded,
    /// The summary exceeds the checkpoint ceiling even after reduction.
    OverCeiling {
        /// Tokens produced.
        tokens: u64,
        /// The ceiling.
        ceiling: u64,
    },
}

impl std::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckpointError::OpenExecutionGroup { open_calls } => write!(
                f,
                "refusing to compact across an open execution group: {open_calls:?}"
            ),
            CheckpointError::EmptySource => write!(f, "no facts to summarize"),
            CheckpointError::MissingMandatoryRef { fact_id } => {
                write!(f, "summary dropped mandatory fact {fact_id}")
            }
            CheckpointError::MissingSection { section } => {
                write!(f, "summary is missing required section {section}")
            }
            CheckpointError::DeadlineExceeded => write!(f, "checkpoint build exceeded its budget"),
            CheckpointError::OverCeiling { tokens, ceiling } => {
                write!(f, "summary {tokens} exceeds ceiling {ceiling}")
            }
        }
    }
}

impl std::error::Error for CheckpointError {}

/// A validated checkpoint plus its branch view (ADR-0278 §1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCheckpoint {
    /// The derived checkpoint (never an execution parent).
    pub checkpoint: Checkpoint,
    /// The branch view that switches to the checkpoint.
    pub view: ContextView,
    /// Whether the deterministic fallback produced the summary.
    pub used_fallback: bool,
}

/// The bounded checkpoint builder (ADR-0278 §3–§4).
pub struct CheckpointBuilder<'a> {
    summarizer: &'a dyn Summarizer,
    clock: &'a dyn Clock,
    /// Per-call timeout (ADR-0280 default 45 s).
    call_timeout_ms: u64,
    /// Total build budget (ADR-0280 default 120 s).
    total_budget_ms: u64,
    /// Checkpoint output ceiling.
    output_ceiling: u64,
    /// Map/reduce depth bound.
    max_reduce_depth: u32,
}

impl<'a> CheckpointBuilder<'a> {
    /// Build a builder with explicit bounds.
    pub fn new(
        summarizer: &'a dyn Summarizer,
        clock: &'a dyn Clock,
        call_timeout_ms: u64,
        total_budget_ms: u64,
        output_ceiling: u64,
    ) -> Self {
        Self {
            summarizer,
            clock,
            call_timeout_ms,
            total_budget_ms,
            output_ceiling,
            max_reduce_depth: 4,
        }
    }

    /// Build and validate a checkpoint for `plan`.
    pub async fn build(&self, plan: &CheckpointPlan) -> Result<ValidatedCheckpoint, CheckpointError> {
        // INV-CKPT-01: never cut across an open execution group.
        if !plan.boundary_group.is_closed() {
            return Err(CheckpointError::OpenExecutionGroup {
                open_calls: plan
                    .boundary_group
                    .open_calls()
                    .map(|c| c.provider_call_id.clone())
                    .collect(),
            });
        }
        if plan.facts.is_empty() {
            return Err(CheckpointError::EmptySource);
        }

        if self.total_budget_ms == 0 || self.output_ceiling == 0 {
            return Err(CheckpointError::DeadlineExceeded);
        }
        let deadline = self.clock.now_ms().saturating_add(self.total_budget_ms);
        let mut work = SummaryWork {
            deadline: tokio::time::Instant::now() + std::time::Duration::from_millis(self.total_budget_ms),
            calls: 0, input: 0, output: 0,
        };
        let manifest = source_manifest(&plan.facts);

        // --- Map phase: shard by the output ceiling --------------------------
        let shards = shard(&plan.facts, self.output_ceiling);
        let mut parts: Vec<String> = Vec::new();
        for shard in &shards {
            if self.clock.now_ms() >= deadline {
                return Err(CheckpointError::DeadlineExceeded);
            }
            let call_deadline =
                deadline.min(self.clock.now_ms().saturating_add(self.call_timeout_ms));
            match self.call_summary(shard, call_deadline, &mut work).await {
                Ok(text) if !text.trim().is_empty() => parts.push(text),
                _ => {
                    // One bounded repair is implicit: a failed call falls
                    // through to the deterministic fallback for its shard.
                    parts.push(fallback_shard(shard));
                }
            }
        }

        // --- Reduce phase: bounded, with a deterministic join ----------------
        let mut summary = self.reduce(parts, deadline, &mut work).await
            .unwrap_or_else(|_| self.deterministic_template(plan));
        self.append_mandatory(&mut summary, plan)?;

        // --- Validation: sections, mandatory refs, size ----------------------
        let mut used_fallback = summary.contains(FALLBACK_MARKER);
        if let Err(err) = self.validate(&summary, plan) {
            // One bounded repair: regenerate via the deterministic template.
            summary = self.deterministic_template(plan);
            self.append_mandatory(&mut summary, plan)?;
            used_fallback = true;
            self.validate(&summary, plan).map_err(|_| err)?;
        }

        let checkpoint = Checkpoint {
            checkpoint_id: plan.checkpoint_id.clone(),
            source_manifest: manifest,
            prior_checkpoint_id: plan.prior_checkpoint_id.clone(),
            summary,
            mandatory_fact_refs: plan.mandatory_fact_refs.clone(),
            source_authority: SourceAuthority::Derived,
        };
        let view = ContextView {
            view_id: plan.view_id.clone(),
            branch_id: plan.branch_id.clone().into(),
            basis_revision: plan.basis_revision,
            checkpoint_id: Some(plan.checkpoint_id.clone()),
            tail_after_fact_id: plan.facts.last().map(|f| f.id.clone()),
            representations: plan
                .facts
                .iter()
                .map(|f| RepresentationEntry {
                    fact_id: f.id.clone(),
                    representation: Representation::Summary,
                    validity: Validity::Current,
                })
                .collect(),
            policy_revision: plan.policy_revision,
        };
        Ok(ValidatedCheckpoint {
            checkpoint,
            view,
            used_fallback,
        })
    }

    async fn reduce(&self, mut parts: Vec<String>, deadline: u64, work: &mut SummaryWork) -> Result<String, CheckpointError> {
        let mut depth = 0;
        loop {
            let joined = parts.join("\n\n");
            let tokens = tokenizer::count_tokens(&joined) as u64;
            if tokens <= self.output_ceiling || parts.len() == 1 {
                if tokens > self.output_ceiling {
                    return Err(CheckpointError::OverCeiling {
                        tokens,
                        ceiling: self.output_ceiling,
                    });
                }
                return Ok(joined);
            }
            if depth >= self.max_reduce_depth {
                // Bounded: stop reducing and let validation fall back.
                return Ok(joined);
            }
            if self.clock.now_ms() >= deadline {
                return Err(CheckpointError::DeadlineExceeded);
            }
            // Pairwise reduce.
            let mut next = Vec::new();
            for pair in parts.chunks(2) {
                let shard = SummaryShard {
                    fact_ids: Vec::new(),
                    rendered: pair.join("\n\n"),
                    tokens: tokenizer::count_tokens(&pair.join("\n\n")) as u64,
                };
                let call_deadline =
                    deadline.min(self.clock.now_ms().saturating_add(self.call_timeout_ms));
                match self.call_summary(&shard, call_deadline, work).await {
                    Ok(text) if !text.trim().is_empty() => next.push(text),
                    _ => next.push(shard.rendered.clone()),
                }
            }
            if next.iter().map(|p| tokenizer::count_tokens(p)).sum::<usize>() >= tokens as usize {
                return Err(CheckpointError::OverCeiling { tokens, ceiling: self.output_ceiling });
            }
            parts = next;
            depth += 1;
        }
    }

    async fn call_summary(&self, shard: &SummaryShard, deadline_ms: u64, work: &mut SummaryWork)
        -> Result<String, SummaryFailure> {
        // Reserve before dispatch. Failed/unknown calls do not refund budget.
        let max_total = self.output_ceiling.saturating_mul(8);
        if shard.tokens > self.output_ceiling || work.calls >= 16
            || work.input.saturating_add(shard.tokens) > max_total
            || work.output.saturating_add(self.output_ceiling) > max_total {
            return Err(SummaryFailure::Error("summary work budget exhausted".into()));
        }
        work.calls += 1;
        work.input += shard.tokens;
        work.output += self.output_ceiling;
        let deadline = work.deadline.min(tokio::time::Instant::now()
            + std::time::Duration::from_millis(self.call_timeout_ms));
        let text = tokio::time::timeout_at(deadline,
            self.summarizer.summarize(shard, self.output_ceiling, deadline_ms, self.clock.now_ms()))
            .await.map_err(|_| SummaryFailure::Timeout)??;
        let tokens = tokenizer::count_tokens(&text) as u64;
        if tokens > self.output_ceiling { return Err(SummaryFailure::TooLong { tokens, ceiling: self.output_ceiling }); }
        if text.trim().is_empty() { return Err(SummaryFailure::Empty); }
        Ok(text)
    }

    fn append_mandatory(&self, summary: &mut String, plan: &CheckpointPlan) -> Result<(), CheckpointError> {
        summary.push_str("\n## Required source state\n");
        for id in &plan.mandatory_fact_refs {
            let fact = plan.facts.iter().find(|f| f.id == *id)
                .ok_or_else(|| CheckpointError::MissingMandatoryRef { fact_id: id.clone() })?;
            summary.push_str(&render_fact(fact));
            summary.push('\n');
        }
        for call in plan.boundary_group.unresolved_external_calls() {
            summary.push_str(&format!("External outcome remains unknown: {} ({:?}); do not replay.\n",
                call.execution_id.as_str(), call.external));
        }
        Ok(())
    }

    fn validate(&self, summary: &str, plan: &CheckpointPlan) -> Result<(), CheckpointError> {
        for section in REQUIRED_SECTIONS {
            if !summary.contains(section) {
                return Err(CheckpointError::MissingSection {
                    section: (*section).to_string(),
                });
            }
        }
        for fact_id in &plan.mandatory_fact_refs {
            if !summary.contains(fact_id.as_str()) {
                return Err(CheckpointError::MissingMandatoryRef {
                    fact_id: fact_id.clone(),
                });
            }
        }
        let tokens = tokenizer::count_tokens(summary) as u64;
        if tokens > self.output_ceiling {
            return Err(CheckpointError::OverCeiling {
                tokens,
                ceiling: self.output_ceiling,
            });
        }
        Ok(())
    }

    /// The deterministic fallback template (ADR-0278 §4): preserves mandatory
    /// state and references the remaining evidence, never nests a full older
    /// summary and never byte-slices.
    fn deterministic_template(&self, plan: &CheckpointPlan) -> String {
        let mut out = String::new();
        out.push_str(FALLBACK_MARKER);
        out.push('\n');
        out.push_str("## Objective\n");
        out.push_str("- summarized fact interval of ");
        out.push_str(&plan.facts.len().to_string());
        out.push_str(" facts\n");
        out.push_str("## Completed\n");
        out.push_str("- No completion inferred; consult the source manifest.\n");
        out.push_str("## Unresolved\n");
        out.push_str("- see evidence references\n");
        out.push_str("## Evidence\n");
        for fact_id in &plan.mandatory_fact_refs {
            out.push_str("- ");
            out.push_str(fact_id.as_str());
            out.push('\n');
        }
        out
    }
}

struct SummaryWork {
    deadline: tokio::time::Instant,
    calls: u32,
    input: u64,
    output: u64,
}

/// Marker identifying a deterministic fallback summary.
pub const FALLBACK_MARKER: &str = "[deterministic checkpoint fallback]";

fn fallback_shard(shard: &SummaryShard) -> String {
    let mut out = String::new();
    out.push_str(FALLBACK_MARKER);
    out.push('\n');
    out.push_str("## Objective\n- shard of ");
    out.push_str(&shard.fact_ids.len().to_string());
    out.push_str(" facts\n## Completed\n");
    out.push_str("- No completion inferred.\n");
    out.push_str("## Unresolved\n- see evidence references\n## Evidence\n");
    for id in &shard.fact_ids {
        out.push_str("- ");
        out.push_str(id.as_str());
        out.push('\n');
    }
    out
}

/// Split the fact interval into shards that each fit the output ceiling.
fn shard(facts: &[FactNode], ceiling: u64) -> Vec<SummaryShard> {
    let mut shards: Vec<SummaryShard> = Vec::new();
    let mut current = SummaryShard {
        fact_ids: Vec::new(),
        rendered: String::new(),
        tokens: 0,
    };
    for fact in facts {
        let rendered = render_fact(fact);
        let tokens = tokenizer::count_tokens(&rendered) as u64;
        if !current.fact_ids.is_empty() && current.tokens + tokens > ceiling {
            shards.push(std::mem::replace(
                &mut current,
                SummaryShard {
                    fact_ids: Vec::new(),
                    rendered: String::new(),
                    tokens: 0,
                },
            ));
        }
        current.fact_ids.push(fact.id.clone());
        current.rendered.push_str(&rendered);
        current.rendered.push('\n');
        current.tokens += tokens;
    }
    if !current.fact_ids.is_empty() {
        shards.push(current);
    }
    shards
}

fn render_fact(fact: &FactNode) -> String {
    format!(
        "[{}] {}",
        fact.id.as_str(),
        serde_json::to_string(&fact.payload).unwrap_or_default()
    )
}

/// A hash-verified ordered manifest of the summarized interval (ADR-0278 §1).
fn source_manifest(facts: &[FactNode]) -> SourceManifest {
    let mut hasher = Sha256::new();
    for fact in facts {
        let bytes = serde_json::to_vec(fact).expect("fact payload is JSON serializable");
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    let hash = format!("{:x}", hasher.finalize());
    SourceManifest {
        intervals: vec![SourceInterval {
            from_seq: facts.first().map(|f| f.seq).unwrap_or(0),
            to_seq: facts.last().map(|f| f.seq).unwrap_or(0),
            hash,
        }],
    }
}

/// The default CAS retry bound (ADR-0280 §2), re-exported from the domain
/// contracts for callers of the persistence-door CAS helper.
pub const DEFAULT_CAS_ATTEMPTS: u32 = nuo_wire::context_lifecycle::CAS_MAX_ATTEMPTS;

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_wire::context_lifecycle::{ResultAcceptance, ExternalExecution, FactPayload, GroupCall, RoundId, TurnId};

    struct StubSummarizer {
        respond: bool,
        text: String,
    }

    #[async_trait::async_trait]
impl Summarizer for StubSummarizer {
        async fn summarize(
            &self,
            _shard: &SummaryShard,
            _max_tokens: u64,
            _deadline_ms: u64,
            _now_ms: u64,
        ) -> Result<String, SummaryFailure> {
            if self.respond {
                Ok(self.text.clone())
            } else {
                Err(SummaryFailure::Error("stub".into()))
            }
        }
    }

    fn good_summary(refs: &[FactId]) -> String {
        let mut s = String::from(
            "## Objective\n- fix the failing test\n## Completed\n- did work\n## Unresolved\n- rerun\n## Evidence\n",
        );
        for r in refs {
            s.push_str("- ");
            s.push_str(r.as_str());
            s.push('\n');
        }
        s
    }

    fn closed_group() -> ExecutionGroup {
        ExecutionGroup::new(vec![GroupCall {
            provider_call_id: "call_1".into(),
            execution_id: "exec-1".into(),
            acceptance: ResultAcceptance::Completed,
            external: ExternalExecution::TerminationConfirmed,
        }])
    }

    fn open_group() -> ExecutionGroup {
        ExecutionGroup::new(vec![GroupCall {
            provider_call_id: "call_open".into(),
            execution_id: "exec-2".into(),
            acceptance: ResultAcceptance::Open,
            external: ExternalExecution::InFlight,
        }])
    }

    fn fact(seq: u64) -> FactNode {
        FactNode {
            id: FactId::from(format!("f{seq}")),
            session_id: "s1".into(),
            branch_origin: "main".into(),
            parent_ids: if seq == 1 {
                vec![]
            } else {
                vec![FactId::from(format!("f{}", seq - 1))]
            },
            seq,
            round_id: RoundId::from("r1"),
            turn_id: TurnId::from("t1"),
            payload: FactPayload::UserMessage {
                text: format!("message {seq}"),
            },
            source_authority: SourceAuthority::User,
            sensitivity: nuo_wire::context_lifecycle::Sensitivity::Internal,
            artifact_refs: vec![],
        }
    }

    fn plan(facts: Vec<FactNode>, group: ExecutionGroup, refs: Vec<FactId>) -> CheckpointPlan {
        CheckpointPlan {
            branch_id: "main".into(),
            facts,
            boundary_group: group,
            prior_checkpoint_id: None,
            mandatory_fact_refs: refs,
            basis_revision: 1,
            policy_revision: 1,
            checkpoint_id: CheckpointId::from("k1"),
            view_id: ViewId::from("v1"),
        }
    }

    struct PendingSummarizer;
    #[async_trait::async_trait]
    impl Summarizer for PendingSummarizer {
        async fn summarize(&self, _: &SummaryShard, _: u64, _: u64, _: u64) -> Result<String, SummaryFailure> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn pending_summary_is_cancelled_by_real_deadline_and_preserves_requirements() {
        let clock = FixedClock { now_ms: 0 };
        let sum = PendingSummarizer;
        let builder = CheckpointBuilder::new(&sum, &clock, 1, 10, 4096);
        let p = plan(vec![fact(1)], closed_group(), vec!["f1".into()]);
        let out = tokio::time::timeout(std::time::Duration::from_millis(200), builder.build(&p))
            .await.unwrap().unwrap();
        assert!(out.used_fallback);
        assert!(out.checkpoint.summary.contains("message 1"));
    }

    #[tokio::test]
    async fn source_manifest_hash_covers_payload_not_only_ids() {
        let a = fact(1);
        let mut b = a.clone();
        b.payload = FactPayload::UserMessage { text: "changed constraint".into() };
        assert_ne!(source_manifest(&[a]), source_manifest(&[b]));
    }

    #[tokio::test]
    async fn an_open_execution_group_is_refused() {
        let clock = FixedClock { now_ms: 0 };
        let sum = StubSummarizer {
            respond: true,
            text: good_summary(&[]),
        };
        let builder = CheckpointBuilder::new(&sum, &clock, 45_000, 120_000, 4_096);
        let p = plan(vec![fact(1)], open_group(), vec![]);
        match builder.build(&p).await.unwrap_err() {
            CheckpointError::OpenExecutionGroup { open_calls } => {
                assert_eq!(open_calls, vec!["call_open".to_string()]);
            }
            other => panic!("expected closure refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_closed_group_builds_a_checkpoint_with_a_source_manifest() {
        let clock = FixedClock { now_ms: 0 };
        let refs = vec![FactId::from("f1")];
        let sum = StubSummarizer {
            respond: true,
            text: good_summary(&refs),
        };
        let builder = CheckpointBuilder::new(&sum, &clock, 45_000, 120_000, 4_096);
        let p = plan(vec![fact(1), fact(2)], closed_group(), refs);
        let out = builder.build(&p).await.unwrap();
        assert!(!out.used_fallback);
        assert_eq!(out.checkpoint.source_authority, SourceAuthority::Derived);
        assert_eq!(out.checkpoint.source_manifest.intervals[0].from_seq, 1);
        assert_eq!(out.checkpoint.source_manifest.intervals[0].to_seq, 2);
        // The view points at the real ancestry boundary, not a reparented node.
        assert_eq!(out.view.tail_after_fact_id, Some(FactId::from("f2")));
        assert_eq!(out.view.checkpoint_id, Some(CheckpointId::from("k1")));
    }

    #[tokio::test]
    async fn a_failed_summary_falls_back_to_a_deterministic_template() {
        let clock = FixedClock { now_ms: 0 };
        let refs = vec![FactId::from("f1")];
        let sum = FailingSummarizer;
        let builder = CheckpointBuilder::new(&sum, &clock, 45_000, 120_000, 4_096);
        let p = plan(vec![fact(1)], closed_group(), refs.clone());
        let out = builder.build(&p).await.unwrap();
        assert!(out.used_fallback);
        assert!(out.checkpoint.summary.contains(FALLBACK_MARKER));
        // Mandatory state survives the fallback (INV-CKPT-03).
        for r in &refs {
            assert!(out.checkpoint.summary.contains(r.as_str()));
        }
        for section in REQUIRED_SECTIONS {
            assert!(out.checkpoint.summary.contains(section));
        }
    }

    #[tokio::test]
    async fn mandatory_text_is_preserved_independently_of_generated_narrative() {
        let clock = FixedClock { now_ms: 0 };
        let refs = vec![FactId::from("f1")];
        // The model returns sections but omits the mandatory fact id.
        let sum = StubSummarizer {
            respond: true,
            text: "## Objective\n- x\n## Completed\n- y\n## Unresolved\n- z\n## Evidence\n".into(),
        };
        let builder = CheckpointBuilder::new(&sum, &clock, 45_000, 120_000, 4_096);
        let p = plan(vec![fact(1)], closed_group(), refs);
        let out = builder.build(&p).await.unwrap();
        assert!(out.checkpoint.summary.contains("f1"));
        assert!(out.checkpoint.summary.contains("message 1"));
    }

    #[tokio::test]
    async fn the_total_budget_is_enforced() {
        let clock = FixedClock { now_ms: 0 };
        let sum = FailingSummarizer;
        // Zero total budget: the deadline is already reached at now_ms == 0.
        let builder = CheckpointBuilder::new(&sum, &clock, 45_000, 0, 4_096);
        let p = plan(vec![fact(1)], closed_group(), vec![]);
        // With a zero budget the first shard loop sees now >= deadline.
        let err = builder.build(&p).await.unwrap_err();
        assert_eq!(err, CheckpointError::DeadlineExceeded);
    }

    #[tokio::test]
    async fn an_empty_source_is_refused() {
        let clock = FixedClock { now_ms: 0 };
        let sum = FailingSummarizer;
        let builder = CheckpointBuilder::new(&sum, &clock, 45_000, 120_000, 4_096);
        let p = plan(vec![], closed_group(), vec![]);
        assert_eq!(builder.build(&p).await.unwrap_err(), CheckpointError::EmptySource);
    }
}
