//! Authoritative query/update boundary for the singleton web-tool configuration.

use nuo_wire::{
    AgentResponse, SecretString, SharedWebConfig, WebConfigUpdate, WebConfigView,
    WebEndpointRequirement, WebProviderAxis, web_provider_capabilities,
};
use nuo_persistence::config::{Config, Credentials, resolve_web_config};
use tokio::sync::mpsc;

fn view(config: &Config, shared: &SharedWebConfig) -> WebConfigView {
    let resolved = resolve_web_config(&config.web, &Credentials::load());
    WebConfigView {
        revision: shared.revision(),
        provider: config.web.provider,
        reader: config.web.reader,
        timeout_secs: config.web.timeout_secs,
        searxng_url: config.web.searxng_url.clone(),
        search_credential: resolved.search_credential,
        reader_credential: resolved.reader_credential,
        capabilities: web_provider_capabilities(),
    }
}

pub fn query(
    config: &Config,
    shared: &SharedWebConfig,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    let _ = resp_tx.send(AgentResponse::WebSearchConfigSnapshot(view(config, shared)));
}

fn validate_url(label: &str, value: &str) -> Result<(), String> {
    // Validate through the same URL parser the egress uses, so a value that
    // passes here cannot fail at request time. It accepts only http/https.
    netune::Target::from_url(value)
        .map(|_| ())
        .map_err(|error| format!("Invalid {label}: {error}"))
}

fn validate_credential_target(axis: WebProviderAxis, provider_id: &str) -> Result<(), String> {
    let known = web_provider_capabilities()
        .into_iter()
        .any(|capability| capability.axis == axis && capability.id == provider_id);
    if known {
        Ok(())
    } else {
        Err(format!(
            "Unsupported {:?} credential target `{provider_id}`; choose an advertised provider",
            axis
        ))
    }
}

