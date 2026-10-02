//! Agent-owned declarative instruction composition and registry (ADR-0056 / ADR-0160).
//!
//! A system prompt has a lifecycle unlike conversational turn messages:
//! multiple declarative policy sections compose into a structured, cache-tiered
//! [`InstructionBundle`] that is rebuilt before every provider request.
//!
//! Sections are categorized by [`InstructionTier`]:
//! - Base (immutable persona, safety ethos, host environment)
//! - Session (workspace rules, multi-root access, static tool categories)
//! - Task (subagent mission, subagent task framing)
//! - Ephemeral (turn-dynamic nudges)
//!
//! Ordering within each tier is governed by semantic [`InstructionOrder`] relations
//! (Head, After, Before, Tail) eliminating fragile magic rank numbers.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use nuo_contracts::{
    InjectionKind, InjectionOrigin, InstructionBundle, InstructionSlice, InstructionTier, Message,
    Role,
};

/// Read-only view of the live turn state an instruction section may draw on to render.
///
/// Plain data (no `&Agent`) keeps a section's `render` signature free of lifetime parameters.
/// Automatically derives [`Hash`] so the entire turn context is content-addressed for memoization
/// without risk of manual hasher drift.
#[derive(Debug, Clone, Default, Hash, PartialEq, Eq)]
pub struct SystemPromptContext {
    /// The composed identity line — name/mission, a persona override, or a
    /// `/master` role directive. Empty when the embedding supplies no identity,
    /// which is the shipped coding CLI's default.
    pub identity_preamble: String,
    /// Names of the tools admitted this turn (e.g. `["ask_user", ...]`).
    pub tool_names: Vec<String>,
    /// Whether a subagent-dispatch tool (one whose
    /// [`Tool::spawns_subagent`](nuo_contracts::Tool::spawns_subagent) is
    /// true) is admitted this turn. Capability-derived, so a renamed or new
    /// dispatch tool needs no prompt-policy change.
    pub has_subagent_tool: bool,
    /// Model-specific guidance from the resolved model.
    pub model_guidance: &'static str,
    /// Provider/protocol-specific prompt guidance from the active provider.
    pub provider_guidance: &'static str,
    /// Content-attested project instructions (e.g. AGENTS.md).
    pub project_rules: String,
    /// Canonicalized additional workspace roots admitted alongside the primary.
    pub additional_workspace_roots: Vec<String>,
    /// The primary workspace root path, if any.
    pub workspace_root: Option<String>,
}

impl SystemPromptContext {
    /// An all-empty context for registry-mechanics tests and turns with no identity/tools.
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Relative or semantic ordering placement of an instruction section within its tier.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InstructionOrder {
    /// Placed at the very front of its tier.
    Head,
    /// Placed before the specified section id within its tier.
    Before(&'static str),
    /// Placed after the specified section id within its tier.
    After(&'static str),
    /// Explicit rank/order within its tier (lower values sort earlier).
    Index(u32),
    /// Placed at the end of its tier (default).
    #[default]
    Tail,
}

/// A self-contained, declaratively registered instruction section.
pub trait SystemPromptSection: Send + Sync {
    /// Stable unique identifier, used for overrides, disables, dependencies, and tracing.
    /// Convention: `system.<area>[.<name>]`, e.g. `"system.project_rules"`.
    fn id(&self) -> &'static str;

    /// Cache tier and lifetime volatility of this section.
    fn tier(&self) -> InstructionTier {
        InstructionTier::Session
    }

    /// Semantic ordering relation within its tier.
    fn order(&self) -> InstructionOrder {
        InstructionOrder::Tail
    }

    /// Backward-compatible rank accessor; maps to Index if not overridden.
    fn rank(&self) -> u32 {
        match self.order() {
            InstructionOrder::Head => 0,
            InstructionOrder::Index(idx) => idx,
            InstructionOrder::Before(_) => 50,
            InstructionOrder::After(_) => 60,
            InstructionOrder::Tail => 100,
        }
    }

    /// Whether this section applies in the current context. Default `true`.
    fn is_active(&self, _ctx: &SystemPromptContext) -> bool {
        true
    }

    /// Render the section body. `None` means "active but produces no text this
    /// turn"; the registry skips a `None` without leaving a blank gap.
    fn render(&self, ctx: &SystemPromptContext) -> Option<String>;
}

/// A registered section entry plus runtime overrides.
struct Entry {
    section: Box<dyn SystemPromptSection + Send + Sync>,
    order_override: Option<InstructionOrder>,
    disabled: bool,
}

impl Entry {
    fn id(&self) -> &'static str {
        self.section.id()
    }

    fn tier(&self) -> InstructionTier {
        self.section.tier()
    }

    fn effective_order(&self) -> InstructionOrder {
        self.order_override
            .clone()
            .unwrap_or_else(|| self.section.order())
    }
}

/// System-prompt policy assembled before an agent starts running.
///
/// Holds registered [`SystemPromptSection`]s, keyed by stable id.
/// Active fragments are topologically ordered and assembled into a structured
/// [`InstructionBundle`] by [`build_bundle`](Self::build_bundle).
#[derive(Default)]
pub struct SystemPromptRegistry {
    entries: Vec<Entry>,
    /// Content-addressed memo of the last render: (context_hash, InstructionBundle).
    render_memo: std::sync::Mutex<Option<(u64, InstructionBundle)>>,
}

/// Configuration error returned while composing a [`SystemPromptRegistry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemPromptRegistryError {
    /// A section with the same stable id is already registered.
    DuplicateId(&'static str),
    /// An override refers to an id that is not registered.
    UnknownId(String),
}

impl std::fmt::Display for SystemPromptRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "duplicate SystemPromptSection id: {id}"),
            Self::UnknownId(id) => write!(f, "unknown SystemPromptSection id: {id}"),
        }
    }
}

