//! Live remote-catalog sync and the fitted-model overlay.
//!
//! Catalog sync fetches each catalog-capable connection's `GET /models` list
//! live and folds it into the catalog's remote-catalog overlay (ADR-0203):
//! advertised capability fields are trusted per preset and recorded in the
//! per-connection remote-catalog cache. Routes are *derived* from that cache at
//! catalog-build time — nothing here mutates config or the connection store.
//! Transport, status, and schema failures retain the last valid subset. A
//! structurally valid empty result is authoritative and clears the connection.

use super::Stores;
use super::derive::resolve_credential;
use futures::stream::{self, StreamExt};
use nuo_contracts::WireProtocol;
use nuo_persistence::config::{FittedModelInfo, ModelListCacheState, RemoteCatalogCache};
use nuo_persistence::connections::Connections;
use nuo_providers::{
    CatalogShape, ModelProviderSpec, RemoteCatalogOptions, RemoteCatalogRequest,
    RemoteCatalogSource, RemoteCatalogUpdate, model_provider_spec,
};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

const MODEL_LIST_CACHE_TTL_MS: i64 = 5 * 60 * 1000;
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const DISCOVERY_CONCURRENCY: usize = 8;

/// The concrete network source a catalog sync job speaks. Both variants are
/// network feeds normalized to the same [`RemoteCatalogUpdate`]; the axis is
/// *who serves the data*, not the transport.
enum CatalogFetchSource {
    /// The provider's own catalog endpoint (first-party `GET /models`).
    FirstParty {
        protocol: CatalogShape,
        base_url: String,
        client_profile: nuo_contracts::ClientProfile,
        cached_etag: Option<String>,
        /// Whether the shape's catalog authenticates with the dialect's own
        /// request signing (`CatalogAuth::Dialect`). When set, the fetch builds
        /// the dialect signer (it has the resolved bearer) and hands it to the
        /// generic fetcher as data.
        needs_dialect_signing: bool,
        /// The resolved request dimensions that select which catalog the server
        /// returns (Qoder's `scene`), after any connection-level override.
        dimensions: Vec<(String, String)>,
    },
}

impl CatalogFetchSource {
    /// Fingerprint every request attribute that may select a different catalog
    /// representation. Validators and TTLs must never cross this boundary.
    fn identity(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"remote-catalog-request-v1\0");
        match self {
            Self::FirstParty {
                protocol,
                base_url,
                client_profile,
                dimensions,
                ..
            } => {
                digest.update(b"first-party\0");
                digest.update(catalog_shape_id(*protocol).as_bytes());
                digest.update(b"\0");
                if *protocol == CatalogShape::Codex {
                    digest.update(nuo_contracts::client_identity::CODEX_VERSION.as_bytes());
                    digest.update(b"\0");
                }
                digest.update(base_url.as_bytes());
                digest.update(b"\0");
                digest.update(client_profile.user_agent().as_bytes());
                let mut headers = client_profile.headers();
                headers.sort_unstable();
                for (name, value) in headers {
                    digest.update(b"\0");
                    digest.update(name.as_bytes());
                    digest.update(b"\0");
                    digest.update(value.as_bytes());
                }
                // Request dimensions select a different catalog (Qoder's
                // `scene`), so they belong in the identity: two connections
                // that differ only here must never share a validator or TTL.
                let mut dimensions = dimensions.clone();
                dimensions.sort_unstable();
                for (name, value) in dimensions {
                    digest.update(b"\0dim\0");
                    digest.update(name.as_bytes());
                    digest.update(b"\0");
                    digest.update(value.as_bytes());
                }
            }
        }
        format!("sha256:{:x}", digest.finalize())
    }

    fn discard_validator(&mut self) {
        let Self::FirstParty { cached_etag, .. } = self;
        *cached_etag = None;
    }
}

