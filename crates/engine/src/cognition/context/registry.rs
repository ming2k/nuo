//! The context policy compiler and the measured plan.
//!
//! `Policy::compile` proves every invariant that does not need a live turn —
//! ordering, topic ownership, supersede resolution, frame volatility, rank
//! placement, and non-empty budgets. `Policy::plan` is then total: no `Result`,
//! no `panic!`, so a layout defect can never surface on the request path
//! (`[INV-CTXSRC-05]`, ADR-0215).
use std::collections::{HashMap, HashSet};

use protocol::{
    Block, ContextNotice, ContextPlan, Elision, NoticeKind, Overflow, PlannedBlock, Position, Rank,
    Stability, Topic,
};

use super::inputs::ContextInputs;
use super::source::{ContextSource, SourceMeta, SourceOrder};

/// A policy that failed to compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextPolicyError {
    /// Two sources share an id.
    DuplicateId(&'static str),
    /// Two sources own one topic and neither supersedes the other.
    DuplicateTopic {
        topic: Topic,
        owners: Vec<&'static str>,
    },
    /// A `supersedes` entry names a source that is not registered.
    UnresolvedSupersede {
        id: &'static str,
        target: &'static str,
    },
    /// Two sources supersede each other.
    SupersedeCycle {
        topic: Topic,
        owners: Vec<&'static str>,
    },
    /// An ordering anchor names a source that is not registered.
    MissingAnchor {
        id: &'static str,
        target: &'static str,
    },
    /// An ordering anchor crosses a position class or a stability band.
    CrossBandAnchor {
        id: &'static str,
        target: &'static str,
    },
    /// Ordering anchors form a cycle.
    Cycle(Vec<&'static str>),
    /// A frame block declares an input too volatile for the head of the prefix.
    FrameVolatility {
        id: &'static str,
        input: &'static str,
    },
    /// A source declared a zero budget.
    EmptyBudget { id: &'static str },
    /// A directive source declared `Overflow::Drop`, which is a silent loss.
    SilentDirectiveDrop { id: &'static str },
    /// A builder asked to disable a source that was never declared.
    UnknownSource(String),
}

impl std::fmt::Display for ContextPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "duplicate context source id: {id}"),
            Self::DuplicateTopic { topic, owners } => write!(
                f,
                "two context sources own topic `{}`: {owners:?}. Declare `supersedes` on one, \
                 or narrow the other's inputs",
                topic.label()
            ),
            Self::UnresolvedSupersede { id, target } => {
                write!(f, "context source `{id}` supersedes unknown `{target}`")
            }
            Self::SupersedeCycle { topic, owners } => write!(
                f,
                "context sources {owners:?} supersede each other for topic `{}`",
                topic.label()
            ),
            Self::MissingAnchor { id, target } => {
                write!(f, "context source `{id}` anchors to missing `{target}`")
            }
            Self::CrossBandAnchor { id, target } => write!(
                f,
                "context source `{id}` anchors to `{target}` across a position class or \
                 stability band; order inside a band only"
            ),
            Self::Cycle(ids) => write!(f, "context ordering cycle: {ids:?}"),
            Self::FrameVolatility { id, input } => write!(
                f,
                "frame block `{id}` declares `{input}`, which is more volatile than the head \
                 of the prefix may be"
            ),
            Self::EmptyBudget { id } => write!(f, "context source `{id}` declared a zero budget"),
            Self::SilentDirectiveDrop { id } => write!(
                f,
                "directive source `{id}` declared `Overflow::Drop`; a directive must disclose \
                 its loss with `Elide` or `Index`"
            ),
            Self::UnknownSource(id) => write!(f, "unknown context source: {id}"),
        }
    }
}

impl std::error::Error for ContextPolicyError {}

/// The ordering group of one source: the frame leads as one group, then one
/// group per stability band. Anchors never cross a group.
fn band(meta: SourceMeta) -> Band {
    match meta.topic.position() {
        Position::Frame => Band::Frame,
        Position::Body => Band::Body(meta.stability),
    }
}