impl std::error::Error for SystemPromptRegistryError {}

impl SystemPromptRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a section. Panics on a duplicate id.
    pub fn register<S: SystemPromptSection + 'static>(&mut self, section: S) {
        if let Err(error) = self.try_register(section) {
            panic!("{error}");
        }
    }

    /// Register a section without panicking on an id collision.
    pub fn try_register<S: SystemPromptSection + 'static>(
        &mut self,
        section: S,
    ) -> Result<(), SystemPromptRegistryError> {
        let id = section.id();
        if self.entries.iter().any(|e| e.id() == id) {
            return Err(SystemPromptRegistryError::DuplicateId(id));
        }
        self.entries.push(Entry {
            section: Box::new(section),
            order_override: None,
            disabled: false,
        });
        Ok(())
    }

    /// Override a section's ordering relative to other sections.
    pub fn set_order(
        &mut self,
        id: &str,
        order: InstructionOrder,
    ) -> Result<(), SystemPromptRegistryError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id() == id)
            .ok_or_else(|| SystemPromptRegistryError::UnknownId(id.to_owned()))?;
        entry.order_override = Some(order);
        Ok(())
    }

    /// Set explicit integer rank override (backward-compatible convenience).
    pub fn set_rank(&mut self, id: &str, rank: u32) -> Result<(), SystemPromptRegistryError> {
        self.set_order(id, InstructionOrder::Index(rank))
    }

    /// Disable a section by id (it is skipped as if inactive).
    pub fn disable(&mut self, id: &str) -> Result<(), SystemPromptRegistryError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id() == id)
            .ok_or_else(|| SystemPromptRegistryError::UnknownId(id.to_owned()))?;
        entry.disabled = true;
        Ok(())
    }

    /// Build a structured, memoized [`InstructionBundle`] from the current context.
    pub fn build_bundle(&self, ctx: &SystemPromptContext) -> InstructionBundle {
        let mut hasher = DefaultHasher::new();
        ctx.hash(&mut hasher);
        let hash = hasher.finish();

        if let Ok(memo) = self.render_memo.lock()
            && let Some((cached_hash, bundle)) = memo.as_ref()
            && *cached_hash == hash
        {
            return bundle.clone();
        }

        let bundle = self.render_bundle(ctx);
        if let Ok(mut memo) = self.render_memo.lock() {
            *memo = Some((hash, bundle.clone()));
        }
        bundle
    }

    /// Legacy / debug helper: render all sections into a combined `Role::System` message.
    pub fn build_message(&self, ctx: &SystemPromptContext) -> Message {
        let bundle = self.build_bundle(ctx);
        Message::new(Role::System, bundle.render_combined())
            .with_origin(InjectionOrigin::new(InjectionKind::SystemPrompt))
    }

    /// Topological assembly path: groups by tier and resolves relative order constraints.
    fn render_bundle(&self, ctx: &SystemPromptContext) -> InstructionBundle {
        let active_entries: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| !e.disabled && e.section.is_active(ctx))
            .collect();

        // Separate by tier (Base -> Session -> Task -> Ephemeral)
        let tiers = [
            InstructionTier::Base,
            InstructionTier::Session,
            InstructionTier::Task,
            InstructionTier::Ephemeral,
        ];

        let mut slices = Vec::new();
        for tier in tiers {
            let tier_entries: Vec<&Entry> = active_entries
                .iter()
                .copied()
                .filter(|e| e.tier() == tier)
                .collect();
            let ordered_indices = sort_tier_entries(&tier_entries);
            for idx in ordered_indices {
                let entry = tier_entries[idx];
                if let Some(content) = entry.section.render(ctx)
                    && !content.trim().is_empty()
                {
                    slices.push(InstructionSlice::new(entry.id(), tier, content));
                }
            }
        }

        let mut bundle = InstructionBundle::new(slices);
        // A section's leading `\n` is its paragraph break from whatever
        // precedes it. The shipped agent carries no identity line, so the
        // host-environment section can be first — drop its separator rather
        // than opening the system message with a blank line.
        if let Some(first) = bundle.slices.first_mut() {
            first.content = first.content.trim_start_matches('\n').to_owned();
        }
        bundle
    }
}