const fn catalog_shape_id(protocol: CatalogShape) -> &'static str {
    match protocol {
        CatalogShape::OpenAi => "openai",
        CatalogShape::Anthropic => "anthropic",
        CatalogShape::Google => "google",
        CatalogShape::GoogleCloudCode => "google-cloud-code",
        CatalogShape::Codex => "codex",
        CatalogShape::OpencodeConsole => "opencode-console",
        CatalogShape::SceneMap => "scene-map",
    }
}

struct CatalogSyncJob {
    connection: nuo_persistence::connections::Connection,
    source: CatalogFetchSource,
    api_key: nuo_contracts::SecretString,
    /// Where the fetch resolves OAuth bearers and signed-catalog identity
    /// (ADR-0303 §1).
    credentials: nuo_providers::CredentialHost,
}

struct CatalogFetchResult {
    connection: nuo_persistence::connections::Connection,
    source_identity: String,
    update: Result<RemoteCatalogUpdate, nuo_providers::ModelListError>,
}

async fn fetch_models(job: CatalogSyncJob) -> CatalogFetchResult {
    let source_identity = job.source.identity();
    match job.source {
        CatalogFetchSource::FirstParty {
            protocol,
            base_url,
            client_profile,
            cached_etag,
            needs_dialect_signing,
            dimensions,
        } => {
            let auth = if job.connection.auth.is_oauth() {
                let source = nuo_providers::oauth::OAuthCredentialSource::new(
                    &job.credentials,
                    &job.connection.name,
                    job.connection.auth.clone(),
                );
                match nuo_contracts::CredentialSource::resolve_auth(&source).await {
                    Ok(auth) => auth,
                    Err(error) => {
                        // A credential that cannot even be resolved is a local
                        // transport-shaped failure, not a server refusal.
                        return CatalogFetchResult {
                            connection: job.connection,
                            source_identity,
                            update: Err(nuo_providers::ModelListError::Http(error)),
                        };
                    }
                }
            } else {
                nuo_contracts::ResolvedAuth::new(job.api_key)
            };
            let extra_headers = client_profile.headers();
            // Build the dialect signer with the freshly-resolved bearer, when
            // the shape needs it. The generic fetcher receives it as data and
            // never names a provider.
            let catalog_signer = if needs_dialect_signing {
                nuo_providers::build_catalog_signer(
                    job.credentials.store().as_ref(),
                    &job.connection.name,
                    auth.token.expose_secret(),
                )
            } else {
                None
            };
            let catalog_signing: Option<&dyn nuo_providers::CatalogSigning> =
                catalog_signer.as_deref();
            let dimensions: Vec<(&str, &str)> = dimensions
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let request = RemoteCatalogRequest {
                protocol,
                base_url: &base_url,
                api_key: &auth.token,
                account_id: auth
                    .extension::<nuo_contracts::ChatGptAuthMetadata>()
                    .map(|m| m.account_id.as_str()),
                org_id: auth
                    .extension::<nuo_contracts::OpencodeAuthMetadata>()
                    .map(|m| m.org_id.as_str()),
                user_agent: Some(client_profile.user_agent()),
                extra_headers: &extra_headers,
                catalog_signing,
                dimensions: &dimensions,
            };
            let options = RemoteCatalogOptions {
                etag: cached_etag.as_deref(),
            };
            let update = nuo_providers::fetch_remote_catalog(request, options).await;
            CatalogFetchResult {
                connection: job.connection,
                source_identity,
                update,
            }
        }
    }
}

/// One connection's result from a live catalog sync pass. Emitted in completion
/// order so the frontend updates per connection without waiting on a slow
/// sibling (ADR-0227).
#[derive(Debug, Clone)]
pub struct ConnectionUpdate {
    /// The connection the update belongs to.
    pub connection: String,
    /// Whether the connection's cached model list or fitted metadata changed.
    pub changed: bool,
    /// The fetch error, when the source could not be refreshed.
    pub error: Option<String>,
    /// Whether [`Self::error`] was a durable upstream **refusal** (`401`/`403`)
    /// rather than a transient failure (ADR-0273). A refusal means the account
    /// was told it may not use the connection; a transient failure means the
    /// network or the server had a bad moment. The notice must not present one
    /// as the other.
    pub refused: bool,
}

