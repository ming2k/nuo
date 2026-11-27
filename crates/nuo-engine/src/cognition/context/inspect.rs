//! Rendering one measured context plan for an operator (ADR-0217, ADR-0220).
//!
//! Three entry points share this module: `/context` in a session, the context
//! dialog in the terminal app, and the golden snapshot in the test suite. They
//! all render the same [`ContextPlanView`], built here from the engine's stored
//! [`ContextMeasurement`], so a discrepancy between what the model saw and what
//! the operator sees is a rendering bug rather than a second code path
//! (`[INV-CTXPLAN-01]`).
//!
//! Nothing here re-measures. The one plan the assembler produced is the one
//! value every reader starts from (`[INV-CTXPLAN-07]`).
use nuo_protocol::{
    ContextMeasurement, ContextNotice, ContextPlan, ContextPlanView, PlannedBlock,
    PlannedBlockView, PrefixDrift,
};

/// Project one measured plan into the operator-facing wire shape.
///
/// `window_tokens` is the active model's declared context window and
/// `instructions_ceiling_tokens` its derived instruction ceiling; both are
/// caller facts (the engine's `Agent` owns the provider), so they are arguments
/// rather than fields on the measurement.
///
/// Bodies come from the plan's own rendered instruction region: `fixed`
/// already holds the exact blocks that became the request, so joining them by
/// id yields the bytes the provider received — no re-render, no second source
/// of truth (`[INV-CTXPLAN-04]`).
pub fn view(
    measurement: &ContextMeasurement,
    window_tokens: usize,
    instructions_ceiling_tokens: usize,
) -> ContextPlanView {
    let plan = &measurement.plan;
    let mut blocks = Vec::with_capacity(plan.blocks.len());
    for row in &plan.blocks {
        blocks.push(PlannedBlockView::new(row, rendered_body(plan, row)));
    }
    ContextPlanView {
        blocks,
        fixed_tokens: plan.fixed_tokens,
        notices: plan.notices.clone(),
        drift: plan.drift.clone(),
        window_tokens,
        instructions_ceiling_tokens,
        estimate: measurement.estimate,
    }
}

/// The rendered text one planned row contributes, joined from the plan's own
/// regions. `None` for an inactive row, which contributes no bytes.
fn rendered_body(plan: &ContextPlan, row: &PlannedBlock) -> Option<String> {
    if !row.active {
        return None;
    }
    plan.fixed
        .iter()
        .find(|block| block.id == row.id)
        .map(|block| nuo_protocol::render_blocks(std::slice::from_ref(block)))
}

/// Render the operator table for one plan — the text form `/context` returns to
/// a non-terminal client.
pub fn render_table(view: &ContextPlanView) -> String {
    let mut out = String::new();
    out.push_str("Region      Tokens   Blocks\n");
    let active_blocks = view.blocks.iter().filter(|b| b.active).count();
    out.push_str(&format!(
        "fixed       {:>6}   {:>6}\n",
        view.fixed_tokens, active_blocks
    ));
    let total = view.fixed_tokens;
    if view.window_tokens > 0 {
        out.push_str(&format!(
            "─────────────────────────────────────────\n\
             instructions {total} / {} ({:.0}% of window)\n",
            view.window_tokens,
            (total as f64 / view.window_tokens as f64) * 100.0
        ));
        out.push_str(&format!(
            "ceiling     {} tokens\n",
            view.instructions_ceiling_tokens
        ));
    } else {
        out.push_str(&format!(
            "─────────────────────────────────────────\ninstructions {total}\n"
        ));
    }
    if let Some(estimate) = view.estimate {
        out.push_str(&format!(
            "\nrequest     {} tokens = instructions {} + history {} + tools {}\n",
            estimate.total_tokens,
            estimate.instructions_tokens,
            estimate.history_tokens,
            estimate.tool_schema_tokens,
        ));
        if let Some(residual) = view.attribution_residual() {
            out.push_str(&format!(
                "attribution {:+#} tokens (per-block sum vs measured region)\n",
                residual
            ));
        }
    }

    out.push_str("\nsources  (frame first, then ascending stability)\n");
    out.push_str(&format!(
        "  {:<24} {:<22} {:<10} {:<10} {:>6}  note\n",
        "id", "topic", "rank", "stability", "tokens"
    ));
    for block in view.blocks.iter().filter(|block| block.active) {
        out.push_str(&row(block));
        out.push('\n');
    }

    let inactive: Vec<&PlannedBlockView> =
        view.blocks.iter().filter(|block| !block.active).collect();
    if !inactive.is_empty() {
        out.push_str("\n  inactive\n");
        for block in inactive {
            out.push_str(&format!(
                "    {:<24} {:<22} {:<10} {:<10}        {}\n",
                block.id,
                block.topic.label(),
                block.rank.label(),
                block.stability.label(),
                block
                    .inactive_reason
                    .as_deref()
                    .unwrap_or("no reason recorded")
            ));
        }
    }

    if !view.notices.is_empty() {
        out.push_str("\n  notices\n");
        for notice in &view.notices {
            out.push_str(&format!(
                "    {:<24} {:<14} {}\n",
                notice.source_id,
                notice_kind_label(notice),
                notice.message
            ));
        }
    }

    out.push_str(&format!("\ndrift: {}\n", describe_drift(&view.drift)));
    out
}

