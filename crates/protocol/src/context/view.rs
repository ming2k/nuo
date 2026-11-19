//! The operator-facing projection of one measured context plan (ADR-0220).
//!
//! [`ContextPlanView`] is the single shape every out-of-process reader renders:
//! the terminal dialog, `/context --json`, and the golden snapshot. It carries
//! the plan *plus* the rendered body of every active block, because "how much
//! does this cost" and "what does this say" are the same question asked twice.
//!
//! # Why a separate type instead of deriving serde on [`ContextPlan`]
//!
//! `ContextPlan` is the assembler's working value: its `fixed`/`transient`
//! vectors hold the blocks that become the request, and a `PlannedBlock` names
//! its inactivity reason as a compile-time `&'static str`. Neither belongs on
//! the wire. The view is a *flattening* — one row per declared source, its band
//! facts, its cost, and its exact bytes — so a reader never has to join two
//! vectors to answer a question.
//!
//! # What is deliberately absent
//!
//! Nothing here is persistable. `request_projections` keeps
//! [`PlannedBlockRecord`](crate::context::PlannedBlockRecord) — ids and costs,
//! no bodies — because a durable copy of the prompt would grow the database
//! without bound (ADR-0217, Alternative D) and would assert something false the
//! moment the workspace moves (`[INV-CTXPLAN-06]`).
use serde::{Deserialize, Serialize};

use super::{
    ContextNotice, ContextPlan, Elision, PlannedBlock, Position, PrefixDrift, Rank, Stability,
    Topic,
};
use crate::pressure::RequestTokenEstimate;

/// One declared source, as an operator sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedBlockView {
    /// Stable source id (`project.rules`, `agent.identity`, …).
    pub id: String,
    pub topic: Topic,
    pub rank: Rank,
    pub position: Position,
    pub stability: Stability,
    /// Measured cost of the emitted body. Zero when inactive.
    pub tokens: usize,
    /// What was cut, and how the model gets it back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elision: Option<Elision>,
    /// Whether this source contributed text to the request.
    pub active: bool,
    /// Why an inactive source contributed nothing. `String` because a wire type
    /// cannot carry the engine's `&'static str`; the engine row stays borrowed
    /// so a per-request plan allocates nothing for a reason it never sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inactive_reason: Option<String>,
    /// The exact rendered text this block contributes — envelope and title
    /// included, byte-identical to what the provider receives. `None` when the
    /// block is inactive.
    ///
    /// `tool::count_tokens(body) == tokens` for every active block
    /// (`[INV-CTXPLAN-04]`): both are derived from the same `render_blocks`
    /// call, so a reader can verify the accounting instead of trusting it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

impl PlannedBlockView {
    /// Project one engine row plus its rendered body.
    ///
    /// `body` is the caller's `render_blocks(&[block])` output; passing it in
    /// (rather than re-rendering here) is what keeps the inspector downstream of
    /// the measurement instead of beside it (`[INV-CTXPLAN-07]`).
    pub fn new(row: &PlannedBlock, body: Option<String>) -> Self {
        Self {
            id: row.id.clone(),
            topic: row.topic,
            rank: row.rank,
            position: row.position,
            stability: row.stability,
            tokens: row.tokens,
            elision: row.elision.clone(),
            active: row.active,
            inactive_reason: row.inactive_reason.map(str::to_owned),
            body: body.filter(|_| row.active),
        }
    }
}

/// One request's context, as every out-of-process reader sees it.
///
/// `blocks` is in **emission order** — the order the request was assembled in,
/// not a re-sorted view. A reader that re-ordered would be a second opinion
/// about the prompt (`[INV-CTXPLAN-05]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPlanView {
    /// Every declared source, active or not, in emission order.
    pub blocks: Vec<PlannedBlockView>,
    /// Sum of the active `fixed` block costs.
    ///
    /// Deliberately the *sum*, not the whole-region token count: it is an
    /// attribution across sources, and tokenizers are not additive, so it can
    /// differ from `estimate.instructions_tokens`. Comparing the two is how a
    /// reader sees the gap between "what each block costs" and "what the region
    /// costs" instead of being told a rounded story.
    pub fixed_tokens: usize,
    /// Sum of the active `transient` block costs.
    pub transient_tokens: usize,
    /// Every lossy decision the assembler made, in emission order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<ContextNotice>,
    /// How the cacheable prefix moved since the previous request on this thread.
    #[serde(default)]
    pub drift: PrefixDrift,
    /// The active model's declared context window. `0` when unknown.
    pub window_tokens: usize,
    /// The instruction-region ceiling derived from that window (ADR-0214).
    pub instructions_ceiling_tokens: usize,
    /// The assembled request's token shape, measured once by the engine.
    ///
    /// `None` when the plan is newer than the last dispatch: a plan is measured
    /// while the request is assembled, and the estimate is filled at the attempt
    /// that sends it. A reader must therefore render the block column without it
    /// rather than invent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<RequestTokenEstimate>,
}

