//! Distributed tool registration.
//!
//! Built-in tools self-register at their definition site via the
//! [`register_tool!`](crate::register_tool!) macro instead of being enumerated
//! by hand at the agent's assembly point. Submissions are collected at runtime
//! by the [`inventory`] crate, so adding a tool is a one-line change in its own
//! module — no central list to keep in sync.
//!
//! Tools that need runtime state (config blobs, shared registries) pull it out
//! of an opaque [`ToolContext`]: a type-keyed service map. This keeps
//! `nuo-wire` free of dependencies on the concrete state types, which live
//! in higher crates (e.g. `SkillRegistry` in `nuo-skills`). The map is the
//! only seam.
//!
//! A handful of "meta" tools genuinely cannot self-register — e.g. a subagent
//! dispatch tool that needs a snapshot of the *rest* of the toolset, which is
//! the registry's own output. Those are still assembled explicitly where the
//! dependency is created; the registry collects everything that is
//! self-contained.

use crate::model::Model;
use crate::{Tool, VariantSelection, empty_variant_selection};
use std::any::{Any, TypeId};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// An opaque, type-keyed bag of services handed to each [`ToolFactory`] at
/// build time. Tools look their concrete dependencies up by Rust type. There
/// is intentionally no typed list of fields: that would force `nuo-wire` to
/// depend on every higher crate's state types. The map is the seam.
///
/// Cheaply [`Clone`](self::ToolContext#method.clone)-able (shared via `Arc`);
/// build one with [`ToolContextBuilder`] and freeze it.
#[derive(Clone, Default)]
pub struct ToolContext {
    services: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl ToolContext {
    /// Look up a value by its exact type. `None` if no service of type `T` was
    /// provided.
    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.services
            .get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref::<T>())
    }

    /// Look up a shared handle stored as `Arc<T>` and clone the `Arc` out.
    /// Shorthand for `self.get::<Arc<T>>().cloned()`.
    pub fn shared<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.get::<Arc<T>>().cloned()
    }
}

/// The directory every workspace-relative tool operation resolves against,
/// and the directory shell commands run in.
///
/// Under the unified session daemon (ADR-0096) one process hosts sessions for
/// *many* projects, so the daemon's own process cwd is whichever directory the
/// first client happened to spawn it from — correct only by coincidence. A
/// session-scoped tool must therefore never consult the process cwd: it
/// resolves relative paths and spawns subprocesses against the session's
/// project root, provided here as a context service. The mapping is a
/// newtype (not a bare `PathBuf`) so a future provider of the same path under
/// a different meaning cannot collide with it in the type-keyed map.
///
/// Provided by the bootstrap that assembles the session (`nuo-server`).
/// A context without one (unit tests, subagent sub-agents built from a static
/// snapshot) leaves the service unset and tools fall back to the process cwd —
/// the historical behaviour — which remains correct wherever one process
/// serves exactly one project.
#[derive(Debug, Clone)]
pub struct WorkspaceRoot(pub std::path::PathBuf);

impl ToolContext {
    /// The session's workspace root, when the assembling bootstrap provided
    /// one. `None` in contexts that predate or decline the service (tests,
    /// side sessions); callers treat that as "use the process cwd".
    pub fn workspace_root(&self) -> Option<&std::path::Path> {
        self.get::<WorkspaceRoot>().map(|root| root.0.as_path())
    }

    /// The session's admitted root set, when the bootstrap provided one.
    ///
    /// `None` means the session declared no additional roots (or predates
    /// ADR-0142); callers fall back to [`Self::workspace_root`]-only
    /// admission, which is the strictly narrower set.
    pub fn workspace_roots(&self) -> Option<&WorkspaceRoots> {
        self.get::<WorkspaceRoots>()
    }
}

/// The session's complete admitted filesystem authority (ADR-0142): the
/// primary workspace root plus any configured additional roots.
///
/// The primary root keeps every historical duty — relative paths resolve
/// against it, shells start in it, project-bound discovery (skills,
/// extensions, `.nuo/config.toml`) reads only it. Additional roots purely
/// *widen admission*: a path under any root passes the sandbox's
/// containment check instead of failing closed. This is the cross-project
/// escape hatch — a session for a frontend repo can admit the sibling
/// backend repo without `/trust`-ing host execution or re-rooting.
///
/// Every root is stored canonicalized at assembly time, so containment
/// checks are plain `starts_with` on lexical paths with no symlink
/// re-resolution per operation.
#[derive(Debug, Clone)]
pub struct WorkspaceRoots {
    primary: std::path::PathBuf,
    additional: Vec<std::path::PathBuf>,
}

impl WorkspaceRoots {
    /// Single-root authority: admission is exactly the primary root.
    pub fn single(primary: impl Into<std::path::PathBuf>) -> Self {
        Self {
            primary: primary.into(),
            additional: Vec::new(),
        }
    }

    /// Multi-root authority. The primary keeps its duties; `additional`
    /// (already canonicalized by the loader) widens admission only.
    pub fn new(
        primary: impl Into<std::path::PathBuf>,
        additional: Vec<std::path::PathBuf>,
    ) -> Self {
        Self {
            primary: primary.into(),
            additional,
        }
    }

    /// The root relative paths resolve against and shells start in.
    pub fn primary(&self) -> &std::path::Path {
        self.primary.as_path()
    }

    /// Configured additional roots, in declaration order.
    pub fn additional(&self) -> &[std::path::PathBuf] {
        self.additional.as_slice()
    }

    /// Every admitted root: primary first, then additional in order.
    pub fn iter(&self) -> impl Iterator<Item = &std::path::Path> {
        std::iter::once(self.primary.as_path()).chain(self.additional.iter().map(|p| p.as_path()))
    }

    /// Admission check: does `path` (already canonical/absolute) fall under
    /// any admitted root? An empty root set admits nothing, so this stays
    /// fail-closed for degenerate configurations.
    pub fn contains(&self, path: &std::path::Path) -> bool {
        self.iter().any(|root| path.starts_with(root))
    }

