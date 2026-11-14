//! Agent-owned declarative instruction composition.
//!
//! Sections produce [`Block`] fragments for one of two request regions:
//! `fixed` (route-stable) or `transient` (request-local). Ordering is declared
//! with semantic anchors (`Head`, `Before`, `After`, `Tail`); the registry
//! compiles them into a stable topological order and rejects missing anchors or
//! cycles.
//!
//! Sections carry no headings, leading newlines, or cache tiers. Formatting is
//! owned by [`nuo_protocol::render_blocks`].
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use nuo_protocol::Block;

/// Read-only view of the live turn state a section may draw on to render.
///
/// Plain data (no `&Agent`) keeps a section's `render` signature free of lifetime
/// parameters and makes the whole context content-addressable for memoization.
#[derive(Debug, Clone, Default, Hash, PartialEq, Eq)]
pub struct InstructionContext {
    /// The composed identity line — name/mission, a persona override, or a
    /// `/role` directive. Empty means no identity block.
    pub identity_preamble: String,
    /// Names of the tools admitted this turn.
    pub tool_names: Vec<String>,
    /// Whether a subagent-dispatch tool is admitted this turn.
    pub has_subagent_tool: bool,
    /// User-authored instructions matching this turn's role and route.
    pub user_instructions: Vec<nuo_protocol::ScopedInstruction>,
    /// Provider/protocol-specific prompt guidance from the active provider.
    pub provider_guidance: &'static str,
    /// Content-attested project instructions (for example AGENTS.md).
    pub project_rules: String,
    /// Canonicalized additional workspace roots admitted alongside the primary.
    pub additional_workspace_roots: Vec<String>,
    /// The primary workspace root path, if any.
    pub workspace_root: Option<String>,
    /// Deterministic workspace architecture / crate topology summary.
    pub workspace_topology: Option<String>,
    /// Whether the active provider channel supports parallel tool calling.
    pub supports_parallel_tool_calls: bool,
    /// Repository-authored instructions that match this turn's role and route.
    pub project_instructions: Vec<nuo_protocol::ScopedInstruction>,
}

impl InstructionContext {
    /// An all-empty context for registry-mechanics tests.
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Relative placement of a section within its region.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InstructionOrder {
    /// Placed at the very front.
    Head,
    /// Placed before the specified section id.
    Before(&'static str),
    /// Placed after the specified section id.
    After(&'static str),
    /// Placed at the end (default).
    #[default]
    Tail,
}

/// A self-contained, declaratively registered instruction section.
pub trait InstructionSection: Send + Sync {
    /// Stable unique identifier, used for overrides, disables, dependencies, and tracing.
    /// Convention: `<origin>.<name>`, e.g. `"project.rules"`.
    fn id(&self) -> &'static str;

    /// Semantic ordering relation. Defaults to `Tail`.
    fn order(&self) -> InstructionOrder {
        InstructionOrder::Tail
    }

    /// Whether this section belongs to the request-local `transient` region.
    fn is_transient(&self) -> bool {
        false
    }

    /// Whether this section applies in the current context. Default `true`.
    fn is_active(&self, _ctx: &InstructionContext) -> bool {
        true
    }

    /// Render one block. `None` means "active but produces no text this turn";
    /// the registry skips it without leaving a blank gap.
    fn render(&self, ctx: &InstructionContext) -> Option<Block>;
}

/// The two rendered regions produced by one registry pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstructionRegions {
    pub fixed: Vec<Block>,
    pub transient: Vec<Block>,
}

/// A registered section plus runtime overrides.
struct Entry {
    section: Box<dyn InstructionSection + Send + Sync>,
    order_override: Option<InstructionOrder>,
    disabled: bool,
    /// Registration index, used as the deterministic tie-break.
    ordinal: usize,
}

impl Entry {
    fn id(&self) -> &'static str {
        self.section.id()
    }

    fn effective_order(&self) -> InstructionOrder {
        self.order_override
            .clone()
            .unwrap_or_else(|| self.section.order())
    }
}

/// Declarative instruction policy for one agent.
#[derive(Default)]
pub struct InstructionRegistry {
    entries: Vec<Entry>,
    /// Content-addressed memo of the last render: (context_hash, regions).
    render_memo: std::sync::Mutex<Option<(u64, InstructionRegions)>>,
}

/// Configuration or layout error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstructionRegistryError {
    /// A section with the same stable id is already registered.
    DuplicateId(&'static str),
    /// An override refers to an id that is not registered.
    UnknownId(String),
    /// An ordering anchor names a section that is not registered.
    MissingAnchor {
        id: &'static str,
        target: &'static str,
    },
    /// Ordering anchors form a cycle.
    Cycle(Vec<&'static str>),
}

impl std::fmt::Display for InstructionRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "duplicate instruction section id: {id}"),
            Self::UnknownId(id) => write!(f, "unknown instruction section id: {id}"),
            Self::MissingAnchor { id, target } => {
                write!(
                    f,
                    "instruction section '{id}' anchors to missing '{target}'"
                )
            }
            Self::Cycle(ids) => write!(f, "instruction ordering cycle: {ids:?}"),
        }
    }
}