/// An ordering group. Anchors never cross one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Band {
    Frame,
    Body(Stability),
}

/// A compiled context policy.
///
/// Owns its sources and an emission order over them, so reordering never copies a
/// source and no `Arc` or `clone` seam exists.
pub struct Policy {
    sources: Vec<Box<dyn ContextSource + Send + Sync>>,
    meta: Vec<SourceMeta>,
    /// Indices into `meta`/`sources`, in emission order.
    order: Vec<usize>,
    generation: u64,
}

impl std::fmt::Debug for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Policy")
            .field("generation", &self.generation)
            .field(
                "order",
                &self
                    .order
                    .iter()
                    .map(|&i| self.meta[i].id)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Policy {
    /// A policy from an already-ordered declaration list.
    ///
    /// Reserved for exactly two callers: the product's compiled-in default
    /// composition, which is ordered by declaration and whose validity is pinned
    /// by `the_default_declaration_order_compiles_unchanged`; and
    /// [`AgentBuilder`](crate::AgentBuilder), which validates every edit through
    /// [`Self::compile`] and hands back an already-ordered set from
    /// [`Self::into_sources`].
    ///
    /// It cannot validate: that is the point. `build()` must not be able to fail,
    /// and a composition that could fail was rejected at the edit that introduced
    /// it.
    pub(crate) fn from_declared(sources: Vec<Box<dyn ContextSource + Send + Sync>>) -> Self {
        let meta: Vec<SourceMeta> = sources
            .iter()
            .map(|source| SourceMeta::derive(source.id(), source.topic(), source.inputs()))
            .collect();
        let order = (0..sources.len()).collect();
        Self {
            sources,
            meta,
            order,
            generation: 1,
        }
    }

    /// Compile a source list into an emission order, or report why it is invalid.
    pub fn compile(
        sources: Vec<Box<dyn ContextSource + Send + Sync>>,
    ) -> Result<Self, ContextPolicyError> {
        let mut seen: HashSet<&'static str> = HashSet::new();
        for source in &sources {
            if !seen.insert(source.id()) {
                return Err(ContextPolicyError::DuplicateId(source.id()));
            }
        }

        for source in &sources {
            if source.topic().position() != Position::Frame {
                continue;
            }
            if let Some(input) = source
                .inputs()
                .iter()
                .find(|key| key.stability() > Stability::Route)
            {
                return Err(ContextPolicyError::FrameVolatility {
                    id: source.id(),
                    input: input.label(),
                });
            }
        }

        for source in &sources {
            let budget = source.budget();
            if budget.tokens == 0 {
                return Err(ContextPolicyError::EmptyBudget { id: source.id() });
            }
            if matches!(budget.overflow, Overflow::Drop) && source.topic().rank() == Rank::Directive
            {
                return Err(ContextPolicyError::SilentDirectiveDrop { id: source.id() });
            }
        }

        let shadowed = resolve_supersedes(&sources)?;
        let live: Vec<usize> = (0..sources.len())
            .filter(|&index| !shadowed.contains(sources[index].id()))
            .collect();

        assert_single_owner(&sources, &live)?;

        let meta: Vec<SourceMeta> = sources
            .iter()
            .map(|source| SourceMeta::derive(source.id(), source.topic(), source.inputs()))
            .collect();

        let order = emission_order(&sources, &meta, &live)?;

        Ok(Self {
            sources,
            meta,
            order,
            generation: 1,
        })
    }

    /// A policy generation counter. Bumped whenever the policy is mutated, so the
    /// render memo cannot go stale the way a forgotten invalidation can.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn live(&self) -> impl Iterator<Item = usize> + '_ {
        self.order.iter().copied()
    }

    /// Every declared source id, in emission order.
    pub fn source_ids(&self) -> Vec<&'static str> {
        self.live().map(|index| self.meta[index].id).collect()
    }

    /// The declared topic of one source, for tests and inspection.
    pub fn topic_of(&self, id: &str) -> Option<Topic> {
        self.meta
            .iter()
            .find(|meta| meta.id == id)
            .map(|meta| meta.topic)
    }

    /// The derived stability of one source, for tests and inspection.
    pub fn stability_of(&self, id: &str) -> Option<Stability> {
        self.meta
            .iter()
            .find(|meta| meta.id == id)
            .map(|meta| meta.stability)
    }

    /// Remove a source by id. Returns whether anything changed.
    ///
    /// Disabling is a policy mutation, so it bumps the generation; it cannot
    /// introduce a layout error because removal preserves every remaining
    /// anchor's group.
    pub fn disable(&mut self, id: &str) -> bool {
        let Some(position) = self
            .order
            .iter()
            .position(|&index| self.meta[index].id == id)
        else {
            return false;
        };
        self.order.remove(position);
        self.generation += 1;
        true
    }

    /// Take the declared sources back out of a compiled policy, in emission
    /// order.
    ///
    /// The builder uses this to keep one source list across edits while still
    /// validating each edit as a whole: compiling early is what turns a duplicate
    /// topic into a registration error instead of a construction panic.
    pub fn into_sources(self) -> Vec<Box<dyn ContextSource + Send + Sync>> {
        let Self {
            mut sources,
            meta,
            order,
            ..
        } = self;
        let mut slots: Vec<Option<Box<dyn ContextSource + Send + Sync>>> =
            sources.drain(..).map(Some).collect();
        let _ = meta;
        order
            .into_iter()
            .filter_map(|index| slots.get_mut(index).and_then(Option::take))
            .collect()
    }

    /// Measure one request against the default model window.
    pub fn plan(&self, inputs: &ContextInputs) -> ContextPlan {
        let ceiling = protocol::CompactionPolicy::default()
            .resolve(0)
            .instructions_ceiling_tokens;
        self.plan_with_ceiling(inputs, ceiling)
    }

    /// Measure one request against an explicit instruction-region ceiling.
    ///
    /// The ceiling is derived from the active model window by the caller
    /// (`protocol::ContextBudget::instructions_ceiling_tokens`): a fixed prompt
    /// must not consume the window before the conversation starts.
    pub fn plan_with_ceiling(&self, inputs: &ContextInputs, ceiling: usize) -> ContextPlan {
        let mut rows: Vec<Row> = Vec::with_capacity(self.order.len());

        for &index in &self.order {
            let meta = self.meta[index];
            let source = &self.sources[index];
            if !source.is_active(inputs) {
                rows.push(Row::Inactive {
                    meta,
                    reason: "inactive for this request",
                });
                continue;
            }
            let Some(mut block) = source.render(inputs) else {
                rows.push(Row::Inactive {
                    meta,
                    reason: "active but rendered no text",
                });
                continue;
            };
            block.id = meta.id.to_owned();
            if protocol::render_blocks(std::slice::from_ref(&block))
                .trim()
                .is_empty()
            {
                // An empty body is not emitted by the formatter, so reporting it
                // as active would claim a contribution the request never got.
                rows.push(Row::Inactive {
                    meta,
                    reason: "active but rendered no text",
                });
                continue;
            }

            let budget = source.budget();
            let mut elision = None;
            let mut tokens = measure(&block);
            if tokens > budget.tokens {
                match overflow(&block, &budget, meta.id) {
                    Some(applied) => {
                        block = applied.block;
                        tokens = applied.tokens;
                        elision = Some(applied.elision);
                        rows.push(Row::Active {
                            meta,
                            block: Box::new(block),
                            tokens,
                            elision,
                            notice: Some(applied.notice),
                            evicted: false,
                        });
                    }
                    None => rows.push(Row::Inactive {
                        meta,
                        reason: "over budget and dropped by its overflow policy",
                    }),
                }
                continue;
            }
            rows.push(Row::Active {
                meta,
                block: Box::new(block),
                tokens,
                elision,
                notice: None,
                evicted: false,
            });
        }

        let evicted = evict_for_ceiling(&mut rows, ceiling);

        let mut plan = ContextPlan::default();
        for row in &rows {
            let meta = row.meta();
            if row.emitted() {
                let Row::Active {
                    tokens, elision, ..
                } = row
                else {
                    unreachable!("only an active row is emitted")
                };
                plan.blocks.push(PlannedBlock {
                    id: meta.id.to_owned(),
                    topic: meta.topic,
                    rank: meta.rank,
                    position: meta.topic.position(),
                    stability: meta.stability,
                    tokens: *tokens,
                    elision: elision.clone(),
                    active: true,
                    inactive_reason: None,
                });
                continue;
            }
            let reason = match row {
                Row::Inactive { reason, .. } => *reason,
                Row::Active { .. } => "evicted to satisfy the instruction-region ceiling",
            };
            plan.blocks.push(PlannedBlock::inactive(
                meta.id,
                meta.topic,
                meta.stability,
                reason,
            ));
        }

        for row in &rows {
            if let Row::Active {
                notice: Some(notice),
                ..
            } = row
            {
                plan.notices.push(notice.clone());
            }
            if !row.emitted() {
                continue;
            }
            if let Row::Active {
                meta,
                block,
                tokens,
                ..
            } = row
            {
                if meta.stability.is_transient() {
                    plan.transient.push(block.as_ref().clone());
                    plan.transient_tokens += tokens;
                } else {
                    plan.fixed.push(block.as_ref().clone());
                    plan.fixed_tokens += tokens;
                }
            }
        }

        if !evicted.is_empty() {
            plan.notices.push(ContextNotice {
                source_id: "context.ceiling".to_owned(),
                kind: NoticeKind::CeilingEvicted,
                message: format!(
                    "The instruction region exceeded its {ceiling} token ceiling; these blocks \
                     were omitted: {evicted:?}. Their content is still reachable through the \
                     file and search tools."
                ),
            });
        }

        plan
    }
}