    /// True when the session admitted anything beyond the primary root.
    pub fn is_multi_root(&self) -> bool {
        !self.additional.is_empty()
    }
}

/// Builder for [`ToolContext`]. Provide services by concrete type, then
/// [`build`](Self::build) to freeze an immutable, cheaply cloneable context.
#[derive(Default)]
pub struct ToolContextBuilder {
    services: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl ToolContextBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Provide a service by its concrete type. Later inserts of the same type
    /// replace earlier ones.
    pub fn provide<T: Any + Send + Sync>(&mut self, value: T) -> &mut Self {
        self.services.insert(TypeId::of::<T>(), Arc::new(value));
        self
    }

    /// Freeze the context.
    pub fn build(self) -> ToolContext {
        ToolContext {
            services: Arc::new(self.services),
        }
    }
}

/// Build step that turns a [`ToolContext`] into one tool instance. Each
/// concrete tool registers its own factory (a private unit struct) via
/// [`register_tool!`](crate::register_tool!). Returning `None` lets a factory
/// decline (e.g. a required service is absent or a feature is off); the tool
/// then simply does not appear in the assembled set.
pub trait ToolFactory: Send + Sync {
    fn build(&self, ctx: &ToolContext) -> Option<Arc<dyn Tool>>;
}

/// An entry in the global, compile-time tool registry. Collected by
/// [`inventory`]; never constructed by hand.
pub struct ToolRegistration {
    pub factory: &'static dyn ToolFactory,
}

inventory::collect!(ToolRegistration);

/// Collect every self-registered tool into a [`ToolSet`], grouping the
/// registrations into capabilities (by [`Tool::name`]) and variants (by
/// [`Tool::variant`]). Factories that decline (return `None`) are skipped.
/// Duplicate `(name, variant)` pairs are dropped keeping the first registration
/// — an accidental double-register merely shadows instead of breaking the agent
/// in release. Unlike MCP tool names (data-driven, lossily sanitized, so
/// disambiguated at runtime), a builtin name is a compile-time contract that
/// appears verbatim in prompts and provider schemas: a `(name, variant)`
/// collision is a programming error, not a recoverable data condition. We catch
/// it loudly at its source in debug/test builds via
/// `debug_assert_unique_identities`, where which-registration-wins would
/// otherwise depend on non-deterministic `inventory` link order.
pub fn collect_toolset(ctx: &ToolContext) -> ToolSet {
    let mut tools = Vec::new();
    for entry in inventory::iter::<ToolRegistration> {
        if let Some(tool) = entry.factory.build(ctx) {
            tools.push(tool);
        }
    }
    debug_assert_unique_identities(&tools);
    debug_assert_safe_targets(&tools);
    ToolSet::from_tools(tools)
}

/// Panic if any two tools share the same `(name, variant)` identity. A no-op in
/// release (the [`ToolSet`] first-wins shadow keeps the agent running); a hard
/// failure in debug/test builds so an accidental double-register is caught in CI
/// rather than silently shipped.
#[cfg(debug_assertions)]
fn debug_assert_unique_identities(tools: &[Arc<dyn Tool>]) {
    let mut seen = BTreeSet::new();
    for tool in tools {
        assert!(
            seen.insert((tool.name(), tool.variant())),
            "duplicate builtin tool registration for (name={:?}, variant={:?}): \
             two register_tool! sites claim the same identity",
            tool.name(),
            tool.variant(),
        );
    }
}

/// #8 — Safety contract: a tool that declares a **mutating** `accesses()`
/// (a `Write`/`ReadWrite`/`All`) **must** override `scope_target` to return a
/// non-`Unspecified` target for the inputs it mutates. Otherwise the call
/// bypasses the bash-policy and permission broker — the gates that key off
/// `ScopeTarget::Unspecified` as the shared "no locatable target / auto-admit"
/// switch — so a write would run with zero
/// permission checks.
///
/// This cannot be enforced perfectly by sampling one input: a tool may take a
/// path argument that is absent in the sample. So this is a **smell check**, not
/// a proof: it flags only the structural case where the tool declares writes
/// *unconditionally* yet returns `Unspecified` even for a representative,
/// well-formed input. A tool that conditionally mutates based on its arguments
/// (and correctly returns a `Path`/`Command` when it does) passes trivially.
///
/// A no-op in release; a hard failure in debug/test builds.
#[cfg(debug_assertions)]
fn debug_assert_safe_targets(tools: &[Arc<dyn Tool>]) {
    // Representative inputs exercising the common arities. A tool whose write
    // path only appears for a specific argument shape is expected to override
    // `scope_target` to surface it for *some* input; sampling a handful keeps
    // the check cheap while catching the unconditional-write default mistake.
    let samples = [
        "{}",
        r#"{"path":"src/example.rs"}"#,
        r#"{"command":"echo hi"}"#,
        r#"{"path":"/tmp/x","content":"y"}"#,
    ];
    for tool in tools {
        // Skip tools that never declare a write — read-only tools legitimately
        // use the `Unspecified` default (it is their auto-admit path).
        let any_write = samples
            .iter()
            .any(|sample| tool.accesses(sample).declares_write());
        if !any_write {
            continue;
        }
        // The tool declares a write for some sample input. For *that same*
        // sample, its scope_target must be non-Unspecified, otherwise the write
        // would bypass every permission gate.
        for sample in samples {
            let accesses = tool.accesses(sample);
            if !accesses.declares_write() {
                continue;
            }
            let target = tool.scope_target(sample);
            assert!(
                !matches!(target, crate::ScopeTarget::Unspecified),
                "tool {:?} declares a mutating access (writes) for input {:?} but \
                 returns ScopeTarget::Unspecified — the write would bypass the \
                 bash-policy and permission broker. Override `scope_target` to \
                 return the Path/Command the call touches.",
                tool.name(),
                sample,
            );
        }
    }
}

#[cfg(not(debug_assertions))]
#[inline]
fn debug_assert_safe_targets(_tools: &[Arc<dyn Tool>]) {}

