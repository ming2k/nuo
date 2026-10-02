//! The application plane's kernel host: the product's implementations of the
//! ports `muta-agent` needs but must not resolve itself.
//!
//! Two ports, both about durable material the kernel reads or writes without
//! owning (ADR-0300 §1, ADR-0303 §1):
//!
//! - [`ProductRoles`] resolves a role *declaration* from `roles.toml` plus the
//!   workspace's `.nuo/roles.toml` override, which is the product's file layout
//!   and merge policy.
//! - [`ProductProjectPaths`] says where a project's permission file lives, which
//!   is the product's path topology (ADR-0013).
//!
//! Both are thin: they exist to keep the kernel's vocabulary free of the words
//! `roles.toml`, `permissions.json`, and `.nuo`, not to add behaviour.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nuo_harness::{
    CatalogMaintenance, KernelHost, ProjectPaths, RecalledMemory, RoleCatalog, RoleMemory,
    TitleSink,
};
use nuo_contracts::CustomRole;
use nuo_persistence::paths;
use nuo_persistence::roles::RolesConfig;
use nuo_persistence::session::SessionStore;

/// Role declarations from the product's files.
///
/// Resolution follows the product's documented order: the user's `roles.toml`,
/// then the workspace's `.nuo/roles.toml` (and its `[roles.<id>]` table) as an
/// override. The merge, the id validation, and the file locations are all
/// [`RolesConfig`]'s; this type only supplies the workspace the caller is in.
pub struct ProductRoles {
    workspace: Option<PathBuf>,
}

impl ProductRoles {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

impl RoleCatalog for ProductRoles {
    fn declaration(&self, role: &str) -> Option<CustomRole> {
        RolesConfig::load_for_workspace(self.workspace.as_deref())
            .get(role)
            .cloned()
    }
}

/// The product's per-project file layout.
pub struct ProductProjectPaths;

impl ProjectPaths for ProductProjectPaths {
    fn permissions_file(&self, project_root: &Path) -> PathBuf {
        paths::get().project_permissions(project_root)
    }
}

/// The product's role-scoped dialogue memory (`role_memory.db`, ADR-0248).
///
/// Opens the store lazily on first use and keeps the handle, which is what the
/// process-global accessor did — the difference is that the kernel now receives
/// it instead of reaching for it.
#[derive(Default)]
pub struct ProductRoleMemory {
    store: std::sync::OnceLock<nuo_persistence::RoleMemoryStore>,
}

impl ProductRoleMemory {
    fn store(&self) -> Result<&nuo_persistence::RoleMemoryStore, String> {
        if let Some(store) = self.store.get() {
            return Ok(store);
        }
        let store = nuo_persistence::RoleMemoryStore::open_default()
            .map_err(|error| format!("failed to open role memory store: {error}"))?;
        Ok(self.store.get_or_init(|| store))
    }
}

impl RoleMemory for ProductRoleMemory {
    fn record(
        &self,
        role: &str,
        session_id: Option<&str>,
        prompt: &str,
        response: &str,
    ) -> Result<(), String> {
        self.store()?
            .record_dialogue(role, session_id, prompt, response)
            .map(|_| ())
            .map_err(|error| format!("failed to record dialogue: {error}"))
    }

    fn recall(&self, role: &str, query: &str, limit: usize) -> Result<Vec<RecalledMemory>, String> {
        let now_s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or(0);
        self.store()?
            .recall(role, query, limit)
            .map(|memories| {
                memories
                    .into_iter()
                    .map(|memory| RecalledMemory {
                        role: memory.role,
                        user_prompt: memory.user_prompt,
                        role_response: memory.role_response,
                        // The kernel wants an age, not the store's two
                        // timestamps: converting here keeps the host's clock
                        // convention out of the contract.
                        age_s: (now_s - memory.created_at_s).max(0),
                        score: memory.score,
                    })
                    .collect()
            })
            .map_err(|error| format!("failed to recall memory: {error}"))
    }
}

/// The session store as the kernel's title sink.
///
/// The adapter exists here, in the application plane, precisely because it is
/// the one place both types are visible (ADR-0300 §1): the kernel names a title
/// port, the store owns the session record, and this impl bridges them without
/// either side learning about the other.
pub struct SessionTitles {
    session: Arc<SessionStore>,
}

impl SessionTitles {
    pub fn new(session: Arc<SessionStore>) -> Self {
        Self { session }
    }

    /// As a port handle, for a round context.
    pub fn handle(session: &Arc<SessionStore>) -> Arc<dyn TitleSink> {
        Arc::new(Self::new(Arc::clone(session)))
    }
}

impl TitleSink for SessionTitles {
    fn title(&self) -> futures::future::BoxFuture<'static, (Option<String>, bool)> {
        let session = Arc::clone(&self.session);
        Box::pin(async move { session.title().await })
    }

    fn set_generated_title(
        &self,
        title: String,
    ) -> futures::future::BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.session);
        Box::pin(async move {
            // `false` = not a manual title. A non-`NULL` title is terminal
            // (ADR-0186), so the titler's re-read is what protects a human's
            // choice rather than this flag.
            session.set_title(Some(title), false).await
        })
    }
}

/// The product's catalog maintenance (ADR-0273).
///
/// The engine sees the provider's `ModelCatalogEtag` and asks; the work —
/// refresh the connection's models, re-derive the fitted-model overlay, prune
/// favourites that no longer exist — is product policy over product stores, so
/// it lives here rather than in the kernel (ADR-0300 §1).
pub struct ProductCatalogMaintenance;

impl CatalogMaintenance for ProductCatalogMaintenance {
    fn catalog_changed(
        &self,
        connection_id: String,
        etag: String,
    ) -> futures::future::BoxFuture<'static, bool> {
        Box::pin(async move {
            let outcome =
                crate::catalog::refresh_connection_models_for_etag(&connection_id, &etag)
                    .await;
            if outcome.changed {
                crate::catalog::sync_fitted_model_registry();
                crate::catalog::prune_stale_models_on_disk();
            }
            for failure in outcome.failures {
                tracing::warn!(
                    connection_id = %failure.connection,
                    error = %failure.message,
                    refused = failure.refused,
                    "model catalog ETag refresh failed"
                );
            }
            outcome.changed
        })
    }

    fn picker_snapshot(&self) -> Option<nuo_contracts::ProviderPickerSnapshot> {
        let config = nuo_persistence::config::Config::load();
        let usage = nuo_persistence::connection_usage::ConnectionUsage::load();
        Some(crate::catalog::build_picker_state(&config, &usage))
    }
}

/// The host every agent in this process is built with.
///
/// One call site, so an agent cannot be assembled with the null host by
/// accident: the workspace is the only variable.
pub fn kernel_host(workspace: Option<PathBuf>) -> KernelHost {
    KernelHost::new(
        Arc::new(ProductRoles::new(workspace)),
        Arc::new(ProductProjectPaths),
    )
    .with_memory(Arc::new(ProductRoleMemory::default()))
    .with_catalog(Arc::new(ProductCatalogMaintenance))
}