/// Apply one optimistic patch. Incomplete provider setup is representable:
/// selecting a provider before entering its required token/endpoint persists
/// the selection but leaves the tool unavailable, with readiness exposed in
/// the returned view.
pub async fn update(
    config: &mut Config,
    shared: &SharedWebConfig,
    update: WebConfigUpdate,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    let expected = update.expected_revision;
    if expected != shared.revision() {
        let _ = resp_tx.send(AgentResponse::Error(format!(
            "Web settings changed concurrently (expected revision {expected}, current {}). Refresh and retry.",
            shared.revision()
        )));
        return;
    }

    let changes_behavior = update.provider.is_some()
        || update.reader.is_some()
        || update.timeout_secs.is_some()
        || update.searxng_url.is_some();
    if changes_behavior && update.credential.is_some() {
        let _ = resp_tx.send(AgentResponse::Error(
            "A web update may change behavior or one credential, not both; refresh and send two revisioned updates".into(),
        ));
        return;
    }
    if !changes_behavior && update.credential.is_none() {
        let _ = resp_tx.send(AgentResponse::Error(
            "Empty web configuration update".into(),
        ));
        return;
    }

    if let Some(credential) = update.credential {
        let provider_id = credential.provider_id.trim().to_ascii_lowercase();
        if let Err(error) = validate_credential_target(credential.axis, &provider_id) {
            let _ = resp_tx.send(AgentResponse::Error(error));
            return;
        }
        let mut credentials = Credentials::load();
        let value = credential.value.trim();
        credentials.set_web_credential(
            credential.axis,
            &provider_id,
            (!value.is_empty()).then(|| SecretString::new(value.to_string())),
        );
        if let Err(error) = credentials.save() {
            let _ = resp_tx.send(AgentResponse::Error(format!(
                "Could not save web credential: {error}"
            )));
            return;
        }
        let resolved = resolve_web_config(&config.web, &credentials);
        shared.replace(resolved.runtime);
        let _ = resp_tx.send(AgentResponse::WebSearchConfigUpdated(view(config, shared)));
        return;
    }

    let mut next = config.web.clone();
    if let Some(provider) = update.provider {
        next.provider = provider;
    }
    if let Some(reader) = update.reader {
        next.reader = reader;
    }
    if let Some(timeout) = update.timeout_secs {
        next.timeout_secs = timeout.max(1);
    }
    if let Some(endpoint) = update.searxng_url.as_deref().map(str::trim) {
        if !endpoint.is_empty()
            && let Err(error) = validate_url("SearXNG endpoint", endpoint)
        {
            let _ = resp_tx.send(AgentResponse::Error(error));
            return;
        }
        next.searxng_url = (!endpoint.is_empty()).then(|| endpoint.to_string());
    }

    // Capabilities with user-supplied endpoints remain selected while incomplete;
    // the frontend can therefore guide the user to fill the required field.
    if let Some(capability) = next.provider.capability()
        && capability.endpoint == WebEndpointRequirement::UserSupplied
        && let Some(endpoint) = next.searxng_url.as_deref()
        && let Err(error) = validate_url("web search endpoint", endpoint)
    {
        let _ = resp_tx.send(AgentResponse::Error(error));
        return;
    }

    let previous = std::mem::replace(&mut config.web, next);
    if let Err(error) = config.save_preserving_connection_selection() {
        config.web = previous;
        let _ = resp_tx.send(AgentResponse::Error(format!(
            "Could not save [web] to config.toml: {error}"
        )));
        return;
    }

    let resolved = resolve_web_config(&config.web, &Credentials::load());
    shared.replace(resolved.runtime);
    let _ = resp_tx.send(AgentResponse::WebSearchConfigUpdated(view(config, shared)));
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use nuo_wire::{
        AgentResponse, WebCredentialStatus, WebCredentialUpdate, WebReaderProvider,
        WebSearchProvider,
    };

    fn sandbox() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = nuo_persistence::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = tempfile::tempdir().unwrap();
        nuo_persistence::paths::set_test_default(Some(nuo_persistence::paths::Dirs {
            config_dir: root.path().join("config"),
            data_dir: root.path().join("data"),
            state_dir: root.path().join("state"),
            cache_dir: root.path().join("cache"),
            runtime_dir: None,
        }));
        std::fs::create_dir_all(&nuo_persistence::paths::get().config_dir).unwrap();
        (root, guard)
    }

    #[test]
    fn capability_catalog_has_no_phantom_reader() {
        let readers: Vec<_> = web_provider_capabilities()
            .into_iter()
            .filter(|capability| capability.axis == WebProviderAxis::Reader)
            .map(|capability| capability.id)
            .collect();
        assert_eq!(readers, [WebReaderProvider::Jina.id()]);
        assert!(WebSearchProvider::Exa.capability().is_some());
    }

    #[tokio::test]
    async fn selection_then_credential_hot_applies_one_revision_at_a_time() {
        let (_root, _guard) = sandbox();
        let mut config = Config::default();
        let shared =
            SharedWebConfig::new(resolve_web_config(&config.web, &Credentials::default()).runtime);
        let (tx, mut rx) = mpsc::unbounded_channel();

        update(
            &mut config,
            &shared,
            WebConfigUpdate {
                expected_revision: 0,
                provider: Some(WebSearchProvider::Tavily),
                ..Default::default()
            },
            &tx,
        )
        .await;
        let AgentResponse::WebSearchConfigUpdated(first) = rx.recv().await.unwrap() else {
            panic!("expected update response");
        };
        assert_eq!(first.revision, 1);
        assert_eq!(first.provider, WebSearchProvider::Tavily);
        assert_eq!(
            first.search_credential,
            WebCredentialStatus::RequiredMissing
        );
        assert!(shared.get().search_credential.is_none());

        update(
            &mut config,
            &shared,
            WebConfigUpdate {
                expected_revision: 1,
                credential: Some(WebCredentialUpdate {
                    axis: WebProviderAxis::Search,
                    provider_id: "tavily".into(),
                    value: "tvly-secret".into(),
                }),
                ..Default::default()
            },
            &tx,
        )
        .await;
        let AgentResponse::WebSearchConfigUpdated(second) = rx.recv().await.unwrap() else {
            panic!("expected credential update response");
        };
        assert_eq!(second.revision, 2);
        assert_eq!(second.search_credential, WebCredentialStatus::Stored);
        assert_eq!(
            shared
                .get()
                .search_credential
                .as_ref()
                .map(SecretString::expose_secret),
            Some("tvly-secret")
        );
        assert!(
            !serde_json::to_string(&second)
                .unwrap()
                .contains("tvly-secret")
        );
        nuo_persistence::paths::set_test_default(None);
    }

    #[tokio::test]
    async fn stale_revision_cannot_overwrite_authoritative_state() {
        let (_root, _guard) = sandbox();
        let mut config = Config::default();
        let shared =
            SharedWebConfig::new(resolve_web_config(&config.web, &Credentials::default()).runtime);
        shared.replace(shared.get());
        let (tx, mut rx) = mpsc::unbounded_channel();
        update(
            &mut config,
            &shared,
            WebConfigUpdate {
                expected_revision: 0,
                provider: Some(WebSearchProvider::Bocha),
                ..Default::default()
            },
            &tx,
        )
        .await;
        assert!(matches!(rx.recv().await, Some(AgentResponse::Error(_))));
        assert_eq!(config.web.provider, WebSearchProvider::Exa);
        nuo_persistence::paths::set_test_default(None);
    }

    #[tokio::test]
    async fn mixed_behavior_and_credential_patch_is_rejected_atomically() {
        let (_root, _guard) = sandbox();
        let mut config = Config::default();
        let shared =
            SharedWebConfig::new(resolve_web_config(&config.web, &Credentials::default()).runtime);
        let (tx, mut rx) = mpsc::unbounded_channel();

        update(
            &mut config,
            &shared,
            WebConfigUpdate {
                expected_revision: 0,
                provider: Some(WebSearchProvider::Tavily),
                credential: Some(WebCredentialUpdate {
                    axis: WebProviderAxis::Search,
                    provider_id: "tavily".into(),
                    value: "must-not-persist".into(),
                }),
                ..Default::default()
            },
            &tx,
        )
        .await;

        assert!(matches!(rx.recv().await, Some(AgentResponse::Error(_))));
        assert_eq!(config.web.provider, WebSearchProvider::Exa);
        assert_eq!(shared.revision(), 0);
        assert!(
            Credentials::load()
                .web_credential(WebProviderAxis::Search, "tavily")
                .is_none()
        );
        nuo_persistence::paths::set_test_default(None);
    }
}