#[cfg(not(debug_assertions))]
#[inline]
fn debug_assert_unique_identities(_tools: &[Arc<dyn Tool>]) {}

/// One capability: a logical tool identity (its [`Tool::name`]) realized by one
/// or more variants keyed by [`Tool::variant`]. Exactly one variant is the
/// default — the one the model sees when no selection names this capability.
#[derive(Clone)]
pub struct Capability {
    name: String,
    default_variant: String,
    variants: BTreeMap<String, Arc<dyn Tool>>,
}

impl Capability {
    /// The capability name (== every variant's [`Tool::name`]).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The id of the default variant.
    pub fn default_variant(&self) -> &str {
        &self.default_variant
    }

    /// The variant for `id`, or the default variant when `id` is `None` or names
    /// a variant this capability does not have.
    pub fn variant_or_default(&self, id: Option<&str>) -> &Arc<dyn Tool> {
        id.and_then(|v| self.variants.get(v))
            .unwrap_or_else(|| &self.variants[&self.default_variant])
    }

    /// Pick the variant this capability resolves to for `model`, honouring the
    /// `preferred` variant id (the composed override) but **never** selecting a
    /// variant the model cannot execute. Returns `None` when no variant is
    /// usable on `model` — i.e. the whole capability is unavailable on this
    /// model and must be dropped from the resolved set.
    ///
    /// Preference order among model-usable variants: the explicitly `preferred`
    /// id, else the capability's default, else the lexicographically-smallest
    /// usable id (deterministic). The model filter is **hard**: an unusable
    /// preferred/default variant is skipped, not surfaced — which is how a
    /// model's capability limit overrides any agent-side override.
    pub fn usable_variant_for(
        &self,
        model: &Model,
        preferred: Option<&str>,
    ) -> Option<&Arc<dyn Tool>> {
        let usable = |id: &str| {
            self.variants
                .get(id)
                .filter(|tool| model_can_use(tool.as_ref(), model))
        };
        if let Some(tool) = preferred.and_then(usable) {
            return Some(tool);
        }
        if let Some(tool) = usable(&self.default_variant) {
            return Some(tool);
        }
        // BTreeMap iterates in sorted key order, so the first usable is the
        // smallest usable id.
        self.variants
            .values()
            .find(|tool| model_can_use(tool.as_ref(), model))
    }

    /// All variant ids of this capability, in sorted order.
    pub fn variant_ids(&self) -> impl Iterator<Item = &str> {
        self.variants.keys().map(String::as_str)
    }

    /// Recompute the default variant: the one literally named `"default"` if
    /// present, otherwise the lexicographically-smallest id (deterministic —
    /// `inventory` link order is not). `variants` is never empty here.
    fn recompute_default(&mut self) {
        const DEFAULT: &str = "default";
        self.default_variant = if self.variants.contains_key(DEFAULT) {
            DEFAULT.to_string()
        } else {
            // BTreeMap keys are sorted, so the first key is the smallest.
            self.variants
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| DEFAULT.to_string())
        };
    }
}

/// The full set of tool capabilities, each with its variants. This is the
/// single source of truth the agent resolves against: given a
/// [`VariantSelection`] (per-model and/or per-profile), [`ToolSet::resolve`]
/// yields exactly one variant per capability. Cheaply [`Clone`]-able (variants
/// are `Arc`).
#[derive(Clone, Default)]
pub struct ToolSet {
    capabilities: BTreeMap<String, Capability>,
}

impl ToolSet {
    /// Build a toolset from a flat list of tools. Tools are grouped by
    /// [`Tool::name`] into capabilities and by [`Tool::variant`] into variants;
    /// the first tool seen for a given `(name, variant)` wins.
    pub fn from_tools(tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Self {
        let mut set = ToolSet::default();
        for tool in tools {
            set.insert(tool);
        }
        set
    }

    /// Add one tool as a variant of its capability, creating the capability if
    /// new. A `(name, variant)` already present is left untouched (first wins).
    /// The capability's default variant is recomputed after the insert.
    pub fn insert(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        let variant = tool.variant().to_string();
        let cap = self
            .capabilities
            .entry(name.clone())
            .or_insert_with(|| Capability {
                name,
                default_variant: variant.clone(),
                variants: BTreeMap::new(),
            });
        cap.variants.entry(variant).or_insert(tool);
        cap.recompute_default();
    }

    /// Add or replace one concrete `(name, variant)` implementation.
    ///
    /// Registry collection uses [`Self::insert`] so accidental duplicate
    /// registrations remain first-wins. Runtime owners use this explicit
    /// method when an invariant-bound implementation must take precedence.
    pub fn upsert(&mut self, tool: Arc<dyn Tool>) -> Option<Arc<dyn Tool>> {
        let name = tool.name().to_string();
        let variant = tool.variant().to_string();
        let cap = self
            .capabilities
            .entry(name.clone())
            .or_insert_with(|| Capability {
                name,
                default_variant: variant.clone(),
                variants: BTreeMap::new(),
            });
        let replaced = cap.variants.insert(variant, tool);
        cap.recompute_default();
        replaced
    }

    /// Resolve the toolset to exactly one variant per capability for the given
    /// selection: a capability listed in `selection` is realized by its named
    /// variant (falling back to the default if the named variant is absent);
    /// capabilities not listed use their default variant.
    pub fn resolve(&self, selection: &VariantSelection) -> Vec<Arc<dyn Tool>> {
        self.capabilities
            .values()
            .map(|cap| {
                cap.variant_or_default(selection.get(cap.name()).map(String::as_str))
                    .clone()
            })
            .collect()
    }

    /// The default view: one default variant per capability (empty selection).
    pub fn default_view(&self) -> Vec<Arc<dyn Tool>> {
        self.resolve(empty_variant_selection())
    }

    /// Resolve the pool to exactly one tool per **surviving** capability for an
    /// agent of selection `agent` running on `model`, whose own capability
    /// limits and variant preferences are `model_sel`. This is the single,
    /// authoritative entry point that composes the two selectors into a live
    /// toolset.
    ///
    /// Composition follows the two-axis algebra:
    ///
    /// - **Scope (which capabilities)** is the *meet* of the two scopes:
    ///   `agent.scope ∩ model_sel.scope`. Both are ceilings; a capability
    ///   survives only if **both** parties admit it. Neither can widen the
    ///   other.
    /// - **Override (which variant)** is by *precedence*: the agent's variant
    ///   preference wins, the model's fills any capability the agent left
    ///   unspecified ([`VariantSelection`] overlay, agent over model).
    /// - **Model capability limits are hard.** A variant `model` cannot execute
    ///   (e.g. [`Tool::requires_vision`] on a text-only model) is never
    ///   selectable; a capability with *no* model-usable variant is dropped
    ///   entirely. Because the unusable variant is simply absent, no agent-side
    ///   override can reinstate it — the model's limit always wins, by
    ///   construction rather than by a priority rule.
    ///
    /// Result order is by capability name (the pool's `BTreeMap` order),
    /// deterministic regardless of `inventory` link order.
    pub fn resolve_for(
        &self,
        model: &Model,
        agent: &ToolSelection,
        model_sel: &ToolSelection,
    ) -> Vec<Arc<dyn Tool>> {
        let preferred = overlay_variants(&agent.variants, &model_sel.variants);
        self.capabilities
            .values()
            .filter(|cap| agent.scope.admits(cap.name()) && model_sel.scope.admits(cap.name()))
            .filter_map(|cap| {
                cap.usable_variant_for(model, preferred.get(cap.name()).map(String::as_str))
                    .cloned()
            })
            .collect()
    }

    /// Look up a capability by name.
    pub fn variants_of(&self, name: &str) -> Option<&Capability> {
        self.capabilities.get(name)
    }

    /// All capability names, in sorted order.
    pub fn capability_names(&self) -> impl Iterator<Item = &str> {
        self.capabilities.keys().map(String::as_str)
    }

    /// Whether the set holds no capabilities.
    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }

    /// Number of capabilities (not variants).
    pub fn len(&self) -> usize {
        self.capabilities.len()
    }
}

/// Whether `model` can actually execute `tool` — the hard model-capability
/// filter applied during [`ToolSet::resolve_for`]. Today the only axis is
/// vision ([`Tool::requires_vision`]); add a conjunct here when a new
/// model-capability requirement is introduced (the resolver and every selector
/// then inherit it for free).
fn model_can_use(tool: &dyn Tool, model: &Model) -> bool {
    !tool.requires_vision() || model.vision
}

/// Overlay two variant selections by precedence: every entry of `high` wins,
/// `low` fills only the capabilities `high` leaves unspecified. This is the
/// **override-axis** composition — the agent's selection is `high`, the model's
/// is `low`, so an agent override beats a model override while the model still
/// supplies a variant for capabilities the agent did not pin.
fn overlay_variants(high: &VariantSelection, low: &VariantSelection) -> VariantSelection {
    let mut merged = low.clone();
    merged.extend(high.iter().map(|(k, v)| (k.clone(), v.clone())));
    merged
}

/// The **scope axis** of a [`ToolSelection`]: which capabilities (by
/// [`Tool::name`]) a party admits. A ceiling, not a request — composing two
/// scopes can only narrow, never widen. Capabilities are named, not realised;
/// the scope is deliberately blind to *which variant* is used (that is the
/// override axis).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ToolScope {
    /// Admit every capability in the pool. The principal agent's default, and a
    /// model's default (a model restricts by *capability requirement*, e.g.
    /// vision, not by capability *name*).
    #[default]
    All,
    /// Admit only the named capabilities. An empty set admits nothing. Used by
    /// subagent roles to confine a spawned agent to e.g. the read-only inspection
    /// tools.
    Only(BTreeSet<String>),
}

impl ToolScope {
    /// Build an `Only` scope from a list of capability names.
    pub fn only<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        ToolScope::Only(names.into_iter().map(Into::into).collect())
    }

    /// Build a scope from an allowlist of capability names or patterns (ADR-0246).
    pub fn from_allowlist<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let set: BTreeSet<String> = patterns.into_iter().map(Into::into).collect();
        if set.contains("*") {
            ToolScope::All
        } else {
            ToolScope::Only(set)
        }
    }

    /// Whether this scope admits the capability `name`.
    pub fn admits(&self, name: &str) -> bool {
        match self {
            ToolScope::All => true,
            ToolScope::Only(set) => set.iter().any(|pat| {
                if pat == "*" {
                    true
                } else if let Some(prefix) = pat.strip_suffix('*') {
                    name.starts_with(prefix)
                } else {
                    pat == name
                }
            }),
        }
    }

    /// The *meet* of two scopes — the set of capabilities **both** admit.
    /// `All` is the identity (`All ∩ x == x`); two `Only` sets intersect.
    /// Commutative and associative, so composition order is irrelevant.
    pub fn intersect(&self, other: &ToolScope) -> ToolScope {
        match (self, other) {
            (ToolScope::All, other) => other.clone(),
            (this, ToolScope::All) => this.clone(),
            (ToolScope::Only(a), ToolScope::Only(b)) => {
                ToolScope::Only(a.intersection(b).cloned().collect())
            }
        }
    }
}

/// One party's ask of the [`ToolSet`] pool: a [`ToolScope`] (which capabilities)
/// plus a [`VariantSelection`] (which implementation of each). Both the agent
/// identity (principal or subagent role) and the active model express themselves
/// as a `ToolSelection`; [`ToolSet::resolve_for`] composes the two — scope by
/// intersection, variants by agent-over-model precedence — into the live
/// toolset.
#[derive(Debug, Clone, Default)]
pub struct ToolSelection {
    /// Which capabilities this party admits.
    pub scope: ToolScope,
    /// This party's variant preference per capability. Empty means "default
    /// variant"; entries are honoured only when the chosen variant is usable on
    /// the active model.
    pub variants: VariantSelection,
}