impl ContextPlanView {
    /// The planned row for one source id.
    pub fn block(&self, id: &str) -> Option<&PlannedBlockView> {
        self.blocks.iter().find(|block| block.id == id)
    }

    /// Whether any declared source contributed nothing this request.
    pub fn has_inactive(&self) -> bool {
        self.blocks.iter().any(|block| !block.active)
    }

    /// The residual between the per-block attribution and the measured
    /// instruction region: `sum(blocks) - estimate.instructions`.
    ///
    /// `None` when the estimate is not recorded yet. A non-zero value is normal
    /// (the region's separators are counted once, whole), and showing it is the
    /// honest alternative to pretending the columns partition the prompt.
    pub fn attribution_residual(&self) -> Option<isize> {
        let estimate = self.estimate?;
        let attributed = self
            .blocks
            .iter()
            .filter(|block| block.active)
            .map(|block| block.tokens)
            .sum::<usize>();
        Some(attributed as isize - estimate.instructions_tokens as isize)
    }
}

/// One request's context as the engine measured it: the plan, plus the token
/// shape of the request that plan built.
///
/// These two facts are produced at different instants — the plan while the
/// request is assembled, the estimate at the attempt that sends it — so they are
/// stored together rather than derived from one another. A reader that needs
/// only the plan treats `estimate` as absent; a reader that shows costs shows
/// both or neither.
///
/// Held in memory only. It is what `/context` and the context dialog render
/// (`[INV-CTXPLAN-07]`), and it is the value a `QueryContextPlan` reply is built
/// from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContextMeasurement {
    pub plan: ContextPlan,
    /// `None` until the attempt that sends this plan's request has measured it.
    pub estimate: Option<RequestTokenEstimate>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, tokens: usize, active: bool) -> PlannedBlock {
        PlannedBlock {
            id: id.to_owned(),
            topic: Topic::ProjectAuthority,
            rank: Rank::Directive,
            position: Position::Body,
            stability: Stability::Workspace,
            tokens,
            elision: None,
            active,
            inactive_reason: (!active).then_some("inactive for this request"),
        }
    }

    #[test]
    fn an_active_row_keeps_its_body_and_an_inactive_row_does_not() {
        let active = PlannedBlockView::new(&row("project.rules", 12, true), Some("body".into()));
        assert_eq!(active.body.as_deref(), Some("body"));
        assert_eq!(active.inactive_reason, None);

        let inactive = PlannedBlockView::new(&row("project.rules", 0, false), Some("body".into()));
        assert_eq!(inactive.body, None, "an inactive row has no bytes to show");
        assert_eq!(
            inactive.inactive_reason.as_deref(),
            Some("inactive for this request")
        );
    }

    #[test]
    fn the_residual_needs_an_estimate_and_compares_the_active_column() {
        let mut view = ContextPlanView {
            blocks: vec![
                PlannedBlockView::new(&row("a", 10, true), Some(String::new())),
                PlannedBlockView::new(&row("b", 5, true), Some(String::new())),
                PlannedBlockView::new(&row("c", 0, false), None),
            ],
            ..ContextPlanView::default()
        };
        assert_eq!(view.attribution_residual(), None, "no estimate recorded");

        view.estimate = Some(RequestTokenEstimate {
            history_tokens: 0,
            overhead_tokens: 17,
            total_tokens: 17,
            instructions_tokens: 17,
            tool_schema_tokens: 0,
            temporary_context_tokens: 0,
        });
        assert_eq!(
            view.attribution_residual(),
            Some(-2),
            "an inactive row must not be counted as attributed cost"
        );
    }

    #[test]
    fn the_view_round_trips_through_the_wire_including_an_owned_resume_tool() {
        let mut active = row("workspace.topology", 39, true);
        active.elision = Some(Elision {
            full_tokens: 90,
            kept_tokens: 39,
            resume: super::super::Resume::Tool {
                tool: "find_files".to_owned(),
            },
        });
        let view = ContextPlanView {
            blocks: vec![PlannedBlockView::new(&active, Some("[...]".into()))],
            fixed_tokens: 39,
            window_tokens: 200_000,
            instructions_ceiling_tokens: 16_000,
            ..ContextPlanView::default()
        };
        let json = serde_json::to_string(&view).unwrap();
        let restored: ContextPlanView = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, view);
        assert!(json.contains("\"tool\":\"find_files\""));
    }

    #[test]
    fn an_inactive_row_decodes_without_a_reason_field() {
        // A reply written by a peer that never names a reason must still decode:
        // the absence is the peer's choice, not a malformed frame.
        let json = r#"{"id":"x","topic":"memory","rank":"reference","position":"body",
                       "stability":"turn","tokens":0,"active":false}"#;
        let view: PlannedBlockView = serde_json::from_str(json).unwrap();
        assert_eq!(view.inactive_reason, None);
        assert_eq!(view.body, None);
    }
}
