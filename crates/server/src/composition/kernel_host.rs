//! The application plane's kernel host: the product's implementations of the
//! ports the runtime (`nuo-harness`) needs but must not resolve itself.
//!
//! - [`ProductRoles`] resolves a role *declaration* from `roles.toml` plus the
//!   workspace's `.nuo/roles.toml` override, which is the product's file layout
//!   and merge policy.
//!
//! This adapter keeps the kernel's vocabulary free of `roles.toml` and `.nuo`,
//! without adding behaviour.
use std::path::PathBuf;
use std::sync::Arc;

use engine::{CatalogMaintenance, KernelHost, RecalledMemory, RoleCatalog, RoleMemory, TitleSink};
use persistence::roles::RolesConfig;
use persistence::thread::ThreadSession;
use protocol::CustomRole;

/// Role declarations from the global `roles.toml` registry (ADR-0254).
#[derive(Default)]
pub struct ProductRoles;

impl ProductRoles {
    pub fn new() -> Self {
        Self
    }
}

impl RoleCatalog for ProductRoles {
    fn declaration(&self, role: &str) -> Option<CustomRole> {
        RolesConfig::load().get(role).cloned()
    }
}

/// The product's role-scoped dialogue memory (`role_memory.db`).
///
/// Opens the store lazily on first use and keeps the handle, which is what the
/// process-global accessor did — the difference is that the kernel now receives
/// it instead of reaching for it.
#[derive(Default)]
pub struct ProductRoleMemory {
    store: std::sync::OnceLock<persistence::RoleMemoryStore>,
}

impl ProductRoleMemory {
    fn store(&self) -> Result<&persistence::RoleMemoryStore, String> {
        if let Some(store) = self.store.get() {
            return Ok(store);
        }
        let store = persistence::RoleMemoryStore::open_default()
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
/// the one place both types are visible: the kernel names a title
/// port, the store owns the session record, and this impl bridges them without
/// either side learning about the other.
pub struct SessionTitles {
    session: Arc<ThreadSession>,
}

impl SessionTitles {
    pub fn new(session: Arc<ThreadSession>) -> Self {
        Self { session }
    }

    /// As a port handle, for a round context.
    pub fn handle(session: &Arc<ThreadSession>) -> Arc<dyn TitleSink> {
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
            // so the titler's re-read is what protects a human's
            // choice rather than this flag.
            session.set_title(Some(title), false).await
        })
    }
}

/// The product's catalog maintenance.
///
/// The engine sees the provider's `ModelCatalogEtag` and asks; the work —
/// refresh the connection's models, re-derive the fitted-model overlay, prune
/// favourites that no longer exist — is product policy over product stores, so
/// it lives here rather than in the kernel.
pub struct ProductCatalogMaintenance;

impl CatalogMaintenance for ProductCatalogMaintenance {
    fn catalog_changed(
        &self,
        connection_id: String,
        etag: String,
    ) -> futures::future::BoxFuture<'static, bool> {
        Box::pin(async move {
            let outcome =
                model_routing::refresh_connection_models_for_etag(&connection_id, &etag).await;
            if outcome.changed {
                model_routing::sync_fitted_model_registry();
                model_routing::prune_stale_models_on_disk();
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

    fn picker_snapshot(&self) -> Option<protocol::ProviderPickerSnapshot> {
        let config = persistence::config::Config::load();
        let usage = persistence::connection_usage::ConnectionUsage::load();
        Some(crate::handlers::handlers_provider::build_live_picker_state(
            &config, &usage,
        ))
    }
}

/// The host every agent in this process is built with.
///
/// One call site, so an agent cannot be assembled with the null host by
/// accident: the workspace is the only variable.
pub fn kernel_host(_workspace: Option<PathBuf>) -> KernelHost {
    KernelHost::new(Arc::new(ProductRoles::new()))
        .with_memory(Arc::new(ProductRoleMemory::default()))
        .with_catalog(Arc::new(ProductCatalogMaintenance))
}