/// One rendered or skipped source, before the plan is assembled.
enum Row {
    Active {
        meta: SourceMeta,
        /// Boxed: an inactive row is a handful of bytes and dominates the vec.
        block: Box<Block>,
        tokens: usize,
        elision: Option<Elision>,
        notice: Option<ContextNotice>,
        evicted: bool,
    },
    Inactive {
        meta: SourceMeta,
        reason: &'static str,
    },
}

impl Row {
    fn meta(&self) -> SourceMeta {
        match self {
            Self::Active { meta, .. } | Self::Inactive { meta, .. } => *meta,
        }
    }

    /// Whether this row contributes text to the request.
    fn emitted(&self) -> bool {
        matches!(self, Self::Active { evicted: false, .. })
    }

    fn tokens(&self) -> usize {
        match self {
            Self::Active {
                tokens,
                evicted: false,
                ..
            } => *tokens,
            _ => 0,
        }
    }
}

/// Evict the most volatile body blocks until the two regions fit the ceiling.
///
/// The frame is never evicted: a prompt without its identity line and
/// arbitration contract is worse than a prompt without a topology summary.
fn evict_for_ceiling(rows: &mut [Row], ceiling: usize) -> Vec<&'static str> {
    let total = |rows: &[Row]| rows.iter().map(Row::tokens).sum::<usize>();
    let mut evicted = Vec::new();
    while total(rows) > ceiling {
        let victim = rows
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, row)| match row {
                Row::Active {
                    evicted: false,
                    meta,
                    ..
                } if meta.topic.position() == Position::Body => Some((index, meta.id)),
                _ => None,
            });
        let Some((index, id)) = victim else { break };
        if let Row::Active {
            evicted: evicted_row,
            ..
        } = &mut rows[index]
        {
            *evicted_row = true;
        }
        evicted.push(id);
    }
    evicted
}