impl std::error::Error for InstructionRegistryError {}

impl InstructionRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a section. A duplicate id is a programming error in the compiled
    /// section table, so it is asserted rather than silently dropping a section.
    pub fn register<S: InstructionSection + 'static>(&mut self, section: S) {
        let id = section.id();
        assert!(
            self.try_register(section).is_ok(),
            "duplicate instruction section id: {id}"
        );
    }

    /// Register a section without panicking on an id collision.
    pub fn try_register<S: InstructionSection + 'static>(
        &mut self,
        section: S,
    ) -> Result<(), InstructionRegistryError> {
        let id = section.id();
        if self.entries.iter().any(|e| e.id() == id) {
            return Err(InstructionRegistryError::DuplicateId(id));
        }
        let ordinal = self.entries.len();
        self.entries.push(Entry {
            section: Box::new(section),
            order_override: None,
            disabled: false,
            ordinal,
        });
        self.invalidate_memo();
        Ok(())
    }

    fn invalidate_memo(&self) {
        if let Ok(mut memo) = self.render_memo.lock() {
            *memo = None;
        }
    }

    /// Override a section's ordering relative to other sections.
    pub fn set_order(
        &mut self,
        id: &str,
        order: InstructionOrder,
    ) -> Result<(), InstructionRegistryError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id() == id)
            .ok_or_else(|| InstructionRegistryError::UnknownId(id.to_owned()))?;
        entry.order_override = Some(order);
        self.invalidate_memo();
        Ok(())
    }

    /// Disable a section by id (it is skipped as if inactive).
    pub fn disable(&mut self, id: &str) -> Result<(), InstructionRegistryError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id() == id)
            .ok_or_else(|| InstructionRegistryError::UnknownId(id.to_owned()))?;
        entry.disabled = true;
        self.invalidate_memo();
        Ok(())
    }

    /// Build the memoized `fixed` / `transient` regions for this context.
    ///
    /// Missing anchors and cycles are hard errors. A missing anchor in an
    /// *inactive* section is not an error: its relation is simply ignored.
    pub fn build_regions(
        &self,
        ctx: &InstructionContext,
    ) -> Result<InstructionRegions, InstructionRegistryError> {
        let mut hasher = DefaultHasher::new();
        ctx.hash(&mut hasher);
        let hash = hasher.finish();

        if let Ok(memo) = self.render_memo.lock()
            && let Some((cached_hash, regions)) = memo.as_ref()
            && *cached_hash == hash
        {
            return Ok(regions.clone());
        }

        let regions = self.render_regions(ctx)?;
        if let Ok(mut memo) = self.render_memo.lock() {
            *memo = Some((hash, regions.clone()));
        }
        Ok(regions)
    }

    fn render_regions(
        &self,
        ctx: &InstructionContext,
    ) -> Result<InstructionRegions, InstructionRegistryError> {
        let registered: HashMap<&'static str, usize> = self
            .entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| (entry.id(), idx))
            .collect();

        let active: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|entry| !entry.disabled && entry.section.is_active(ctx))
            .collect();

        for entry in &active {
            match entry.effective_order() {
                InstructionOrder::Before(target) | InstructionOrder::After(target) => {
                    if !registered.contains_key(target) {
                        return Err(InstructionRegistryError::MissingAnchor {
                            id: entry.id(),
                            target,
                        });
                    }
                }
                InstructionOrder::Head | InstructionOrder::Tail => {}
            }
        }

        let ordered = topological_order(&active, &registered)?;
        let mut regions = InstructionRegions::default();
        for entry in ordered {
            let Some(mut block) = entry.section.render(ctx) else {
                continue;
            };
            if block.body.trim().is_empty() {
                continue;
            }
            block.id = entry.id().to_owned();
            if entry.section.is_transient() {
                regions.transient.push(block);
            } else {
                regions.fixed.push(block);
            }
        }
        Ok(regions)
    }
}