/// The result of a live catalog sync pass ([`sync_remote_catalog`]).
#[derive(Debug, Default)]
pub struct CatalogSyncOutcome {
    /// Whether any connection changed its cached model list or fitted metadata.
    pub changed: bool,
    /// Per-connection fetch failures.
    pub failures: Vec<CatalogSyncFailureEntry>,
}

/// One connection's catalog-sync failure, with the durable/transient verdict
/// attached so nothing downstream has to re-derive it from the message text
/// (ADR-0273).
#[derive(Debug, Clone)]
pub struct CatalogSyncFailureEntry {
    pub connection: String,
    pub message: String,
    /// Whether the upstream refused the request (`401`/`403`) rather than
    /// failing to serve it.
    pub refused: bool,
}

/// Fetch every catalog-capable connection's live model list and update the
/// remote-catalog cache. Used by single-shot callers that do not stream per
/// connection.
pub async fn sync_remote_catalog() -> CatalogSyncOutcome {
    sync_catalogs_matching(None, None).await
}

/// Fetch every catalog-capable connection's live model list, emitting a
/// [`ConnectionUpdate`] as each connection's result is applied.
pub async fn sync_remote_catalog_streaming(
    sink: mpsc::UnboundedSender<ConnectionUpdate>,
) -> CatalogSyncOutcome {
    sync_catalogs_matching(None, Some(sink)).await
}

/// Refresh one exact connection. Login and add flows use this path so an
/// unrelated slow provider cannot delay or contaminate their result.
pub async fn sync_connection_catalog(connection_name: &str) -> CatalogSyncOutcome {
    sync_catalogs_matching(Some(connection_name), None).await
}

