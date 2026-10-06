//! Provider static specifications, route resolutions, and prompt-cache policies (ADR-0015).

use std::borrow::Cow;
use std::sync::{Arc, RwLock};
use nuo_model_codec::{
    ApiRoot, ClientPreset, Model, PromptCacheCapabilities, PromptCacheSpec, ProviderDialect,
    WireProtocol,
};
pub use nuo_model_codec::RemoteCatalogSource;
pub use nuo_model_codec::provider_surface::ProviderPromptCache;

/// Static specification of a model provider surface.
#[derive(Clone)]
pub struct ModelProviderSpec {
    /// Stable identifier of this service surface (e.g. `"openai"`, `"anthropic"`, `"deepseek"`).
    pub id: Cow<'static, str>,
    /// The connection-level default endpoint this preset's routes reach.
    pub root_url: Cow<'static, str>,
    /// The `User-Agent` header this preset's routes must send.
    pub user_agent: Option<Cow<'static, str>>,
    /// Baseline capability metadata for the models this provider serves.
    pub baselines: &'static [Model],
    /// Exact inference protocol spoken by the preset's default route.
    pub protocol: WireProtocol,
    /// Service behavior inherited independently of model protocol selection.
    pub dialect: ProviderDialect,
    /// Explicit transport endpoints for services whose protocol families use different paths.
    pub protocol_roots: Cow<'static, [(WireProtocol, Cow<'static, str>)]>,
    pub catalog_root_url: Option<Cow<'static, str>>,
    /// The model ids the preset initially seeds, in display/activation order.
    pub models: &'static [&'static str],
    /// Remote model-catalog source layered on top of the compiled baseline.
    pub catalog_source: RemoteCatalogSource,
    /// Factory-recommended client emulation profile.
    pub default_client_profile: ClientPreset,
    /// Whether this provider strictly requires its recommended client profile.
    pub client_profile_sensitive: bool,
    /// Resolve prompt-cache behavior for one exact preset route and model.
    pub prompt_cache: PromptCachePolicy,
    /// Typed binding to the quota/balance implementation this surface uses
    /// (`[INV-PROV-08]`). `None` means the provider exposes no balance query.
    /// Replaces substring matching on the provider id / base URL.
    pub quota: Option<QuotaPort>,
}

/// Typed quota/balance port declared by a provider spec (`[INV-PROV-08]`).
///
/// Dispatch resolves this port from the spec; it never sniffs the provider-id
/// string or the base URL. The concrete implementation lives in the owning
/// provider crate and is called by the composition root.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuotaPort {
    DeepSeekBalance,
    KimiBalance,
    CommandCodeCredits,
    OpenRouterKey,
    Antigravity,
    SiliconFlow,
    Qoder,
}

#[derive(Clone)]
pub enum PromptCachePolicy {
    Compiled(fn(&str) -> PromptCacheSpec),
    Declared(ProviderPromptCache),
}

impl PromptCachePolicy {
    pub fn resolve(&self, model: &str) -> PromptCacheCapabilities {
        match self {
            Self::Compiled(resolve) => resolve(model).materialize(),
            Self::Declared(declaration) => declaration.resolve(model),
        }
    }
}

pub const fn unsupported_prompt_cache(_: &str) -> PromptCacheSpec {
    PromptCacheSpec::UNSUPPORTED
}