/// Resolve supersede declarations into the set of shadowed ids.
fn resolve_supersedes(
    sources: &[Box<dyn ContextSource + Send + Sync>],
) -> Result<HashSet<&'static str>, ContextPolicyError> {
    let ids: HashSet<&'static str> = sources.iter().map(|source| source.id()).collect();
    let mut shadowed: HashSet<&'static str> = HashSet::new();
    let mut declared: HashSet<(&'static str, &'static str)> = HashSet::new();

    for source in sources {
        for target in source.supersedes() {
            if !ids.contains(target) {
                return Err(ContextPolicyError::UnresolvedSupersede {
                    id: source.id(),
                    target,
                });
            }
            if declared.contains(&(target, source.id())) {
                return Err(ContextPolicyError::SupersedeCycle {
                    topic: source.topic(),
                    owners: vec![source.id(), target],
                });
            }
            declared.insert((source.id(), target));
            shadowed.insert(target);
        }
    }
    Ok(shadowed)
}

/// Prove one owner per topic among the live sources.
fn assert_single_owner(
    sources: &[Box<dyn ContextSource + Send + Sync>],
    live: &[usize],
) -> Result<(), ContextPolicyError> {
    let mut owners: HashMap<Topic, Vec<&'static str>> = HashMap::new();
    for &index in live {
        owners
            .entry(sources[index].topic())
            .or_default()
            .push(sources[index].id());
    }
    let mut collisions: Vec<(Topic, Vec<&'static str>)> = owners
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .collect();
    collisions.sort_by_key(|(topic, _)| *topic);
    if let Some((topic, mut owners)) = collisions.into_iter().next() {
        owners.sort_unstable();
        return Err(ContextPolicyError::DuplicateTopic { topic, owners });
    }
    Ok(())
}

/// Resolve anchors inside each band and lay out the emission order.
fn emission_order(
    sources: &[Box<dyn ContextSource + Send + Sync>],
    meta: &[SourceMeta],
    live: &[usize],
) -> Result<Vec<usize>, ContextPolicyError> {
    let by_id: HashMap<&'static str, usize> =
        live.iter().map(|&index| (meta[index].id, index)).collect();

    // Anchor validity and grouping.
    let mut groups: HashMap<Band, Vec<usize>> = HashMap::new();
    for &index in live {
        let source = &sources[index];
        match source.order() {
            SourceOrder::Before(target) | SourceOrder::After(target) => {
                let Some(&target_index) = by_id.get(target) else {
                    return Err(ContextPolicyError::MissingAnchor {
                        id: meta[index].id,
                        target,
                    });
                };
                if band(meta[target_index]) != band(meta[index]) {
                    return Err(ContextPolicyError::CrossBandAnchor {
                        id: meta[index].id,
                        target,
                    });
                }
            }
            SourceOrder::Head | SourceOrder::Tail => {}
        }
        groups.entry(band(meta[index])).or_default().push(index);
    }

    let mut bands: Vec<Band> = groups.keys().copied().collect();
    bands.sort_unstable();

    let mut order = Vec::with_capacity(live.len());
    for band in bands {
        let Some(members) = groups.remove(&band) else {
            continue;
        };
        let mut ordered = order_band(sources, meta, &by_id, members)?;
        if band == Band::Frame {
            // The frame is not a stability band: identity then the arbitration
            // contract, by topic, so a declaration order cannot put the contract
            // before the identity it qualifies.
            ordered.sort_by_key(|&index| meta[index].topic);
        }
        order.extend(ordered);
    }
    Ok(order)
}

/// Kahn's algorithm inside one band, with `Head` first, declaration order in the
/// middle, and `Tail` last.
fn order_band(
    sources: &[Box<dyn ContextSource + Send + Sync>],
    meta: &[SourceMeta],
    by_id: &HashMap<&'static str, usize>,
    members: Vec<usize>,
) -> Result<Vec<usize>, ContextPolicyError> {
    let position: HashMap<usize, usize> = members
        .iter()
        .enumerate()
        .map(|(slot, &index)| (index, slot))
        .collect();
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); members.len()];
    let mut indegree: Vec<usize> = vec![0; members.len()];

    for &index in &members {
        let from = position[&index];
        match sources[index].order() {
            SourceOrder::Before(target) => {
                let to = position[&by_id[target]];
                if from == to {
                    return Err(ContextPolicyError::Cycle(vec![meta[index].id]));
                }
                edges[from].push(to);
                indegree[to] += 1;
            }
            SourceOrder::After(target) => {
                let to = position[&by_id[target]];
                if from == to {
                    return Err(ContextPolicyError::Cycle(vec![meta[index].id]));
                }
                edges[to].push(from);
                indegree[from] += 1;
            }
            SourceOrder::Head | SourceOrder::Tail => {}
        }
    }

    let base_rank = |slot: usize| match sources[members[slot]].order() {
        SourceOrder::Head => 0u8,
        SourceOrder::Before(_) | SourceOrder::After(_) => 1,
        SourceOrder::Tail => 2,
    };

    let mut ready: Vec<usize> = (0..members.len()).filter(|&s| indegree[s] == 0).collect();
    let mut ordered = Vec::with_capacity(members.len());
    while !ready.is_empty() {
        ready.sort_by_key(|&slot| (base_rank(slot), slot));
        let slot = ready.remove(0);
        ordered.push(members[slot]);
        for &next in &edges[slot] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                ready.push(next);
            }
        }
    }

    if ordered.len() != members.len() {
        let cycle: Vec<&'static str> = members
            .iter()
            .filter(|index| !ordered.contains(index))
            .map(|index| meta[*index].id)
            .collect();
        return Err(ContextPolicyError::Cycle(cycle));
    }
    Ok(ordered)
}