/// Stable topological sort of active sections.
///
/// Base order is `Head` first, then registration order, then `Tail`. Explicit
/// `Before`/`After` edges refine it. A target that is registered but inactive
/// contributes no edge; a target that is not registered is a hard error handled
/// by the caller.
fn topological_order<'a>(
    active: &[&'a Entry],
    registered: &HashMap<&'static str, usize>,
) -> Result<Vec<&'a Entry>, InstructionRegistryError> {
    let mut index_of: HashMap<&'static str, usize> = HashMap::new();
    for (idx, entry) in active.iter().enumerate() {
        index_of.insert(entry.id(), idx);
    }

    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); active.len()];
    let mut indegree: Vec<usize> = vec![0; active.len()];

    for (idx, entry) in active.iter().enumerate() {
        match entry.effective_order() {
            InstructionOrder::Before(target) | InstructionOrder::After(target) => {
                // A registered-but-inactive target contributes no edge.
                if !registered.contains_key(target) {
                    return Err(InstructionRegistryError::MissingAnchor {
                        id: entry.id(),
                        target,
                    });
                }
                let Some(&target_idx) = index_of.get(target) else {
                    continue;
                };
                let (from, to) = match entry.effective_order() {
                    InstructionOrder::Before(_) => (idx, target_idx),
                    InstructionOrder::After(_) => (target_idx, idx),
                    _ => unreachable!(),
                };
                edges[from].push(to);
                indegree[to] += 1;
            }
            InstructionOrder::Head | InstructionOrder::Tail => {}
        }
    }

    // Base priority: Head (0), middle (1), Tail (2), then registration order.
    let base_rank = |entry: &Entry| match entry.effective_order() {
        InstructionOrder::Head => 0u8,
        InstructionOrder::Tail => 2,
        InstructionOrder::Before(_) | InstructionOrder::After(_) => 1,
    };

    let mut ready: Vec<usize> = (0..active.len()).filter(|&i| indegree[i] == 0).collect();
    ready.sort_by_key(|&i| (base_rank(active[i]), active[i].ordinal));

    let mut ordered = Vec::with_capacity(active.len());
    while let Some(idx) = ready.first().copied() {
        ready.remove(0);
        ordered.push(active[idx]);
        for &next in &edges[idx] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                ready.push(next);
            }
        }
        ready.sort_by_key(|&i| (base_rank(active[i]), active[i].ordinal));
    }

    if ordered.len() != active.len() {
        let cycle: Vec<&'static str> = active
            .iter()
            .filter(|entry| !ordered.iter().any(|seen| seen.id() == entry.id()))
            .map(|entry| entry.id())
            .collect();
        return Err(InstructionRegistryError::Cycle(cycle));
    }

    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_protocol::BlockRender;

    struct TestSection {
        id: &'static str,
        order: InstructionOrder,
        transient: bool,
        active: bool,
        text: Option<&'static str>,
    }

    impl TestSection {
        fn new(id: &'static str, text: &'static str) -> Self {
            Self {
                id,
                order: InstructionOrder::Tail,
                transient: false,
                active: true,
                text: Some(text),
            }
        }
    }

    impl InstructionSection for TestSection {
        fn id(&self) -> &'static str {
            self.id
        }
        fn order(&self) -> InstructionOrder {
            self.order.clone()
        }
        fn is_transient(&self) -> bool {
            self.transient
        }
        fn is_active(&self, _ctx: &InstructionContext) -> bool {
            self.active
        }
        fn render(&self, _ctx: &InstructionContext) -> Option<Block> {
            self.text
                .map(|text| Block::new("ignored", text).with_render(BlockRender::Plain))
        }
    }

    #[test]
    fn sections_render_in_registration_order_by_default() {
        let mut reg = InstructionRegistry::new();
        reg.register(TestSection::new("a", "A"));
        reg.register(TestSection::new("b", "B"));
        let regions = reg.build_regions(&InstructionContext::empty()).unwrap();
        assert_eq!(regions.fixed.len(), 2);
        assert_eq!(regions.fixed[0].id, "a");
        assert_eq!(regions.fixed[1].id, "b");
    }

    #[test]
    fn head_before_after_and_tail_are_honored() {
        let mut reg = InstructionRegistry::new();
        let mut first = TestSection::new("first", "1");
        first.order = InstructionOrder::Tail;
        let mut head = TestSection::new("head", "H");
        head.order = InstructionOrder::Head;
        let mut before = TestSection::new("before", "B");
        before.order = InstructionOrder::Before("first");
        let mut after = TestSection::new("after", "A");
        after.order = InstructionOrder::After("first");
        reg.register(first);
        reg.register(head);
        reg.register(before);
        reg.register(after);

        let regions = reg.build_regions(&InstructionContext::empty()).unwrap();
        let ids: Vec<&str> = regions.fixed.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["head", "before", "first", "after"]);
    }

    #[test]
    fn missing_anchor_is_an_error() {
        let mut reg = InstructionRegistry::new();
        let mut section = TestSection::new("a", "A");
        section.order = InstructionOrder::After("missing");
        reg.register(section);
        let error = reg.build_regions(&InstructionContext::empty()).unwrap_err();
        assert!(matches!(
            error,
            InstructionRegistryError::MissingAnchor {
                id: "a",
                target: "missing"
            }
        ));
    }

    #[test]
    fn cycle_is_an_error() {
        let mut reg = InstructionRegistry::new();
        let mut a = TestSection::new("a", "A");
        a.order = InstructionOrder::After("b");
        let mut b = TestSection::new("b", "B");
        b.order = InstructionOrder::After("a");
        reg.register(a);
        reg.register(b);
        let error = reg.build_regions(&InstructionContext::empty()).unwrap_err();
        assert!(matches!(error, InstructionRegistryError::Cycle(_)));
    }

    #[test]
    fn transient_sections_partition_into_the_transient_region() {
        let mut reg = InstructionRegistry::new();
        reg.register(TestSection::new("fixed", "F"));
        let mut transient = TestSection::new("transient", "T");
        transient.transient = true;
        reg.register(transient);
        let regions = reg.build_regions(&InstructionContext::empty()).unwrap();
        assert_eq!(regions.fixed.len(), 1);
        assert_eq!(regions.transient.len(), 1);
        assert_eq!(regions.transient[0].id, "transient");
    }

    #[test]
    fn inactive_sections_are_skipped_and_their_anchors_are_ignored() {
        let mut reg = InstructionRegistry::new();
        let mut inactive = TestSection::new("inactive", "I");
        inactive.active = false;
        let mut dependent = TestSection::new("dependent", "D");
        dependent.order = InstructionOrder::After("inactive");
        reg.register(inactive);
        reg.register(dependent);
        let regions = reg.build_regions(&InstructionContext::empty()).unwrap();
        let ids: Vec<&str> = regions.fixed.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["dependent"]);
    }

    #[test]
    fn disable_and_memo_invalidation() {
        let mut reg = InstructionRegistry::new();
        reg.register(TestSection::new("a", "A"));
        let first = reg.build_regions(&InstructionContext::empty()).unwrap();
        assert_eq!(first.fixed.len(), 1);
        reg.disable("a").unwrap();
        let second = reg.build_regions(&InstructionContext::empty()).unwrap();
        assert!(second.fixed.is_empty());
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let mut reg = InstructionRegistry::new();
        reg.register(TestSection::new("a", "A"));
        let error = reg.try_register(TestSection::new("a", "B")).unwrap_err();
        assert!(matches!(error, InstructionRegistryError::DuplicateId("a")));
    }

    #[test]
    fn registry_overwrites_the_block_id_with_the_section_id() {
        let mut reg = InstructionRegistry::new();
        reg.register(TestSection::new("canonical", "body"));
        let regions = reg.build_regions(&InstructionContext::empty()).unwrap();
        assert_eq!(regions.fixed[0].id, "canonical");
    }
}
