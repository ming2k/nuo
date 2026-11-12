//! Shared routing fixtures: declarations, sandboxes, connections, and mock providers.

use super::*;

pub(crate) fn ensure_test_init() {
    TEST_INIT.call_once(|| {
        nuo_server::provider_registry::init();
    });
}

/// Owned derivation inputs for a test.
///
/// A derivation takes one borrowed bundle. Tests vary one or two stores and keep
/// the rest at their defaults; this owns the bundle, and `inputs()` hands out the
/// borrow. Because a temporary in an expression lives to the end of its
/// statement, `&TestInputs::new(..).inputs()` is a valid call argument.
pub(crate) struct TestInputs {
    cache: RemoteCatalogCache,
    routes: RouteSettingsStore,
    creds: Credentials,
    providers: nuo_persistence::model_providers::ModelProviders,
    credentials: nuo_provider::CredentialHost,
}

impl TestInputs {
    fn new(cache: &RemoteCatalogCache, routes: &RouteSettingsStore, creds: &Credentials) -> Self {
        ensure_test_init();
        Self {
            cache: cache.clone(),
            routes: routes.clone(),
            creds: creds.clone(),
            providers: nuo_persistence::model_providers::ModelProviders::load(),
            credentials: nuo_provider::CredentialHost::file_backed(
                nuo_persistence::paths::get().auth_file(),
                nuo_persistence::paths::get().state_dir.join("machine_id"),
            ),
        }
    }

    fn inputs(&self) -> DerivationInputs<'_> {
        DerivationInputs {
            cache: &self.cache,
            routes: &self.routes,
            creds: &self.creds,
            providers: &self.providers,
            credentials: &self.credentials,
        }
    }
}

/// The sandboxed credential store a test writes through.
///
/// Reads the paths override, so a test that stores a token cannot touch a
/// developer's real `auth.toml`.
pub(crate) fn test_credential_store() -> std::sync::Arc<dyn CredentialStore> {
    std::sync::Arc::new(nuo_provider::FileCredentialStore::new(
        nuo_persistence::paths::get().auth_file(),
    ))
}

/// The host's provider declarations, as a value (ADR-0081 §1).
///
/// Leaked for the same reason `inputs_of` is: the call site stays an expression.
pub(crate) fn declarations() -> &'static nuo_persistence::model_providers::ModelProviders {
    ensure_test_init();
    Box::leak(Box::new(
        nuo_persistence::model_providers::ModelProviders::load(),
    ))
}

/// One-expression derivation inputs.
///
/// Leaks the owned bundle deliberately: a test bundle is tiny and the test
/// process is short-lived, and keeping the call site an expression means the
/// rewrite works in argument position, in a `let`, and inside an assertion
/// alike.
pub(crate) fn inputs_of(
    cache: &RemoteCatalogCache,
    routes: &RouteSettingsStore,
    creds: &Credentials,
) -> DerivationInputs<'static> {
    let owned: &'static TestInputs = Box::leak(Box::new(TestInputs::new(cache, routes, creds)));
    owned.inputs()
}

/// The common shape: defaults for everything except the cache.
#[allow(dead_code)]
pub(crate) fn inputs_with_cache(cache: &RemoteCatalogCache) -> DerivationInputs<'static> {
    // Leaked deliberately: a test bundle is tiny and the process is short-lived,
    // and this keeps every call site a single expression.
    let owned: &'static TestInputs = Box::leak(Box::new(TestInputs::new(
        cache,
        &RouteSettingsStore::default(),
        &Credentials::default(),
    )));
    owned.inputs()
}

/// Sandbox the process-wide XDG roots for the duration of a test. Tests that
/// write the instance store / credentials / catalog cache must bind the
/// returned guard (and its `TempDir`) to `_sandbox` for the whole body;
/// [`nuo_persistence::paths::TestDirsGuard`] holds the shared override lock and
/// clears the roots on drop.
pub(crate) fn sandboxed_paths() -> (nuo_persistence::paths::TestDirsGuard, tempfile::TempDir) {
    ensure_test_init();
    let tmp = tempfile::tempdir().expect("tempdir");
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: tmp.path().join("config"),
        data_dir: tmp.path().join("data"),
        state_dir: tmp.path().join("state"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: None,
    };
    (nuo_persistence::paths::TestDirsGuard::install(dirs), tmp)
}

pub(crate) fn instance(name: &str, provider: Option<&str>) -> Connection {
    ensure_test_init();
    Connection {
        name: name.to_string(),
        provider: provider.unwrap_or("deepseek").to_string(),
        ..Default::default()
    }
}

pub(crate) fn register_mock_provider(
    id: &'static str,
    base_url: &str,
    protocol: WireProtocol,
    models: &'static [&'static str],
    catalog_protocol: nuo_provider::CatalogShape,
) {
    let mut store = nuo_persistence::model_providers::ModelProviders::load();
    store.set_provider(
        id,
        nuo_persistence::model_providers::UserDeclaredProvider {
            label: Some(id.to_string()),
            root_url: base_url.to_string(),
            default_protocol: Some(protocol),
            user_agent: None,
            headers: None,
            catalog: Some(nuo_provider::RemoteCatalogSource::Endpoint(
                catalog_protocol,
            )),
            dialect: None,
            protocol_roots: vec![],
            catalog_root_url: None,
            prompt_cache: None,
        },
    );
    store.get_or_create_mut(id).include = models
        .iter()
        .map(|id| nuo_protocol::DeclaredModel {
            id: id.to_string(),
            ..Default::default()
        })
        .collect();
    nuo_persistence::model_providers::ModelProviders::save(&store).unwrap();
    nuo_provider::sync_user_declared_providers(
        &nuo_persistence::model_providers::ModelProviders::load(),
    )
    .unwrap();
}