/// One applied overflow decision.
struct Applied {
    block: Block,
    tokens: usize,
    elision: Elision,
    notice: ContextNotice,
}

fn overflow(block: &Block, budget: &protocol::Budget, id: &'static str) -> Option<Applied> {
    let full = measure(block);
    match &budget.overflow {
        Overflow::Drop => None,
        Overflow::Index { resume } => {
            let body = format!("[{id}: {full} tokens omitted; {}]", resume.instruction());
            let indexed = Block {
                body,
                ..block.clone()
            };
            let tokens = measure(&indexed);
            let message = indexed.body.clone();
            Some(Applied {
                block: indexed,
                tokens,
                elision: Elision {
                    full_tokens: full,
                    kept_tokens: tokens,
                    resume: resume.clone(),
                },
                notice: ContextNotice {
                    source_id: id.to_owned(),
                    kind: NoticeKind::Indexed,
                    message,
                },
            })
        }
        Overflow::Elide { resume } => {
            // Reserve room for the notice so the emitted block stays inside the
            // budget instead of overrunning it.
            const NOTICE_RESERVE_TOKENS: usize = 64;
            let keep = budget.tokens.saturating_sub(NOTICE_RESERVE_TOKENS).max(1);
            let (truncated, _) = tool::truncate_to_tokens(&block.body, keep);
            let mut body = truncate_on_line_boundary(truncated);
            let dropped = full.saturating_sub(measure(&Block {
                body: body.clone(),
                ..block.clone()
            }));
            let message = format!("[{id}: {dropped} tokens elided; {}]", resume.instruction());
            body.push_str("\n\n");
            body.push_str(&message);
            let elided = Block {
                body,
                ..block.clone()
            };
            let tokens = measure(&elided);
            Some(Applied {
                block: elided,
                tokens,
                elision: Elision {
                    full_tokens: full,
                    kept_tokens: tokens,
                    resume: resume.clone(),
                },
                notice: ContextNotice {
                    source_id: id.to_owned(),
                    kind: NoticeKind::Elided,
                    message,
                },
            })
        }
    }
}

/// Cut to the last newline and close an odd code fence, so an elided Markdown
/// body is never structurally broken.
fn truncate_on_line_boundary(text: &str) -> String {
    let mut out = match text.rfind('\n') {
        Some(index) if index > 0 => text[..index].to_owned(),
        _ => text.to_owned(),
    };
    if out.matches("```").count() % 2 == 1 {
        out.push_str("\n```");
    }
    out
}

/// Rendered cost of one block, including its title line and envelope, so a
/// budget bounds what the provider actually receives.
fn measure(block: &Block) -> usize {
    tool::count_tokens(&protocol::render_blocks(std::slice::from_ref(block)))
}