impl ToolSelection {
    /// An unrestricted selection: every capability, default variants. The
    /// principal agent's baseline before per-model variants are applied.
    pub fn unrestricted() -> Self {
        Self::default()
    }

    /// A selection that admits only capabilities matching the given allowlist patterns (ADR-0246).
    pub fn from_allowlist<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            scope: ToolScope::from_allowlist(patterns),
            variants: VariantSelection::new(),
        }
    }

    /// A selection that admits only the named capabilities (default variants).
    pub fn only<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            scope: ToolScope::only(names),
            variants: VariantSelection::new(),
        }
    }

    /// Attach a variant preference (override axis) to this selection.
    pub fn with_variants(mut self, variants: VariantSelection) -> Self {
        self.variants = variants;
        self
    }
}

/// An agent's tool requirement declaration against the pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDeclaration {
    /// Identifier or role of the declaring agent (e.g. "developer", "code_analyst", "explore").
    pub agent_name: String,
    /// Archetype/kind of the agent making the declaration.
    pub kind: crate::AgentKind,
    /// The capability name scope requested.
    pub scope: ToolScope,
}

impl ToolDeclaration {
    pub fn new(agent_name: impl Into<String>, kind: crate::AgentKind, scope: ToolScope) -> Self {
        Self {
            agent_name: agent_name.into(),
            kind,
            scope,
        }
    }
}

/// Audit snapshot of a capability's admission status for an agent against the pool.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolCapabilityAudit {
    pub capability: String,
    pub requested: bool,
    pub admitted: bool,
    pub active_variant: Option<String>,
    pub reason: Option<String>,
}

/// Snapshot of the tool pool for auditing and inspection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolPoolSnapshot {
    pub capabilities: Vec<ToolCapabilityAudit>,
}

/// The session-wide or system-wide tool pool: freezes a capability set as the pool,
/// against which agents declare their tool requirements and resolve concrete toolsets.
#[derive(Clone, Default)]
pub struct ToolPool {
    toolset: ToolSet,
    declarations: Arc<std::sync::RwLock<HashMap<String, ToolDeclaration>>>,
}