/// Re-run the catalog sync for one connection when the provider advertises a new
/// catalog ETag on an in-flight response. This is event-initiated, not a
/// scheduled poll (ADR-0227).
pub async fn refresh_connection_models_for_etag(
    connection_name: &str,
    advertised_etag: &str,
) -> CatalogSyncOutcome {
    let cached = RemoteCatalogCache::load();
    let connections = Connections::load();
    let Some(connection) = connections.get(connection_name) else {
        return CatalogSyncOutcome::default();
    };
    let Some(spec) = model_provider_spec(&connection.provider) else {
        return CatalogSyncOutcome::default();
    };
    let Some(source) =
        catalog_fetch_source(super::credential_host().store().as_ref(), connection, &cached, &spec)
    else {
        return CatalogSyncOutcome::default();
    };
    let expected_source_identity = source.identity();
    let Some(state) = cached.model_lists.get(connection_name) else {
        return sync_connection_catalog(connection_name).await;
    };
    if state.etag.as_deref() != Some(advertised_etag)
        || state.client_version != CLIENT_VERSION
        || state.source_identity != expected_source_identity
    {
        return sync_connection_catalog(connection_name).await;
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    if now_ms.saturating_sub(state.refreshed_at_ms) < MODEL_LIST_CACHE_TTL_MS / 2 {
        return CatalogSyncOutcome::default();
    }
    let mut locked = match RemoteCatalogCache::lock().await {
        Ok(lock) => lock,
        Err(error) => {
            return CatalogSyncOutcome {
                changed: false,
                failures: vec![CatalogSyncFailureEntry {
                    connection: connection_name.to_string(),
                    message: error,
                    refused: false,
                }],
            };
        }
    };
    let Some(current) = locked.model_lists.get_mut(connection_name) else {
        drop(locked);
        return sync_connection_catalog(connection_name).await;
    };
    if current.etag.as_deref() != Some(advertised_etag)
        || current.client_version != CLIENT_VERSION
        || current.source_identity != expected_source_identity
    {
        drop(locked);
        return sync_connection_catalog(connection_name).await;
    }
    current.refreshed_at_ms = now_ms;
    match locked.save() {
        Ok(()) => CatalogSyncOutcome::default(),
        Err(error) => CatalogSyncOutcome {
            changed: false,
            failures: vec![CatalogSyncFailureEntry {
                connection: connection_name.to_string(),
                message: error.to_string(),
                refused: false,
            }],
        },
    }
}

async fn sync_catalogs_matching(
    target: Option<&str>,
    sink: Option<mpsc::UnboundedSender<ConnectionUpdate>>,
) -> CatalogSyncOutcome {
    let stores = Stores::load();
    let mut failures: Vec<CatalogSyncFailureEntry> = Vec::new();
    let mut jobs = Vec::new();

    for connection in &stores.connections.connections {
        if target.is_some_and(|target| target != connection.name) {
            continue;
        }
        let Some(spec) = model_provider_spec(&connection.provider) else {
            continue;
        };
        let Some(mut source) =
            catalog_fetch_source(stores.credentials.store().as_ref(), connection, &stores.cache, &spec)
        else {
            continue;
        };
        let source_identity = source.identity();
        let identity_matches =
            stores
                .cache
                .model_lists
                .get(&connection.name)
                .is_some_and(|state| {
                    state.client_version == CLIENT_VERSION
                        && state.source_identity == source_identity
                });
        if !identity_matches {
            // The validator belongs to a different source; never send it.
            source.discard_validator();
        }
        jobs.push(CatalogSyncJob {
            connection: connection.clone(),
            source,
            api_key: resolve_credential(connection, &stores.creds),
            credentials: stores.credentials.clone(),
        });
    }

    if jobs.is_empty() {
        return CatalogSyncOutcome::default();
    }

    let mut fetched = stream::iter(jobs)
        .map(fetch_models)
        .buffer_unordered(DISCOVERY_CONCURRENCY);
    let mut changed = false;

    while let Some(fetched) = fetched.next().await {
        let connection_name = fetched.connection.name.clone();
        // Never merge or resurrect a connection deleted during the network fetch.
        if Connections::load().get(&connection_name).is_none() {
            continue;
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut cache = match RemoteCatalogCache::lock().await {
            Ok(lock) => lock,
            Err(error) => {
                failures.push(CatalogSyncFailureEntry {
                    connection: connection_name.clone(),
                    message: error.clone(),
                    refused: false,
                });
                emit(&sink, &connection_name, false, Some(error), false);
                continue;
            }
        };
        let (connection_changed, error) = apply_fetched(&mut cache, fetched, now_ms);
        if let Err(error) = cache.save() {
            let error = error.to_string();
            failures.push(CatalogSyncFailureEntry {
                connection: connection_name.clone(),
                message: error.clone(),
                refused: false,
            });
            emit(&sink, &connection_name, false, Some(error), false);
            continue;
        }
        drop(cache);
        if connection_changed {
            changed = true;
            tracing::info!(connection = %connection_name, "live catalog sync updated connection");
        }
        let refused = error
            .as_ref()
            .is_some_and(nuo_providers::ModelListError::is_refusal);
        if let Some(error) = &error {
            tracing::warn!(
                connection = %connection_name,
                error = %error,
                refused,
                "live catalog sync failed; keeping previous models"
            );
            failures.push(CatalogSyncFailureEntry {
                connection: connection_name.clone(),
                message: error.to_string(),
                refused,
            });
        }
        emit(
            &sink,
            &connection_name,
            connection_changed,
            error.map(|error| error.to_string()),
            refused,
        );
    }

    CatalogSyncOutcome { changed, failures }
}

/// Fold one connection's fetched result into the remote-catalog cache. A fetch
/// error leaves the existing list untouched (ADR-0227: failure never diminishes
/// a connection).
fn apply_fetched(
    cache: &mut RemoteCatalogCache,
    fetched: CatalogFetchResult,
    now_ms: i64,
) -> (bool, Option<nuo_providers::ModelListError>) {
    let connection = &fetched.connection;
    match fetched.update {
        Ok(RemoteCatalogUpdate::Modified { models, etag }) => {
            let mut changed = false;
            // A model the provider declared unusable registers in the fitted
            // overlay only when the user's own scope injected it — sovereignty
            // over an upstream verdict (ADR-0203 `[INV-CATALOG-04]`, ADR-0273
            // `[INV-AVAIL-05]`).
            let sovereign = connection.models.included_ids();
            let fitted: std::collections::BTreeMap<String, FittedModelInfo> = models
                .iter()
                .filter(|model| {
                    model
                        .availability
                        .as_ref()
                        .is_none_or(nuo_contracts::Availability::is_usable)
                        || sovereign.contains(&model.id)
                })
                .filter(|model| nuo_contracts::model::model_by_id(&model.id).is_none())
                .map(|model| (model.id.clone(), fitted_model_info(model)))
                .collect();
            if cache.fitted_models.get(&connection.name) != Some(&fitted) {
                cache.fitted_models.insert(connection.name.clone(), fitted);
                changed = true;
            }
            // Locked models (declared unusable) stay in the connection's
            // catalog view and metadata so the picker can render them
            // greyed-out with the provider's own reason (official CLI `/model`
            // parity), but they never register in the fitted overlay: the model
            // registry is the inference-capable set, and a locked model must
            // not resolve for inference (ADR-0273).
            let supported: Vec<String> = models.iter().map(|model| model.id.clone()).collect();
            let remote_metadata: std::collections::BTreeMap<String, _> = models
                .iter()
                .map(|model| (model.id.clone(), model.remote_metadata()))
                .collect();
            if cache.remote_metadata.get(&connection.name) != Some(&remote_metadata) {
                cache
                    .remote_metadata
                    .insert(connection.name.clone(), remote_metadata);
                changed = true;
            }
            if cache.connection_models.get(&connection.name) != Some(&supported) {
                cache
                    .connection_models
                    .insert(connection.name.clone(), supported);
                changed = true;
            }
            cache.model_lists.insert(
                connection.name.clone(),
                ModelListCacheState {
                    etag,
                    client_version: CLIENT_VERSION.to_string(),
                    source_identity: fetched.source_identity,
                    refreshed_at_ms: now_ms,
                    // A successful refresh re-verifies every availability
                    // verdict it carried (ADR-0273).
                    refresh_failed: false,
                },
            );
            (changed, None)
        }
        Ok(RemoteCatalogUpdate::NotModified { etag }) => {
            cache.model_lists.insert(
                connection.name.clone(),
                ModelListCacheState {
                    etag,
                    client_version: CLIENT_VERSION.to_string(),
                    source_identity: fetched.source_identity,
                    refreshed_at_ms: now_ms,
                    refresh_failed: false,
                },
            );
            (false, None)
        }
        Err(error) => {
            // The payload is deliberately retained (ADR-0227 / `[INV-CATALOG-03]`),
            // so record that the verdicts inside it are now unverified: a
            // retained `unavailable` may have been reversed upstream
            // (ADR-0273).
            if let Some(state) = cache.model_lists.get_mut(&connection.name) {
                state.refresh_failed = true;
            }
            (false, Some(error))
        }
    }
}

fn emit(
    sink: &Option<mpsc::UnboundedSender<ConnectionUpdate>>,
    connection: &str,
    changed: bool,
    error: Option<String>,
    refused: bool,
) {
    if let Some(sink) = sink {
        let _ = sink.send(ConnectionUpdate {
            connection: connection.to_string(),
            changed,
            error,
            refused,
        });
    }
}

fn catalog_fetch_source(
    credentials: &dyn nuo_providers::oauth::CredentialStore,
    connection: &nuo_persistence::connections::Connection,
    cache: &RemoteCatalogCache,
    spec: &ModelProviderSpec,
) -> Option<CatalogFetchSource> {
    match spec.catalog_source {
        RemoteCatalogSource::Endpoint(protocol) => {
            build_first_party_source(credentials, connection, cache, spec, protocol)
        }
        RemoteCatalogSource::None => None,
    }
}

#[cfg(test)]
pub(super) fn source_identity_for_connection(
    connection: &nuo_persistence::connections::Connection,
    cache: &RemoteCatalogCache,
) -> Option<String> {
    let spec = model_provider_spec(&connection.provider)?;
    catalog_fetch_source(super::credential_host().store().as_ref(), connection, cache, &spec)
        .map(|source| source.identity())
}

/// Build the [`CatalogFetchSource::FirstParty`] variant for a connection,
/// Returns `None` only when the
/// connection's first route cannot be derived (unknown provider or an empty
/// model seed) — a provider's declared `live_catalog` scheme is authoritative,
/// so OAuth providers (ChatGPT Codex, Google Antigravity cloudcode) discover
/// their own first-party catalog exactly like keyed providers do.
#[allow(clippy::too_many_arguments)]
fn build_first_party_source(
    credentials: &dyn nuo_providers::oauth::CredentialStore,
    connection: &nuo_persistence::connections::Connection,
    cache: &RemoteCatalogCache,
    spec: &ModelProviderSpec,
    protocol: CatalogShape,
) -> Option<CatalogFetchSource> {
    // The elected endpoint when the provider's stored identity carries one
    // (Qoder's server-issued region map, §3.1a); every other provider keeps
    // the compiled spec root — the hook returning `None` is the ordinary path.
    let base_url = nuo_providers::catalog_root_for_connection(credentials, &connection.name)
        .unwrap_or_else(|| spec.catalog_root().to_string());
    let client_profile = if connection.client_identity != nuo_contracts::ClientIdentity::Native {
        connection.client_identity.clone()
    } else if spec.default_client_profile != nuo_contracts::ClientPreset::Native {
        nuo_contracts::ClientProfile::from(spec.default_client_profile)
    } else if let Some(user_agent) = spec.user_agent.as_deref() {
        nuo_contracts::ClientProfile::from_user_agent(user_agent)
    } else {
        nuo_contracts::ClientProfile::Native
    };
    let cached_etag = cache
        .model_lists
        .get(&connection.name)
        .and_then(|state| state.etag.clone());
    // The catalog signer is built at fetch time, once the connection's bearer
    // is resolved (the signature embeds it). This only records that the shape
    // needs dialect signing; the fetcher receives the built signer as data.
    let needs_dialect_signing = protocol
        .auth()
        .eq(&nuo_contracts::provider_surface::CatalogAuth::Dialect);
    let dimensions = resolved_catalog_dimensions(connection, protocol);
    Some(CatalogFetchSource::FirstParty {
        protocol,
        base_url,
        client_profile,
        cached_etag,
        needs_dialect_signing,
        dimensions,
    })
}

/// The request dimensions a shape selects its catalog by, after applying any
/// connection-level override.
///
/// A shape declares its defaults ([`CatalogShape::dimensions`]); a connection
/// may override any of them (Qoder's `scene`: `assistant` vs `experts`). The
/// resolved set is part of the catalog identity, so an override caches
/// independently of the default.
fn resolved_catalog_dimensions(
    connection: &nuo_persistence::connections::Connection,
    protocol: CatalogShape,
) -> Vec<(String, String)> {
    let mut dimensions: Vec<(String, String)> = protocol
        .dimensions()
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    for (name, value) in &connection.catalog_dimensions {
        match dimensions.iter_mut().find(|(existing, _)| existing == name) {
            Some(slot) => slot.1 = value.clone(),
            None => dimensions.push((name.clone(), value.clone())),
        }
    }
    dimensions
}

/// Rebuild the fitted-model overlay (`nuo_contracts::model`) from the
/// remote-catalog cache.
pub fn sync_fitted_model_registry() {
    let cache = RemoteCatalogCache::load();
    let connections = Connections::load();
    let fitted: Vec<nuo_contracts::model::FittedModel> = connections
        .connections
        .iter()
        .flat_map(|connection| {
            let spec = model_provider_spec(&connection.provider);
            let fitted_map = cache.fitted_models.get(&connection.name);
            fitted_map.map(|map| {
                let (format, family) = match spec {
                    Some(spec) => (spec.protocol, spec.id.to_string()),
                    None => (WireProtocol::ChatCompletions, connection.provider.clone()),
                };
                map.iter()
                    .map(move |(id, info)| nuo_contracts::model::FittedModel {
                        id: id.clone(),
                        family: family.clone(),
                        context_window: info.context_window,
                        reasoning: info.reasoning,
                        vision: info.vision,
                        protocol: format,
                        effort_levels: info
                            .efforts
                            .iter()
                            .filter_map(|level| match nuo_contracts::Effort::parse(level) {
                                Some(e) => Some(e),
                                None => {
                                    tracing::warn!(
                                        level = level,
                                        model = %id,
                                        "effort tier outside the known vocabulary; \
                                         preserved on the channel but not the static \
                                         baseline"
                                    );
                                    None
                                }
                            })
                            .collect(),
                    })
            })
        })
        .flatten()
        .collect();
    nuo_contracts::model::register_fitted_models(fitted);
}

fn fitted_model_info(model: &nuo_providers::DiscoveredModel) -> FittedModelInfo {
    FittedModelInfo {
        context_window: model.context_window.unwrap_or(0),
        reasoning: model.reasoning.unwrap_or(false),
        vision: model.vision,
        efforts: model.effort_levels.clone().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_providers::DiscoveredModel;

    fn discovered(id: &str, availability: Option<nuo_contracts::Availability>) -> DiscoveredModel {
        DiscoveredModel {
            id: id.to_string(),
            availability,
            advertised: None,
            protocol: None,
            endpoint: None,
            family: Some("qwen".to_string()),
            name: Some(id.to_string()),
            context_window: Some(200_000),
            max_output_tokens: None,
            reasoning: Some(true),
            thinking: Some(nuo_contracts::ReasoningSupport::ReasoningContent),
            tool_call: Some(true),
            vision: Some(true),
            effort_levels: None,
            catalog_source: Some("system".to_string()),
        }
    }

    /// Qoder's server catalog lists subscription-locked models (`enable:false`)
    /// greyed-out in the official `/model` menu. The fold must keep them in the
    /// connection's catalog view (visible, inert) while the fitted overlay —
    /// the inference-capable registry — stays enabled-only.
    #[test]
    fn locked_models_stay_visible_but_do_not_register_for_inference() {
        let mut cache = RemoteCatalogCache::default();
        // Pre-sorted like the generic fetcher emits them (id-ascending).
        let models = vec![
            discovered("gmodel", Some(nuo_contracts::Availability::locked(None))),
            discovered("mmodel", None),
            discovered("qtest-max", Some(nuo_contracts::Availability::usable())),
        ];
        let fetched = CatalogFetchResult {
            connection: nuo_persistence::connections::Connection {
                name: "qoder-test".to_string(),
                ..Default::default()
            },
            source_identity: "sha256:test".to_string(),
            update: Ok(RemoteCatalogUpdate::Modified {
                models,
                etag: Some("\"v1\"".to_string()),
            }),
        };
        let (changed, error) = apply_fetched(&mut cache, fetched, 0);
        assert!(changed);
        assert!(error.is_none());

        let served = &cache.connection_models["qoder-test"];
        assert_eq!(
            served,
            &[
                "gmodel".to_string(),
                "mmodel".to_string(),
                "qtest-max".to_string(),
            ],
            "locked models stay in the picker's catalog view"
        );
        let metadata = &cache.remote_metadata["qoder-test"];
        assert_eq!(
            metadata["gmodel"].availability,
            Some(nuo_contracts::Availability::locked(None)),
            "the lock declaration round-trips to the picker surface"
        );
        assert_eq!(metadata["mmodel"].availability, None);
        let fitted = &cache.fitted_models["qoder-test"];
        assert!(
            !fitted.contains_key("gmodel"),
            "a locked model must never register as inference-capable"
        );
        assert!(fitted.contains_key("qtest-max"));
        assert!(fitted.contains_key("mmodel"));
    }

    /// A user who explicitly injects a provider-declared-unavailable model
    /// overrides the verdict (ADR-0203 `[INV-CATALOG-04]`): it must register as
    /// inference-capable, because sovereignty beats an upstream declaration.
    #[test]
    fn injected_locked_model_registers_for_inference() {
        let mut cache = RemoteCatalogCache::default();
        let mut connection = nuo_persistence::connections::Connection {
            name: "qoder-test".to_string(),
            ..Default::default()
        };
        connection.models.include = vec![nuo_contracts::DeclaredModel {
            id: "gmodel".to_string(),
            ..Default::default()
        }];
        let fetched = CatalogFetchResult {
            connection,
            source_identity: "sha256:test".to_string(),
            update: Ok(RemoteCatalogUpdate::Modified {
                models: vec![discovered(
                    "gmodel",
                    Some(nuo_contracts::Availability::locked(Some(
                        "requires a paid plan".to_string(),
                    ))),
                )],
                etag: Some("\"v1\"".to_string()),
            }),
        };
        let (_changed, error) = apply_fetched(&mut cache, fetched, 0);
        assert!(error.is_none());
        let fitted = &cache.fitted_models["qoder-test"];
        assert!(
            fitted.contains_key("gmodel"),
            "an injected model must override the provider's unavailable verdict"
        );
        // The declaration itself is never rewritten — only overridden.
        assert_eq!(
            cache.remote_metadata["qoder-test"]["gmodel"].availability,
            Some(nuo_contracts::Availability::locked(Some(
                "requires a paid plan".to_string()
            )))
        );
    }

    /// A failed refresh retains the payload (`[INV-CATALOG-03]`) but must record
    /// that the retained availability verdicts are no longer verified, so a
    /// surface cannot present a possibly-reversed verdict as freshly confirmed
    /// (ADR-0273).
    #[test]
    fn failed_refresh_marks_retained_verdicts_stale() {
        let connection = nuo_persistence::connections::Connection {
            name: "qoder-test".to_string(),
            ..Default::default()
        };
        let mut cache = RemoteCatalogCache::default();
        let seeded = CatalogFetchResult {
            connection: connection.clone(),
            source_identity: "sha256:test".to_string(),
            update: Ok(RemoteCatalogUpdate::Modified {
                models: vec![discovered(
                    "gmodel",
                    Some(nuo_contracts::Availability::locked(None)),
                )],
                etag: Some("\"v1\"".to_string()),
            }),
        };
        let (_changed, error) = apply_fetched(&mut cache, seeded, 1);
        assert!(error.is_none());
        assert!(!cache.model_lists["qoder-test"].refresh_failed);

        let failed = CatalogFetchResult {
            connection,
            source_identity: "sha256:test".to_string(),
            update: Err(nuo_providers::ModelListError::Status(
                503,
                "upstream down".to_string(),
            )),
        };
        let (_changed, error) = apply_fetched(&mut cache, failed, 2);
        assert!(error.is_some_and(|error| !error.is_refusal()));
        assert!(
            cache.model_lists["qoder-test"].refresh_failed,
            "the failure must be recorded so the verdict reads as unverified"
        );
        // The verdict is retained, not dropped — and still enforced.
        assert_eq!(
            cache.remote_metadata["qoder-test"]["gmodel"].availability,
            Some(nuo_contracts::Availability::locked(None))
        );
    }
}