/// Topologically sorts entries within a single tier based on `InstructionOrder`.
fn sort_tier_entries(entries: &[&Entry]) -> Vec<usize> {
    let n = entries.len();
    if n <= 1 {
        return (0..n).collect();
    }

    let id_to_index: HashMap<&'static str, usize> = entries
        .iter()
        .enumerate()
        .map(|(idx, e)| (e.id(), idx))
        .collect();

    let mut heads = Vec::new();
    let mut indices: Vec<(u32, usize)> = Vec::new();
    let mut befores: Vec<(usize, usize)> = Vec::new(); // (item_idx, target_idx)
    let mut afters: Vec<(usize, usize)> = Vec::new(); // (item_idx, target_idx)
    let mut tails = Vec::new();

    for (idx, entry) in entries.iter().enumerate() {
        match entry.effective_order() {
            InstructionOrder::Head => heads.push(idx),
            InstructionOrder::Index(rank) => indices.push((rank, idx)),
            InstructionOrder::Before(target_id) => {
                if let Some(&target_idx) = id_to_index.get(target_id) {
                    befores.push((idx, target_idx));
                } else {
                    tails.push(idx);
                }
            }
            InstructionOrder::After(target_id) => {
                if let Some(&target_idx) = id_to_index.get(target_id) {
                    afters.push((idx, target_idx));
                } else {
                    tails.push(idx);
                }
            }
            InstructionOrder::Tail => tails.push(idx),
        }
    }

    indices.sort_by_key(|(rank, _)| *rank);
    let mut result = Vec::with_capacity(n);
    result.extend(heads);
    for (_, idx) in indices {
        result.push(idx);
    }
    result.extend(tails);

    // Apply Before / After relative dependencies
    for (item_idx, target_idx) in befores {
        if let Some(target_pos) = result.iter().position(|&x| x == target_idx) {
            result.retain(|&x| x != item_idx);
            let insert_pos = result
                .iter()
                .position(|&x| x == target_idx)
                .unwrap_or(target_pos);
            result.insert(insert_pos, item_idx);
        } else if !result.contains(&item_idx) {
            result.push(item_idx);
        }
    }

    for (item_idx, target_idx) in afters {
        if let Some(target_pos) = result.iter().position(|&x| x == target_idx) {
            result.retain(|&x| x != item_idx);
            let target_current = result
                .iter()
                .position(|&x| x == target_idx)
                .unwrap_or(target_pos);
            result.insert(target_current + 1, item_idx);
        } else if !result.contains(&item_idx) {
            result.push(item_idx);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestSection {
        id: &'static str,
        tier: InstructionTier,
        order: InstructionOrder,
        active: bool,
        text: Option<&'static str>,
    }

    impl SystemPromptSection for TestSection {
        fn id(&self) -> &'static str {
            self.id
        }
        fn tier(&self) -> InstructionTier {
            self.tier
        }
        fn order(&self) -> InstructionOrder {
            self.order.clone()
        }
        fn is_active(&self, _ctx: &SystemPromptContext) -> bool {
            self.active
        }
        fn render(&self, _ctx: &SystemPromptContext) -> Option<String> {
            self.text.map(String::from)
        }
    }

    fn sec(id: &'static str, order: InstructionOrder, text: &'static str) -> TestSection {
        TestSection {
            id,
            tier: InstructionTier::Base,
            order,
            active: true,
            text: Some(text),
        }
    }

    #[test]
    fn relative_ordering_resolves_semantic_dependencies() {
        let mut reg = SystemPromptRegistry::new();
        reg.register(sec("tail_item", InstructionOrder::Tail, "Tail"));
        reg.register(sec("head_item", InstructionOrder::Head, "Head"));
        reg.register(sec(
            "after_head",
            InstructionOrder::After("head_item"),
            "AfterHead",
        ));
        reg.register(sec(
            "before_tail",
            InstructionOrder::Before("tail_item"),
            "BeforeTail",
        ));

        let bundle = reg.build_bundle(&SystemPromptContext::empty());
        assert_eq!(
            bundle.render_combined(),
            "Head\nAfterHead\nBeforeTail\nTail"
        );
    }

    #[test]
    fn equal_orders_preserve_registration_order() {
        let mut reg = SystemPromptRegistry::new();
        reg.register(sec("system.a", InstructionOrder::Head, "A"));
        reg.register(sec("system.b", InstructionOrder::Head, "B"));

        let bundle = reg.build_bundle(&SystemPromptContext::empty());
        assert_eq!(bundle.render_combined(), "A\nB");
    }

    #[test]
    fn first_section_loses_its_separator_newline() {
        // Sections carry a leading `\n` as their paragraph break from whatever
        // precedes them. When there is no identity line, the host section is
        // first and must not open the message with a blank line.
        let mut reg = SystemPromptRegistry::new();
        reg.register(sec("system.leading", InstructionOrder::Head, "\n## Host"));
        reg.register(sec(
            "system.trailing",
            InstructionOrder::Tail,
            "\n## Persistence",
        ));

        let bundle = reg.build_bundle(&SystemPromptContext::empty());
        assert_eq!(bundle.render_combined(), "## Host\n\n## Persistence");
    }

    #[test]
    fn disabled_section_is_skipped() {
        let mut reg = SystemPromptRegistry::new();
        reg.register(sec("system.a", InstructionOrder::Head, "A"));
        reg.register(sec("system.b", InstructionOrder::Tail, "B"));
        reg.disable("system.a").unwrap();

        let bundle = reg.build_bundle(&SystemPromptContext::empty());
        assert_eq!(bundle.render_combined(), "B");
    }

    #[test]
    fn order_override_changes_position() {
        let mut reg = SystemPromptRegistry::new();
        reg.register(sec("system.a", InstructionOrder::Head, "A"));
        reg.register(sec("system.b", InstructionOrder::Tail, "B"));
        reg.set_order("system.b", InstructionOrder::Before("system.a"))
            .unwrap();

        let bundle = reg.build_bundle(&SystemPromptContext::empty());
        assert_eq!(bundle.render_combined(), "B\nA");
    }

    #[test]
    fn system_message_origin_is_system_prompt() {
        let mut reg = SystemPromptRegistry::new();
        reg.register(sec("system.a", InstructionOrder::Head, "A"));
        let msg = reg.build_message(&SystemPromptContext::empty());
        assert_eq!(
            msg.origin.map(|o| o.kind),
            Some(InjectionKind::SystemPrompt)
        );
    }

    #[test]
    fn memoization_returns_identical_bundle() {
        let mut reg = SystemPromptRegistry::new();
        reg.register(sec("system.a", InstructionOrder::Head, "A"));
        let b1 = reg.build_bundle(&SystemPromptContext::empty());
        let b2 = reg.build_bundle(&SystemPromptContext::empty());
        assert_eq!(b1, b2);
    }
}