impl ToolPool {
    /// Create a new tool pool wrapping a capability set.
    pub fn new(toolset: ToolSet) -> Self {
        Self {
            toolset,
            declarations: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Build a tool pool from an iterator of tools.
    pub fn from_tools(tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Self {
        Self::new(ToolSet::from_tools(tools))
    }

    /// Access the underlying capability set.
    pub fn toolset(&self) -> &ToolSet {
        &self.toolset
    }

    /// Insert a tool into the pool.
    pub fn insert(&mut self, tool: Arc<dyn Tool>) {
        self.toolset.insert(tool);
    }

    /// Upsert a tool in the pool.
    pub fn upsert(&mut self, tool: Arc<dyn Tool>) -> Option<Arc<dyn Tool>> {
        self.toolset.upsert(tool)
    }

    /// Record an agent's requirement declaration against the pool.
    pub fn declare(&self, declaration: ToolDeclaration) {
        self.declarations
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(declaration.agent_name.clone(), declaration);
    }

    /// Get an agent's recorded declaration if any.
    pub fn get_declaration(&self, agent_name: &str) -> Option<ToolDeclaration> {
        self.declarations
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(agent_name)
            .cloned()
    }

    /// Resolve the pool to exactly one tool per surviving capability for an agent
    /// of selection `agent` running on `model`, whose own capability limits and variant
    /// preferences are `model_sel`.
    pub fn resolve_for(
        &self,
        model: &Model,
        agent: &ToolSelection,
        model_sel: &ToolSelection,
    ) -> Vec<Arc<dyn Tool>> {
        self.toolset.resolve_for(model, agent, model_sel)
    }

    /// Produce an audit snapshot of the pool against an agent's selection.
    pub fn snapshot(&self, agent: &ToolSelection, model: &Model) -> ToolPoolSnapshot {
        let mut audits = Vec::new();
        for (name, cap) in &self.toolset.capabilities {
            let requested = agent.scope.admits(name);
            let preferred_variant = agent.variants.get(name).map(String::as_str);
            let usable_tool = cap.usable_variant_for(model, preferred_variant);
            let admitted = requested && usable_tool.is_some();
            let active_variant = if admitted {
                usable_tool.map(|t| t.variant().to_string())
            } else {
                None
            };
            let reason = if !requested {
                Some("Not requested by agent tool scope".to_string())
            } else if usable_tool.is_none() {
                Some("No variant usable on model (e.g. missing model capability)".to_string())
            } else {
                None
            };
            audits.push(ToolCapabilityAudit {
                capability: name.clone(),
                requested,
                admitted,
                active_variant,
                reason,
            });
        }
        ToolPoolSnapshot {
            capabilities: audits,
        }
    }
}

/// Register a self-contained tool at its definition site.
///
/// Expands to a private factory unit struct implementing [`ToolFactory`] plus
/// an [`inventory`] submission.
///
/// Two forms:
///
/// - **Context-free** — for tools needing no runtime state. `$build` is any
///   expression evaluating to `T: Tool`:
///   ```ignore
///   nuo_wire::register_tool!(ExecuteCommandFactory => ExecuteCommandTool);
///   ```
///
/// - **Context-aware** — `$ctx` (a name *you* choose) binds the build context,
///   which `$build` may then reference. Use `?` / early `return None` to
///   decline (the tool then won't appear in the assembled set):
///   ```ignore
///   nuo_wire::register_tool!(WebReaderFactory => |ctx| {
///       let cfg = ctx.get::<nuo_wire::WebSearchConfig>()?.clone();
///       WebReaderTool::with_config(cfg)
///   });
///   ```
///
/// The caller names the context parameter (`|ctx|`) on purpose: Rust macro
/// hygiene means a binding invented inside the macro can't be referenced from
/// the call-site `$build` tokens. Giving the caller the parameter name keeps
/// both in the same hygiene context.
#[macro_export]
macro_rules! register_tool {
    ($id:ident => |$ctx:ident| $build:expr) => {
        struct $id;
        impl $crate::ToolFactory for $id {
            fn build(
                &self,
                $ctx: &$crate::tool_registry::ToolContext,
            ) -> Option<std::sync::Arc<dyn $crate::Tool>> {
                let tool: std::sync::Arc<dyn $crate::Tool> = std::sync::Arc::new($build);
                Some(tool)
            }
        }
        ::inventory::submit!($crate::tool_registry::ToolRegistration { factory: &$id });
    };
    ($id:ident => $build:expr) => {
        struct $id;
        impl $crate::ToolFactory for $id {
            fn build(
                &self,
                _ctx: &$crate::tool_registry::ToolContext,
            ) -> Option<std::sync::Arc<dyn $crate::Tool>> {
                let tool: std::sync::Arc<dyn $crate::Tool> = std::sync::Arc::new($build);
                Some(tool)
            }
        }
        ::inventory::submit!($crate::tool_registry::ToolRegistration { factory: &$id });
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScopeTarget, ToolAccesses};
    use async_trait::async_trait;
    use std::path::PathBuf;

    struct PingTool;
    #[async_trait]
    impl Tool for PingTool {
        fn name(&self) -> &str {
            "registry_ping"
        }
        fn description(&self) -> &str {
            "test tool"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("pong".to_string())
        }
    }

    struct DeclinedTool;
    #[async_trait]
    impl Tool for DeclinedTool {
        fn name(&self) -> &str {
            "registry_declined"
        }
        fn description(&self) -> &str {
            "declined test tool"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("never".to_string())
        }
    }

    register_tool!(PingFactory => PingTool);
    // Declines when a (deliberately absent) marker service is missing.
    register_tool!(DeclinedFactory => |ctx| {
        let _ = ctx.get::<String>()?;
        DeclinedTool
    });

    /// A well-behaved write tool: declares a write AND overrides scope_target to
    /// surface the path it touches. Must pass `debug_assert_safe_targets`.
    struct SafeWriteTool;
    #[async_trait]
    impl Tool for SafeWriteTool {
        fn name(&self) -> &str {
            "safe_write"
        }
        fn description(&self) -> &str {
            "test write tool with a correct scope_target"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("wrote".to_string())
        }
        fn scope_target(&self, arguments: &str) -> ScopeTarget {
            // Surface the path so the scope-gate / broker can check it.
            serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(PathBuf::from))
                .map(ScopeTarget::Path)
                .unwrap_or(ScopeTarget::Unspecified)
        }
        // accesses is conditional in lockstep with scope_target: a write is only
        // declared when there is a real path to write — the realistic shape.
        fn accesses(&self, arguments: &str) -> ToolAccesses {
            match self.scope_target(arguments) {
                ScopeTarget::Path(p) => ToolAccesses::write_file(p.to_string_lossy().into_owned()),
                _ => ToolAccesses::none(),
            }
        }
    }

    /// A *misbehaving* write tool: declares a write but leaves scope_target at
    /// the `Unspecified` default. Must trip `debug_assert_safe_targets`.
    struct UnsafeWriteTool;
    #[async_trait]
    impl Tool for UnsafeWriteTool {
        fn name(&self) -> &str {
            "unsafe_write"
        }
        fn description(&self) -> &str {
            "test write tool that forgets scope_target"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("wrote".to_string())
        }
        // scope_target defaults to Unspecified — the hole.
        fn accesses(&self, _arguments: &str) -> ToolAccesses {
            ToolAccesses::write_file("/sample".to_string())
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn safe_targets_passes_for_correct_write_tool() {
        let tool: Arc<dyn Tool> = Arc::new(SafeWriteTool);
        // Must not panic: the write tool surfaces its target.
        debug_assert_safe_targets(std::slice::from_ref(&tool));
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "would bypass the")]
    fn safe_targets_flags_write_tool_with_unspecified_target() {
        let tool: Arc<dyn Tool> = Arc::new(UnsafeWriteTool);
        // Must panic: the tool declares a write but returns Unspecified, so the
        // write would bypass every permission gate.
        debug_assert_safe_targets(std::slice::from_ref(&tool));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn safe_targets_ignores_read_only_tools() {
        // A read-only tool (no declared write) may legitimately use the
        // Unspecified default — its auto-admit path.
        let tool: Arc<dyn Tool> = Arc::new(PingTool);
        debug_assert_safe_targets(std::slice::from_ref(&tool));
    }

    #[test]
    fn collect_toolset_includes_registered_and_skips_declined() {
        let ctx = ToolContextBuilder::new().build();
        let toolset = collect_toolset(&ctx);
        let names: Vec<&str> = toolset
            .capability_names()
            .filter(|name| name.starts_with("registry_"))
            .collect();
        assert!(names.contains(&"registry_ping"));
        assert!(!names.contains(&"registry_declined"));
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "duplicate builtin tool registration")]
    fn duplicate_identity_panics_in_debug() {
        // Same name AND same (default) variant => a true identity collision.
        debug_assert_unique_identities(&[
            Arc::new(PingTool) as Arc<dyn Tool>,
            Arc::new(PingTool) as Arc<dyn Tool>,
        ]);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn distinct_variants_are_not_a_collision() {
        // Same name but different variant is the legitimate variant mechanism,
        // not a double-register; the guard must allow it.
        debug_assert_unique_identities(&[
            Arc::new(VariantTool {
                variant: "default",
                desc: "default impl",
            }) as Arc<dyn Tool>,
            Arc::new(VariantTool {
                variant: "terse",
                desc: "terse impl",
            }) as Arc<dyn Tool>,
        ]);
    }

    /// Two variants of one capability: `name()` shared, `variant()` distinct.
    struct VariantTool {
        variant: &'static str,
        desc: &'static str,
    }
    #[async_trait]
    impl Tool for VariantTool {
        fn name(&self) -> &str {
            "cap"
        }
        fn variant(&self) -> &str {
            self.variant
        }
        fn description(&self) -> &str {
            self.desc
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok(String::new())
        }
    }

    fn toolset_with_variants() -> ToolSet {
        ToolSet::from_tools(vec![
            Arc::new(VariantTool {
                variant: "default",
                desc: "default impl",
            }) as Arc<dyn Tool>,
            Arc::new(VariantTool {
                variant: "terse",
                desc: "terse impl",
            }) as Arc<dyn Tool>,
        ])
    }

    #[test]
    fn from_tools_groups_variants_under_one_capability() {
        let toolset = toolset_with_variants();
        assert_eq!(toolset.len(), 1);
        let cap = toolset.variants_of("cap").unwrap();
        assert_eq!(cap.default_variant(), "default");
        let ids: Vec<&str> = cap.variant_ids().collect();
        assert_eq!(ids, vec!["default", "terse"]);
    }

    #[test]
    fn resolve_picks_selected_variant_else_default() {
        let toolset = toolset_with_variants();

        // No selection → default variant.
        let resolved = toolset.resolve(empty_variant_selection());
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].description(), "default impl");

        // Selection names the terse variant → terse impl.
        let mut selection = VariantSelection::new();
        selection.insert("cap".to_string(), "terse".to_string());
        let resolved = toolset.resolve(&selection);
        assert_eq!(resolved[0].description(), "terse impl");

        // Selection names a missing variant → falls back to default.
        let mut selection = VariantSelection::new();
        selection.insert("cap".to_string(), "nope".to_string());
        let resolved = toolset.resolve(&selection);
        assert_eq!(resolved[0].description(), "default impl");
    }

    #[test]
    fn upsert_replaces_one_concrete_identity() {
        let mut toolset = toolset_with_variants();
        let replaced = toolset.upsert(Arc::new(VariantTool {
            variant: "default",
            desc: "agent-owned impl",
        }));

        assert_eq!(
            replaced.as_deref().map(Tool::description),
            Some("default impl")
        );
        assert_eq!(toolset.default_view()[0].description(), "agent-owned impl");
        assert_eq!(
            toolset
                .variants_of("cap")
                .expect("capability should remain installed")
                .variant_or_default(Some("terse"))
                .description(),
            "terse impl"
        );
    }

    #[test]
    fn default_falls_back_to_smallest_id_when_no_default_named() {
        let toolset = ToolSet::from_tools(vec![
            Arc::new(VariantTool {
                variant: "zulu",
                desc: "z",
            }) as Arc<dyn Tool>,
            Arc::new(VariantTool {
                variant: "alpha",
                desc: "a",
            }) as Arc<dyn Tool>,
        ]);
        let cap = toolset.variants_of("cap").unwrap();
        assert_eq!(cap.default_variant(), "alpha");
    }

    /// A tool with a configurable name, variant, and vision requirement, for
    /// exercising `resolve_for`'s scope/override/model-capability composition.
    struct CapTool {
        name: &'static str,
        variant: &'static str,
        requires_vision: bool,
    }
    #[async_trait]
    impl Tool for CapTool {
        fn name(&self) -> &str {
            self.name
        }
        fn variant(&self) -> &str {
            self.variant
        }
        fn description(&self) -> &str {
            self.variant
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn requires_vision(&self) -> bool {
            self.requires_vision
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok(String::new())
        }
    }

    fn cap(name: &'static str, variant: &'static str, requires_vision: bool) -> Arc<dyn Tool> {
        Arc::new(CapTool {
            name,
            variant,
            requires_vision,
        })
    }

    /// A test model with vision on/off (other fields are irrelevant to the
    /// resolver).
    fn model(vision: bool) -> Model {
        Model {
            id: "test",
            family: "test",
            context_window: 100_000,
            thinking: crate::reasoning::ReasoningSupport::None,
            tool_call: true,
            vision,
            protocol: crate::WireProtocol::ChatCompletions,
            model_guidance: "",
            effort_levels: &[],
        }
    }

    #[test]
    fn tool_scope_intersect_is_a_meet() {
        // All is the identity.
        let only = ToolScope::only(["a", "b"]);
        assert_eq!(ToolScope::All.intersect(&only), only);
        assert_eq!(only.intersect(&ToolScope::All), only);
        // Two Only sets intersect.
        let other = ToolScope::only(["b", "c"]);
        assert_eq!(only.intersect(&other), ToolScope::only(["b"]));
        // Commutative.
        assert_eq!(only.intersect(&other), other.intersect(&only));
    }

    #[test]
    fn resolve_for_intersects_agent_and_model_scope() {
        let pool = ToolSet::from_tools(vec![
            cap("read_text", "default", false),
            cap("search_text", "default", false),
            cap("write_file", "default", false),
        ]);
        // Agent admits read_text + write_file; model admits everything (All).
        let agent = ToolSelection::only(["read_text", "write_file"]);
        let model_sel = ToolSelection::unrestricted();
        let resolved = pool.resolve_for(&model(true), &agent, &model_sel);
        let names: Vec<&str> = resolved.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["read_text", "write_file"]);
    }

    #[test]
    fn resolve_for_drops_vision_tool_on_text_only_model() {
        // read_image is a single-variant, vision-only capability.
        let pool = ToolSet::from_tools(vec![
            cap("read_text", "default", false),
            cap("read_image", "default", true),
        ]);
        let agent = ToolSelection::unrestricted();
        let model_sel = ToolSelection::unrestricted();

        // Vision model: both survive.
        let resolved = pool.resolve_for(&model(true), &agent, &model_sel);
        let names: Vec<&str> = resolved.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["read_image", "read_text"]);

        // Text-only model: read_image is dropped entirely — the capability has
        // no model-usable variant.
        let resolved = pool.resolve_for(&model(false), &agent, &model_sel);
        let names: Vec<&str> = resolved.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["read_text"]);
    }