fn row(block: &PlannedBlockView) -> String {
    let note = match &block.elision {
        Some(elision) => format!(
            "elided {} tokens; {}",
            elision.dropped_tokens(),
            elision.resume.instruction()
        ),
        None => String::new(),
    };
    format!(
        "  {:<24} {:<22} {:<10} {:<10} {:>6}  {}",
        block.id,
        block.topic.label(),
        block.rank.label(),
        block.stability.label(),
        block.tokens,
        note
    )
}

/// Lowercase wire name of a notice kind. The kind is a closed enum, so this is
/// exhaustive by construction rather than by a `Debug` format.
fn notice_kind_label(notice: &ContextNotice) -> &'static str {
    match notice.kind {
        nuo_protocol::NoticeKind::Elided => "elided",
        nuo_protocol::NoticeKind::Indexed => "indexed",
        nuo_protocol::NoticeKind::Dropped => "dropped",
        nuo_protocol::NoticeKind::CeilingEvicted => "ceiling_evicted",
        nuo_protocol::NoticeKind::Truncated => "truncated",
    }
}

fn describe_drift(drift: &PrefixDrift) -> String {
    match drift {
        PrefixDrift::None => "none (cacheable prefix unchanged)".to_string(),
        PrefixDrift::Suffix { blocks } => format!("suffix moved: {blocks:?}"),
        PrefixDrift::Head { blocks } => {
            format!("DEFECT — a frame/frozen block moved with no route change: {blocks:?}")
        }
        PrefixDrift::Route => "route changed (expected)".to_string(),
    }
}

