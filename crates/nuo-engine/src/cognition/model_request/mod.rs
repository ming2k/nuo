//! Request-scoped model-request assembly (ADR-0056, ADR-0215, ADR-0217).
//!
//! The assembler is intentionally independent of [`crate::Agent`]. The agent owns
//! *when* assembly occurs and supplies a plain [`ContextInputs`] snapshot plus the
//! route fingerprint; this module owns the pure projection from a live
//! conversation window plus a compiled [`Policy`] to one immutable
//! [`nuo_protocol::ModelRequest`].
//!
//! Two steps, deliberately separate:
//!
//! 1. [`ModelRequestAssembler::plan`] measures the context and classifies prefix
//!    drift. The returned [`ContextPlan`] is what an inspector renders.
//! 2. [`ModelRequestAssembler::build`] renders the request **from that plan**, so
//!    the text the model receives and the table an operator reads are the same
//!    bytes ([`INV-CTXPLAN-01`]).
//!
//! Neither step can fail: layout is proven once by `Policy::compile`
//! (`[INV-CTXSRC-05]`).
use std::sync::Arc;
use std::sync::Mutex;

use nuo_protocol::{ContextPlan, PrefixDrift, Stability};

use super::context::{ContextInputs, Policy};

/// Remembers the previous request's prefix so drift can be classified.
///
/// The fingerprint is a SHA-256 identity of the cacheable prefix
/// (`nuo_protocol::request_prefix_fingerprint`); the per-block digests are held only
/// long enough to name what moved and are never persisted
/// ([`INV-CTXPLAN-03`]).
#[derive(Default)]
struct PrefixWitness {
    fingerprint: String,
    route: String,
    blocks: Vec<WitnessedBlock>,
}

struct WitnessedBlock {
    id: String,
    stability: Stability,
    digest: u64,
}

/// Pure request projector configured with one compiled context policy.
pub(crate) struct ModelRequestAssembler {
    policy: Policy,
    witness: Mutex<PrefixWitness>,
}

impl ModelRequestAssembler {
    pub(crate) fn new(policy: Policy) -> Self {
        Self {
            policy,
            witness: Mutex::new(PrefixWitness::default()),
        }
    }

    /// Measure the context for this request and classify prefix drift.
    ///
    /// `route` is the route fingerprint; `ceiling_tokens` is the instruction
    /// ceiling derived from the active model window.
    pub(crate) fn plan(
        &self,
        context: &ContextInputs,
        route: &str,
        ceiling_tokens: usize,
    ) -> ContextPlan {
        let mut plan = self.policy.plan_with_ceiling(context, ceiling_tokens);
        let blocks: Vec<WitnessedBlock> = plan
            .fixed
            .iter()
            .map(|block| WitnessedBlock {
                id: block.id.clone(),
                stability: plan
                    .block(&block.id)
                    .map_or(Stability::Frozen, |row| row.stability),
                digest: digest(&nuo_protocol::render_blocks(std::slice::from_ref(block))),
            })
            .collect();
        let fingerprint = nuo_protocol::request_prefix_fingerprint::<()>(
            &nuo_protocol::InstructionText::new(nuo_protocol::render_blocks(&plan.fixed)),
            &[],
            &[],
        );
        plan.drift = self.classify(route, &fingerprint, &blocks);
        if let Ok(mut witness) = self.witness.lock() {
            *witness = PrefixWitness {
                fingerprint,
                route: route.to_owned(),
                blocks,
            };
        }
        plan
    }

    /// Render one request from a plan produced by [`Self::plan`].
    pub(crate) fn build(
        &self,
        plan: ContextPlan,
        messages: Vec<crate::Message>,
        tools: &[Arc<dyn crate::Tool>],
    ) -> nuo_protocol::ModelRequest {
        nuo_protocol::RequestBuilder {
            tools: tool_specs(tools),
            instructions: plan.fixed,
            messages,
        }
        .build()
    }

    /// Render one request from a durable thread projection.
    ///
    /// This is the replay/branch-switch entry point: the fact window is projected
    /// to a `RequestView`, then the one `RequestBuilder` renders the final
    /// request from the same plan shape the live path uses.
    pub(crate) fn build_from_thread(
        &self,
        plan: ContextPlan,
        thread: &nuo_protocol::Thread,
        branch: &nuo_protocol::BranchId,
        tools: &[Arc<dyn crate::Tool>],
    ) -> Result<nuo_protocol::ModelRequest, nuo_protocol::ThreadError> {
        let window = nuo_protocol::ThreadWindow::from_thread(thread, branch, usize::MAX)?;
        let view = nuo_protocol::RequestView::from_window(&window);
        let request_context = nuo_protocol::RequestContext::new(plan.fixed, tool_specs(tools));
        Ok(nuo_protocol::RequestBuilder::from_view(&view, &request_context).build())
    }

    fn classify(&self, route: &str, fingerprint: &str, blocks: &[WitnessedBlock]) -> PrefixDrift {
        let Ok(witness) = self.witness.lock() else {
            return PrefixDrift::None;
        };
        if witness.fingerprint.is_empty() {
            return PrefixDrift::None;
        }
        if witness.route != route {
            return PrefixDrift::Route;
        }
        if witness.fingerprint == fingerprint {
            return PrefixDrift::None;
        }
        let mut moved: Vec<String> = Vec::new();
        let mut head = false;
        for block in blocks {
            let previous = witness.blocks.iter().find(|prior| prior.id == block.id);
            if previous.is_some_and(|prior| prior.digest == block.digest) {
                continue;
            }
            moved.push(block.id.clone());
            head |= matches!(block.stability, Stability::Frozen | Stability::Route);
        }
        for prior in &witness.blocks {
            if blocks.iter().any(|block| block.id == prior.id) {
                continue;
            }
            moved.push(prior.id.clone());
            head |= matches!(prior.stability, Stability::Frozen | Stability::Route);
        }
        if head {
            PrefixDrift::Head { blocks: moved }
        } else {
            PrefixDrift::Suffix { blocks: moved }
        }
    }
}

fn tool_specs(tools: &[Arc<dyn crate::Tool>]) -> Vec<nuo_protocol::capability::ToolSpec> {
    tools
        .iter()
        .map(|tool| {
            nuo_protocol::capability::ToolSpec::new(
                tool.name(),
                tool.description(),
                tool.parameters_schema(),
            )
        })
        .collect()
}

/// In-process content digest. Never persisted: a `DefaultHasher` value is not
/// stable across builds, so durable identity is the SHA-256 prefix instead.
fn digest(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}