    #[test]
    fn resolve_for_model_falls_back_to_usable_variant_over_unusable_default() {
        // One capability with a vision default + a text-only variant: a
        // text-only model must get the usable variant, not lose the capability.
        let pool = ToolSet::from_tools(vec![
            cap("describe", "default", true), // vision default
            cap("describe", "text", false),   // text-only fallback
        ]);
        let agent = ToolSelection::unrestricted();
        let model_sel = ToolSelection::unrestricted();

        // Text-only model: default is unusable, so the resolver picks `text`.
        let resolved = pool.resolve_for(&model(false), &agent, &model_sel);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].variant(), "text");

        // Vision model: keeps the default.
        let resolved = pool.resolve_for(&model(true), &agent, &model_sel);
        assert_eq!(resolved[0].variant(), "default");
    }

    #[test]
    fn resolve_for_agent_override_beats_model_override() {
        // Capability with two model-usable variants. Agent pins `terse`, model
        // pins `verbose`; agent precedence must win.
        let pool = ToolSet::from_tools(vec![
            cap("read_text", "default", false),
            cap("read_text", "terse", false),
            cap("read_text", "verbose", false),
        ]);
        let mut agent = ToolSelection::unrestricted();
        agent
            .variants
            .insert("read_text".to_string(), "terse".to_string());
        let mut model_sel = ToolSelection::unrestricted();
        model_sel
            .variants
            .insert("read_text".to_string(), "verbose".to_string());

        let resolved = pool.resolve_for(&model(true), &agent, &model_sel);
        assert_eq!(resolved[0].variant(), "terse");
    }

    #[test]
    fn resolve_for_model_override_fills_unpinned_capability() {
        // Agent pins nothing; the model's override supplies the variant.
        let pool = ToolSet::from_tools(vec![
            cap("read_text", "default", false),
            cap("read_text", "verbose", false),
        ]);
        let agent = ToolSelection::unrestricted();
        let mut model_sel = ToolSelection::unrestricted();
        model_sel
            .variants
            .insert("read_text".to_string(), "verbose".to_string());

        let resolved = pool.resolve_for(&model(true), &agent, &model_sel);
        assert_eq!(resolved[0].variant(), "verbose");
    }

    #[test]
    fn resolve_for_model_cannot_be_overridden_into_an_unusable_variant() {
        // The hard-limit invariant: even if the agent explicitly pins the
        // vision variant, a text-only model never receives it.
        let pool = ToolSet::from_tools(vec![
            cap("describe", "text", false),
            cap("describe", "vision", true),
        ]);
        let mut agent = ToolSelection::unrestricted();
        agent
            .variants
            .insert("describe".to_string(), "vision".to_string());
        let model_sel = ToolSelection::unrestricted();

        let resolved = pool.resolve_for(&model(false), &agent, &model_sel);
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved[0].variant(),
            "text",
            "agent override must not reinstate a model-unusable variant"
        );
    }

    #[test]
    fn context_provides_and_looks_up_by_type() {
        let mut builder = ToolContextBuilder::new();
        builder.provide(42_u32).provide(String::from("hi"));
        let ctx = builder.build();
        assert_eq!(ctx.get::<u32>(), Some(&42));
        assert_eq!(ctx.get::<String>().map(String::as_str), Some("hi"));
        assert_eq!(ctx.get::<bool>(), None);
    }

    #[test]
    fn shared_clones_an_arc_out() {
        let mut builder = ToolContextBuilder::new();
        builder.provide(Arc::new(7_i32));
        let ctx = builder.build();
        let handle: Arc<i32> = ctx.shared::<i32>().unwrap();
        assert_eq!(*handle, 7);
        assert!(ctx.shared::<u64>().is_none());
    }
    #[test]
    fn workspace_roots_admission_and_visibility() {
        use super::WorkspaceRoots;
        use std::path::Path;

        let roots = WorkspaceRoots::new("/proj/frontend", vec!["/proj/backend".into()]);
        // Primary keeps its resolution duties.
        assert_eq!(roots.primary(), Path::new("/proj/frontend"));
        assert!(roots.is_multi_root());
        // Admission spans every root...
        assert!(roots.contains(Path::new("/proj/frontend/src/main.rs")));
        assert!(roots.contains(Path::new("/proj/backend/api.rs")));
        // ...and fails closed elsewhere.
        assert!(!roots.contains(Path::new("/proj/design-kit/logo.png")));
        assert!(!roots.contains(Path::new("/etc/passwd")));
        // Surfacing: primary first, then additional in declaration order.
        let listed: Vec<&Path> = roots.iter().collect();
        assert_eq!(
            listed,
            vec![Path::new("/proj/frontend"), Path::new("/proj/backend")]
        );

        // The single-root construction admits exactly the primary.
        let single = WorkspaceRoots::single("/proj/frontend");
        assert!(!single.is_multi_root());
        assert!(single.contains(Path::new("/proj/frontend/x")));
        assert!(!single.contains(Path::new("/proj/backend/x")));
    }

    #[test]
    fn tool_pool_declaration_and_resolution() {
        let pool = ToolPool::from_tools(vec![
            cap("read_text", "default", false),
            cap("search_text", "default", false),
            cap("write_file", "default", false),
        ]);

        let decl = ToolDeclaration::new(
            "code_analyst",
            crate::AgentKind::Root,
            ToolScope::only(["read_text", "search_text"]),
        );
        pool.declare(decl.clone());

        assert_eq!(pool.get_declaration("code_analyst"), Some(decl));
        assert_eq!(pool.get_declaration("unknown"), None);

        let agent = ToolSelection::only(["read_text", "search_text"]);
        let resolved = pool.resolve_for(&model(true), &agent, &ToolSelection::unrestricted());
        let names: Vec<&str> = resolved.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["read_text", "search_text"]);

        let snapshot = pool.snapshot(&agent, &model(true));
        assert_eq!(snapshot.capabilities.len(), 3);
        let read_audit = snapshot
            .capabilities
            .iter()
            .find(|c| c.capability == "read_text")
            .unwrap();
        assert!(read_audit.requested);
        assert!(read_audit.admitted);
        assert_eq!(read_audit.active_variant.as_deref(), Some("default"));

        let write_audit = snapshot
            .capabilities
            .iter()
            .find(|c| c.capability == "write_file")
            .unwrap();
        assert!(!write_audit.requested);
        assert!(!write_audit.admitted);
    }
}