/// Render the one machine form (ADR-0220 §5).
///
/// Serializing the view — not a body-less projection of it — is the point: two
/// JSON shapes for one plan is exactly the divergence ADR-0217 exists to
/// prevent, and the body-less form answered strictly fewer questions.
pub fn render_json(view: &ContextPlanView) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_protocol::{Block, ContextPlan, Stability, Topic};

    fn active(id: &str, topic: Topic, stability: Stability, body: &str) -> PlannedBlock {
        let tokens = nuo_tool::count_tokens(body);
        PlannedBlock {
            id: id.to_owned(),
            topic,
            rank: topic.rank(),
            position: topic.position(),
            stability,
            tokens,
            elision: None,
            active: true,
            inactive_reason: None,
        }
    }

    fn measurement(plan: ContextPlan) -> ContextMeasurement {
        ContextMeasurement {
            plan,
            estimate: None,
        }
    }

    #[test]
    fn the_table_shows_active_and_inactive_rows() {
        let plan = ContextPlan {
            blocks: vec![
                active(
                    "agent.identity",
                    Topic::Identity,
                    Stability::Route,
                    "You are.",
                ),
                PlannedBlock::inactive(
                    "project.rules",
                    Topic::ProjectAuthority,
                    Stability::Workspace,
                    "inactive for this request",
                ),
            ],
            fixed: vec![Block::new("agent.identity", "You are.")],
            fixed_tokens: 3,
            ..Default::default()
        };
        let table = render_table(&view(&measurement(plan), 128_000, 10_240));
        assert!(table.contains("agent.identity"));
        assert!(table.contains("inactive"));
        assert!(table.contains("project.rules"));
        assert!(table.contains("inactive for this request"));
        assert!(table.contains("drift: none"));
    }

    #[test]
    fn a_head_drift_is_rendered_as_a_defect() {
        let plan = ContextPlan {
            drift: PrefixDrift::Head {
                blocks: vec!["agent.precedence".to_owned()],
            },
            ..Default::default()
        };
        let view = view(&measurement(plan), 0, 0);
        assert!(render_table(&view).contains("DEFECT"));
    }

    #[test]
    fn the_bodies_are_the_plans_own_rendered_regions() {
        // `[INV-CTXPLAN-04]`: the body a reader sees is the block the request
        // carries, byte for byte — not a re-render that could drift.
        let block = Block::new("project.rules", "Follow the rules.")
            .with_title("Project Rules")
            .with_source("AGENTS.md");
        let rendered = nuo_protocol::render_blocks(std::slice::from_ref(&block));
        let plan = ContextPlan {
            blocks: vec![active(
                "project.rules",
                Topic::ProjectAuthority,
                Stability::Workspace,
                &rendered,
            )],
            fixed: vec![block],
            fixed_tokens: nuo_tool::count_tokens(&rendered),
            ..Default::default()
        };
        let view = view(&measurement(plan), 0, 0);
        let row = view.block("project.rules").expect("row");
        assert_eq!(row.body.as_deref(), Some(rendered.as_str()));
        assert_eq!(
            nuo_tool::count_tokens(row.body.as_deref().unwrap()),
            row.tokens,
            "a body that does not cost its reported tokens is a lying inspector"
        );
    }

    #[test]
    fn emission_order_is_preserved_verbatim() {
        // `[INV-CTXPLAN-05]`: the dialog's order IS the request's order, so no
        // reader may re-sort by cost.
        let plan = ContextPlan {
            blocks: vec![
                active("agent.identity", Topic::Identity, Stability::Route, "a"),
                active(
                    "agent.precedence",
                    Topic::Precedence,
                    Stability::Frozen,
                    "bb",
                ),
                active(
                    "project.rules",
                    Topic::ProjectAuthority,
                    Stability::Workspace,
                    "ccc",
                ),
            ],
            fixed: vec![
                Block::new("agent.identity", "a"),
                Block::new("agent.precedence", "bb"),
                Block::new("project.rules", "ccc"),
            ],
            ..Default::default()
        };
        let view = view(&measurement(plan), 0, 0);
        let ids: Vec<&str> = view.blocks.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["agent.identity", "agent.precedence", "project.rules"]
        );
    }

    #[test]
    fn the_json_form_carries_the_bodies_and_the_layers() {
        let plan = ContextPlan {
            blocks: vec![active(
                "agent.identity",
                Topic::Identity,
                Stability::Route,
                "You are Nuo.",
            )],
            fixed: vec![Block::new("agent.identity", "You are Nuo.")],
            fixed_tokens: 3,
            ..Default::default()
        };
        let mut m = measurement(plan);
        m.estimate = Some(nuo_protocol::RequestTokenEstimate::layered(41, 3, 9));
        let json = render_json(&view(&m, 200_000, 16_000)).unwrap();
        assert!(json.contains("\"topic\": \"identity\""));
        assert!(json.contains("\"rank\": \"directive\""));
        assert!(json.contains("\"body\": \"You are Nuo.\""));
        assert!(json.contains("\"tool_schema_tokens\": 9"));
        assert!(json.contains("\"instructions_ceiling_tokens\": 16000"));
    }

    #[test]
    fn the_table_names_the_attribution_residual() {
        let body = "You are Nuo.";
        let block = active("agent.identity", Topic::Identity, Stability::Route, body);
        let block_tokens = block.tokens;
        let plan = ContextPlan {
            blocks: vec![block],
            fixed: vec![Block::new("agent.identity", body)],
            fixed_tokens: block_tokens,
            ..Default::default()
        };
        let mut m = measurement(plan);
        // The region costs four tokens more than the one block it contains:
        // separators are counted once, whole. The table must say so.
        m.estimate = Some(nuo_protocol::RequestTokenEstimate::layered(
            0,
            block_tokens + 4,
            0,
        ));
        let table = render_table(&view(&m, 100_000, 8_000));
        assert!(
            table.contains("attribution -4"),
            "the gap between per-block attribution and the measured region must be shown, not rounded away:\n{table}"
        );
    }
}
