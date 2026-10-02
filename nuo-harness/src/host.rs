//! Host-supplied material the kernel needs but must not resolve itself.
//!
//! The engine executes agents; it does not decide where a product keeps its
//! files or which role declarations a user wrote. Both arrive as ports
//! (ADR-0300 §1, ADR-0303 §1), supplied once at construction and consulted on
//! use. Each has a null implementation whose behaviour is stated rather than
//! implied, so an embedding that supplies nothing gets a documented outcome
//! instead of a silent default.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nuo_contracts::CustomRole;

/// The user's declared roles (`roles.toml` and workspace overrides).
///
/// The kernel switches an agent's role on request (`/role`, a subagent's
/// staffing) and must resolve a *declaration* to do it. Reading the file,
/// merging workspace overrides, and deciding what counts as a valid id are the
/// host's (`muta-persistence::roles`); the kernel only asks by name.
pub trait RoleCatalog: Send + Sync + 'static {
    /// The declaration for `role`, if the user declared one.
    ///
    /// A `None` means "no user declaration" — the caller then falls back to the
    /// built-in roles it ships. It never means "error": a malformed file is the
    /// host's problem to report, not a reason to refuse a role switch.
    fn declaration(&self, role: &str) -> Option<CustomRole>;
}

/// The host's role-scoped dialogue memory (ADR-0248).
///
/// The kernel records what was said and recalls it on request; *where* that
/// memory lives, how it is scored, and whether the host keeps it at all are the
/// host's business. Both directions are best-effort by contract: a host whose
/// memory is unavailable must report the failure, and the kernel's round must
/// not fail because of it.
pub trait RoleMemory: Send + Sync + 'static {
    /// Record one completed user↔role exchange.
    fn record(&self, role: &str, session_id: Option<&str>, prompt: &str, response: &str)
    -> Result<(), String>;

    /// Recall memories for `role` matching `query`.
    fn recall(&self, role: &str, query: &str, limit: usize) -> Result<Vec<RecalledMemory>, String>;
}

/// One recalled memory, as the host hands it back.
///
/// A view type rather than the host's own row: the kernel needs the text, the
/// age, and the score it will show the model, and nothing about how the host
/// computed them.
#[derive(Debug, Clone, PartialEq)]
pub struct RecalledMemory {
    pub role: String,
    pub user_prompt: String,
    pub role_response: String,
    /// Seconds since the exchange, as the host measured it.
    pub age_s: i64,
    /// The host's relevance score, already normalized.
    pub score: f64,
}

/// A host with no dialogue memory: nothing is recorded, nothing is recalled.
///
/// Correct for an embedding that keeps no cross-session state, and honest about
/// the consequence — `recall` returns an empty set, not an error, because "this
/// host has no memory" is a normal condition rather than a failure.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoRoleMemory;

impl RoleMemory for NoRoleMemory {
    fn record(
        &self,
        _role: &str,
        _session_id: Option<&str>,
        _prompt: &str,
        _response: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    fn recall(&self, _role: &str, _query: &str, _limit: usize) -> Result<Vec<RecalledMemory>, String> {
        Ok(Vec::new())
    }
}

/// A host that declares no roles: every lookup misses, so only the kernel's
/// built-in roles are reachable.
///
/// This is the honest outcome for an embedding with no configuration root, and
/// it is what makes the kernel's built-in fallback the *only* path — no file is
/// read and none is assumed.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoDeclaredRoles;

impl RoleCatalog for NoDeclaredRoles {
    fn declaration(&self, _role: &str) -> Option<CustomRole> {
        None
    }
}

/// Where the host keeps per-project state the kernel's guards persist.
///
/// The permission broker remembers a user's "always allow" answers in a
/// per-project file. The kernel owns the *content* of that file (it is the
/// broker's own record) and nothing else about it: not the directory layout, not
/// the naming scheme, not whether the product keeps it on disk at all.
pub trait ProjectPaths: Send + Sync + 'static {
    /// The permission file for `project_root`.
    fn permissions_file(&self, project_root: &Path) -> PathBuf;
}

/// A host with no per-project storage: guards work, nothing is remembered
/// across runs.
///
/// The file path is deliberately *inside the project root* rather than in a
/// temporary directory: it keeps the "no state outside the project" property
/// obvious, and a caller that inspects it sees the truth (a path nobody
/// created) instead of a stray temporary file.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoProjectStorage;

impl ProjectPaths for NoProjectStorage {
    fn permissions_file(&self, project_root: &Path) -> PathBuf {
        project_root.join(".nuo-permissions-not-persisted.json")
    }
}

/// Where a generated session title is published (ADR-0022).
///
/// The titler's only needs from the product are "what is the current title" and
/// "record this one". The session's identity, lineage, and durable store are the
/// product's business, so this is a two-method sink rather than a store trait:
/// a narrow question instead of a container (ADR-0300 §1).
pub trait TitleSink: Send + Sync + 'static {
    /// The current title and whether a human set it.
    ///
    /// A manual title is never overwritten by a generated one, which is why the
    /// flag travels with the value.
    fn title(&self) -> futures::future::BoxFuture<'static, (Option<String>, bool)>;

    /// Record a generated title.
    fn set_generated_title(&self, title: String)
    -> futures::future::BoxFuture<'static, Result<(), String>>;
}

/// A host that keeps no titles: every write is discarded.
///
/// The titler's `title()` re-read is what makes this safe rather than merely
/// inert: a sink that always reports "no title" would let the titler write
/// repeatedly, so this one reports the title it was handed and thereby
/// suppresses the second write. Nothing is durable, and nothing spins.
#[derive(Debug, Default, Clone)]
pub struct NoTitles;