impl ModelProviderSpec {
    pub fn validate(&self) -> Result<(), String> {
        ApiRoot::parse(&self.root_url)?;
        if !self.dialect.supports(self.protocol) {
            return Err(format!(
                "provider `{}` has an incompatible default protocol",
                self.id
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for (wire, root) in self.protocol_roots.iter() {
            if !seen.insert(*wire) {
                return Err(format!("duplicate protocol root for {wire}"));
            }
            ApiRoot::parse(root)?;
        }
        if let Some(root) = &self.catalog_root_url {
            ApiRoot::parse(root)?;
        }
        Ok(())
    }

    pub fn model_protocol(&self, model_id: &str) -> WireProtocol {
        self.baselines
            .iter()
            .find(|model| model.id == model_id)
            .map(|model| model.protocol)
            .unwrap_or(self.protocol)
    }

    pub fn endpoint(&self, protocol: WireProtocol) -> Result<String, String> {
        let root = self
            .protocol_roots
            .iter()
            .find(|(wire, _)| *wire == protocol)
            .map(|(_, root)| root.as_ref())
            .unwrap_or(&self.root_url);
        let root = ApiRoot::parse(root)?;
        Ok(endpoint_for(self.dialect, &root, protocol))
    }

    pub fn catalog_root(&self) -> &str {
        self.catalog_root_url.as_deref().unwrap_or(&self.root_url)
    }
}

/// Suffix algebra resolving an API root to the transport endpoint.
pub fn endpoint_for(
    dialect: ProviderDialect,
    root: &ApiRoot,
    protocol: WireProtocol,
) -> String {
    match (protocol, dialect) {
        (WireProtocol::GoogleGemini, _) | (_, ProviderDialect::Qoder) => root.as_str().to_string(),
        (WireProtocol::ChatCompletions, _) => root.append("chat/completions"),
        (WireProtocol::Responses, _) => root.append("responses"),
        (WireProtocol::AnthropicMessages, _) => root.append("messages"),
    }
}

/// Registry storing static and user-declared provider specs.
static SPECS: RwLock<std::collections::BTreeMap<String, Arc<ModelProviderSpec>>> =
    RwLock::new(std::collections::BTreeMap::new());

/// Register a provider specification.
pub fn register_provider_spec(spec: ModelProviderSpec) {
    SPECS
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(spec.id.to_string(), Arc::new(spec));
}

/// Register multiple provider specifications.
pub fn register_provider_specs(specs: impl IntoIterator<Item = ModelProviderSpec>) {
    let mut map = SPECS.write().unwrap_or_else(|e| e.into_inner());
    for spec in specs {
        map.insert(spec.id.to_string(), Arc::new(spec));
    }
}

/// Look up a provider specification by its stable id.
pub fn model_provider_spec(id: &str) -> Option<Arc<ModelProviderSpec>> {
    let canonical = nuo_model_codec::model_providers::canonical_provider_id(id)
        .unwrap_or_else(|| id.to_string());
    let map = SPECS.read().unwrap_or_else(|e| e.into_inner());
    map.get(id).or_else(|| map.get(&canonical)).cloned()
}

/// Resolve route configuration for a given provider and model id.
pub fn route_for_model(
    provider_id: &str,
    model_id: &str,
) -> Option<(WireProtocol, String, Option<Cow<'static, str>>)> {
    let spec = model_provider_spec(provider_id)?;
    let protocol = spec.model_protocol(model_id);
    Some((
        protocol,
        spec.endpoint(protocol).ok()?,
        spec.user_agent.clone(),
    ))
}

/// Synchronize user-declared provider definitions into the provider specifications registry.
pub fn sync_user_declared_providers(
    declared: &impl AsRef<std::collections::BTreeMap<String, nuo_model_codec::model_providers::UserDeclaredProvider>>,
) -> Result<(), String> {
    let mut next = std::collections::BTreeMap::new();
    for (id, provider) in declared.as_ref() {
        provider.validate(id)?;
        let wire = provider
            .default_protocol
            .unwrap_or(nuo_model_codec::WireProtocol::ChatCompletions);
        let dialect = provider.dialect.unwrap_or_default();
        let catalog_source = provider.catalog.clone().unwrap_or_else(|| {
            RemoteCatalogSource::Endpoint(match dialect {
                nuo_model_codec::ProviderDialect::Antigravity => nuo_model_codec::CatalogShape::GoogleCloudCode,
                nuo_model_codec::ProviderDialect::ChatGpt => nuo_model_codec::CatalogShape::Codex,
                _ => nuo_model_codec::CatalogShape::from_wire_protocol(wire),
            })
        });
        let spec = ModelProviderSpec {
            id: Cow::Owned(id.clone()),
            baselines: &[],
            root_url: Cow::Owned(provider.root_url.clone()),
            user_agent: provider.user_agent.clone().map(Cow::Owned),
            protocol: wire,
            dialect,
            protocol_roots: Cow::Owned(
                provider
                    .protocol_roots
                    .iter()
                    .map(|(wire, root)| (*wire, Cow::Owned(root.clone())))
                    .collect(),
            ),
            catalog_root_url: provider.catalog_root_url.clone().map(Cow::Owned),
            models: &[],
            catalog_source,
            default_client_profile: provider
                .client_profile
                .unwrap_or(nuo_model_codec::ClientPreset::Native),
            client_profile_sensitive: provider.client_profile_sensitive,
            prompt_cache: PromptCachePolicy::Declared(provider.prompt_cache.clone().unwrap_or_default()),
            quota: None,
        };
        spec.validate()?;
        next.insert(id.clone(), Arc::new(spec));
    }
    let mut map = SPECS.write().unwrap_or_else(|e| e.into_inner());
    for (id, spec) in next {
        map.insert(id, spec);
    }
    Ok(())
}
