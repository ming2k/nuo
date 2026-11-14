//! Distributed tool registration.
//!
//! Built-in tools self-register at their definition site via the
//! [`register_tool!`](crate::register_tool!) macro instead of being enumerated
//! by hand at the agent's assembly point. Submissions are collected at runtime
//! by the [`inventory`] crate, so adding a tool is a one-line change in its own
//! module — no central list to keep in sync.
//!
//! Tools that need runtime state (config blobs, shared registries) pull it out
//! of an opaque [`ToolFactoryContext`]: a type-keyed service map. This keeps
//! `nuo-protocol` free of dependencies on the concrete state types, which live
//! in higher crates (e.g. `SkillRegistry` in `nuo-skills`). The map is the
//! only seam.
//!
//! A handful of "meta" tools genuinely cannot self-register — e.g. a subagent
//! dispatch tool that needs a snapshot of the *rest* of the toolset, which is
//! the registry's own output. Those are still assembled explicitly where the
//! dependency is created; the registry collects everything that is
//! self-contained.
use crate::model::Model;
use crate::{VariantSelection, empty_variant_selection};
use nuo_tool::{BuiltinTool, Tool};

use std::any::{Any, TypeId};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// An opaque, type-keyed bag of services handed to each [`ToolFactory`] at
/// build time. Tools look their concrete dependencies up by Rust type. There
/// is intentionally no typed list of fields: that would force `nuo-protocol` to
/// depend on every higher crate's state types. The map is the seam.
///
/// Cheaply [`Clone`](self::ToolFactoryContext#method.clone)-able (shared via `Arc`);
/// build one with [`ToolFactoryContextBuilder`] and freeze it.
#[derive(Clone, Default)]
pub struct ToolFactoryContext {
    services: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl ToolFactoryContext {
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
/// Under the unified session server one process hosts sessions for
/// *many* projects, so the server's own process cwd is whichever directory the
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

impl ToolFactoryContext {
    /// The session's workspace root, when the assembling bootstrap provided
    /// one. `None` in contexts that predate or decline the service (tests,
    /// side sessions); callers treat that as "use the process cwd".
    pub fn workspace_root(&self) -> Option<&std::path::Path> {
        self.get::<WorkspaceRoot>().map(|root| root.0.as_path())
    }

    /// The session's admitted root set, when the bootstrap provided one.
    ///
    /// `None` means the session declared no additional roots (or predates
    /// ); callers fall back to [`Self::workspace_root`]-only
    /// admission, which is the strictly narrower set.
    pub fn workspace_roots(&self) -> Option<&WorkspaceRoots> {
        self.get::<WorkspaceRoots>()
    }
}

/// The session's complete admitted filesystem authority: the
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

/// Builder for [`ToolFactoryContext`]. Provide services by concrete type, then
/// [`build`](Self::build) to freeze an immutable, cheaply cloneable context.
#[derive(Default)]
pub struct ToolFactoryContextBuilder {
    services: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl ToolFactoryContextBuilder {
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
    pub fn build(self) -> ToolFactoryContext {
        ToolFactoryContext {
            services: Arc::new(self.services),
        }
    }
}

/// Build step that turns a [`ToolFactoryContext`] into one tool instance. Each
/// concrete tool registers its own factory (a private unit struct) via
/// [`register_tool!`](crate::register_tool!). Returning `None` lets a factory
/// decline (e.g. a required service is absent or a feature is off); the tool
/// then simply does not appear in the assembled set.
pub trait ToolFactory: Send + Sync {
    fn build(&self, ctx: &ToolFactoryContext) -> Option<Arc<dyn Tool>>;
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
pub fn collect_toolset(ctx: &ToolFactoryContext) -> ToolSet {
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
                !matches!(target, nuo_tool::ScopeTarget::Unspecified),
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

    /// Whether `scope` admits this capability either by its primary name or by any variant's alias.
    pub fn admits(&self, scope: &CapabilityScope) -> bool {
        if scope.admits(self.name()) {
            return true;
        }
        self.variants
            .values()
            .any(|tool| tool.aliases().iter().any(|alias| scope.admits(alias)))
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
            .filter(|cap| cap.admits(&agent.scope) && cap.admits(&model_sel.scope))
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
pub enum CapabilityScope {
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

impl CapabilityScope {
    /// Build an `Only` scope from a list of capability names.
    pub fn only<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        CapabilityScope::Only(names.into_iter().map(Into::into).collect())
    }

    /// Build a scope from an allowlist of capability names or patterns.
    pub fn from_allowlist<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let set: BTreeSet<String> = patterns.into_iter().map(Into::into).collect();
        if set.contains("*") {
            CapabilityScope::All
        } else {
            CapabilityScope::Only(set)
        }
    }

    /// Whether this scope admits the capability `name`.
    pub fn admits(&self, name: &str) -> bool {
        match self {
            CapabilityScope::All => true,
            CapabilityScope::Only(set) => set.iter().any(|pat| {
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

    /// Build a scope from a strongly-typed set of built-in tools.
    pub fn from_builtin<I>(tools: I) -> Self
    where
        I: IntoIterator<Item = BuiltinTool>,
    {
        CapabilityScope::Only(tools.into_iter().map(|t| t.as_str().to_string()).collect())
    }

    /// Whether this scope admits a typed `BuiltinTool` (checking both canonical name and aliases).
    pub fn admits_builtin(&self, tool: BuiltinTool) -> bool {
        self.admits(tool.as_str()) || tool.aliases().iter().any(|alias| self.admits(alias))
    }

    /// Whether this scope admits `tool` by its primary name or any of its compatibility aliases.
    pub fn admits_tool(&self, tool: &dyn Tool) -> bool {
        self.admits(tool.name()) || tool.aliases().iter().any(|alias| self.admits(alias))
    }

    /// The *meet* of two scopes — the set of capabilities **both** admit.
    /// `All` is the identity (`All ∩ x == x`); two `Only` sets intersect.
    /// Commutative and associative, so composition order is irrelevant.
    pub fn intersect(&self, other: &CapabilityScope) -> CapabilityScope {
        match (self, other) {
            (CapabilityScope::All, other) => other.clone(),
            (this, CapabilityScope::All) => this.clone(),
            (CapabilityScope::Only(a), CapabilityScope::Only(b)) => {
                CapabilityScope::Only(a.intersection(b).cloned().collect())
            }
        }
    }
}

/// One party's ask of the [`ToolSet`] pool: a [`CapabilityScope`] (which capabilities)
/// plus a [`VariantSelection`] (which implementation of each). Both the agent
/// identity (principal or subagent role) and the active model express themselves
/// as a `ToolSelection`; [`ToolSet::resolve_for`] composes the two — scope by
/// intersection, variants by agent-over-model precedence — into the live
/// toolset.
#[derive(Debug, Clone, Default)]
pub struct ToolSelection {
    /// Which capabilities this party admits.
    pub scope: CapabilityScope,
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

    /// A selection that admits only capabilities matching the given allowlist patterns.
    pub fn from_allowlist<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            scope: CapabilityScope::from_allowlist(patterns),
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
            scope: CapabilityScope::only(names),
            variants: VariantSelection::new(),
        }
    }

    /// A selection that admits only the strongly-typed built-in tools (default variants).
    pub fn only_builtin<I>(tools: I) -> Self
    where
        I: IntoIterator<Item = BuiltinTool>,
    {
        Self {
            scope: CapabilityScope::from_builtin(tools),
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
    pub scope: CapabilityScope,
}

impl ToolDeclaration {
    pub fn new(agent_name: impl Into<String>, kind: crate::AgentKind, scope: CapabilityScope) -> Self {
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
            let requested = cap.admits(&agent.scope);
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
///   nuo_protocol::register_tool!(ExecuteCommandFactory => ExecuteCommandTool);
///   ```
///
/// - **Context-aware** — `$ctx` (a name *you* choose) binds the build context,
///   which `$build` may then reference. Use `?` / early `return None` to
///   decline (the tool then won't appear in the assembled set):
///   ```ignore
///   nuo_protocol::register_tool!(WebReaderFactory => |ctx| {
///       let cfg = ctx.get::<nuo_protocol::WebSearchConfig>()?.clone();
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
                $ctx: &$crate::tool_registry::ToolFactoryContext,
            ) -> Option<std::sync::Arc<dyn ::nuo_tool::Tool>> {
                let tool: std::sync::Arc<dyn ::nuo_tool::Tool> = std::sync::Arc::new($build);
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
                _ctx: &$crate::tool_registry::ToolFactoryContext,
            ) -> Option<std::sync::Arc<dyn ::nuo_tool::Tool>> {
                let tool: std::sync::Arc<dyn ::nuo_tool::Tool> = std::sync::Arc::new($build);
                Some(tool)
            }
        }
        ::inventory::submit!($crate::tool_registry::ToolRegistration { factory: &$id });
    };
}

#[cfg(test)]
mod tests;