impl TitleSink for NoTitles {
    fn title(&self) -> futures::future::BoxFuture<'static, (Option<String>, bool)> {
        Box::pin(async { (None, false) })
    }

    fn set_generated_title(
        &self,
        _title: String,
    ) -> futures::future::BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// A provider reported that its model catalog moved.
///
/// `ModelCatalogEtag` arrives on the *provider stream* — the kernel sees it, and
/// what it means is entirely the host's business (refresh the catalog, re-derive
/// fitted models, prune stale favourites, or nothing at all). The kernel's part
/// is to notice and to say so; the host's part is the maintenance, which is
/// product policy over product stores (ADR-0300 §1).
pub trait CatalogMaintenance: Send + Sync + 'static {
    /// React to a new catalog version. Returns whether anything changed, which
    /// is what decides whether the host is told to refresh a picker.
    fn catalog_changed(
        &self,
        connection_id: String,
        etag: String,
    ) -> futures::future::BoxFuture<'static, bool>;

    /// The picker snapshot a frontend should now render, if this host has one.
    ///
    /// `None` means "this host publishes no picker" — an embedding with no model
    /// picker UI. The kernel forwards whatever the host says and never assembles
    /// a snapshot itself: a picker is a product surface over product stores.
    fn picker_snapshot(&self) -> Option<nuo_contracts::ProviderPickerSnapshot> {
        None
    }
}

/// A host that does no catalog maintenance: the notice is acknowledged and
/// dropped.
///
/// Correct for an embedding whose provider catalogs are fixed, and honest: the
/// kernel still emits the change signal it observed, and nothing pretends a
/// refresh happened.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoCatalogMaintenance;

impl CatalogMaintenance for NoCatalogMaintenance {
    fn catalog_changed(
        &self,
        _connection_id: String,
        _etag: String,
    ) -> futures::future::BoxFuture<'static, bool> {
        Box::pin(async { false })
    }
}

/// The ports one agent runs with.
///
/// Cloned into every agent the host builds, so an embedding states its wiring
/// once. Both defaults are explicit nulls; see each type for what that means.
#[derive(Clone)]
pub struct KernelHost {
    roles: Arc<dyn RoleCatalog>,
    project_paths: Arc<dyn ProjectPaths>,
    memory: Arc<dyn RoleMemory>,
    catalog: Arc<dyn CatalogMaintenance>,
}

impl KernelHost {
    pub fn new(roles: Arc<dyn RoleCatalog>, project_paths: Arc<dyn ProjectPaths>) -> Self {
        Self {
            roles,
            project_paths,
            memory: Arc::new(NoRoleMemory),
            catalog: Arc::new(NoCatalogMaintenance),
        }
    }

    /// Supply the host's catalog maintenance (ADR-0273).
    pub fn with_catalog(mut self, catalog: Arc<dyn CatalogMaintenance>) -> Self {
        self.catalog = catalog;
        self
    }

    /// Supply the host's role-scoped dialogue memory (ADR-0248).
    pub fn with_memory(mut self, memory: Arc<dyn RoleMemory>) -> Self {
        self.memory = memory;
        self
    }

    /// The null host: no declared roles, no per-project storage, no memory.
    pub fn none() -> Self {
        Self {
            roles: Arc::new(NoDeclaredRoles),
            project_paths: Arc::new(NoProjectStorage),
            memory: Arc::new(NoRoleMemory),
            catalog: Arc::new(NoCatalogMaintenance),
        }
    }

    pub fn roles(&self) -> &dyn RoleCatalog {
        self.roles.as_ref()
    }

    pub fn project_paths(&self) -> &dyn ProjectPaths {
        self.project_paths.as_ref()
    }

    /// The project-path policy as a shared handle, for a component that
    /// outlives one borrow (the permission store holds it for its lifetime).
    pub fn project_paths_arc(&self) -> Arc<dyn ProjectPaths> {
        Arc::clone(&self.project_paths)
    }

    /// The role catalog as a shared handle.
    pub fn roles_arc(&self) -> Arc<dyn RoleCatalog> {
        Arc::clone(&self.roles)
    }

    /// The host's dialogue memory.
    pub fn memory(&self) -> &dyn RoleMemory {
        self.memory.as_ref()
    }

    /// The dialogue memory as a shared handle, for a consumer that outlives one
    /// borrow (a tool holds it for its lifetime).
    pub fn memory_arc(&self) -> Arc<dyn RoleMemory> {
        Arc::clone(&self.memory)
    }

    /// The catalog-maintenance policy.
    pub fn catalog(&self) -> &dyn CatalogMaintenance {
        self.catalog.as_ref()
    }
}

impl Default for KernelHost {
    fn default() -> Self {
        Self::none()
    }
}

impl std::fmt::Debug for KernelHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelHost").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_null_host_declares_no_roles() {
        let host = KernelHost::none();
        assert!(host.roles().declaration("reviewer").is_none());
        assert!(NoDeclaredRoles.declaration("anything").is_none());
    }

    #[test]
    fn the_null_host_has_no_memory() {
        let host = KernelHost::none();
        host.memory()
            .record("developer", Some("s"), "q", "a")
            .expect("recording into a memory-less host is a no-op, not a failure");
        assert!(
            host.memory().recall("developer", "q", 5).unwrap().is_empty(),
            "a host with no memory recalls nothing rather than erroring"
        );
    }

    #[test]
    fn the_null_host_persists_permissions_inside_the_project() {
        let host = KernelHost::none();
        let path = host.project_paths().permissions_file(Path::new("/srv/project"));
        assert!(
            path.starts_with("/srv/project"),
            "the null path stays inside the project it belongs to: {path:?}"
        );
    }
}
