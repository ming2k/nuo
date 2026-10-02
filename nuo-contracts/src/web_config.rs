//! Canonical configuration contract for the two web-tool axes.
//!
//! Search and reader are deliberately finite provider selections, not user-created
//! connection instances. Persisted behavior contains no credentials; the runtime
//! receives a resolved snapshot with at most one credential per axis.

use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::SecretString;

pub const EXA_SEARCH_ENDPOINT: &str = "https://mcp.exa.ai/mcp";
pub const PARALLEL_SEARCH_ENDPOINT: &str = "https://search.parallel.ai/mcp";
pub const DUCKDUCKGO_LITE_ENDPOINT: &str = "https://lite.duckduckgo.com/lite/";
pub const DUCKDUCKGO_HTML_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
pub const TAVILY_SEARCH_ENDPOINT: &str = "https://api.tavily.com/search";
pub const BOCHA_SEARCH_ENDPOINT: &str = "https://api.bochaai.com/v1/web-search";
pub const JINA_READER_ENDPOINT: &str = "https://r.jina.ai/";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum WebProviderAxis {
    Search,
    Reader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum WebCredentialRequirement {
    None,
    Optional,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum WebCredentialStatus {
    NotRequired,
    Environment,
    Stored,
    OptionalMissing,
    RequiredMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum WebEndpointRequirement {
    Fixed,
    UserSupplied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct WebProviderCapability {
    pub axis: WebProviderAxis,
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub credential: WebCredentialRequirement,
    pub endpoint: WebEndpointRequirement,
    pub default_endpoint: Option<String>,
    pub default_env_var: Option<String>,
}

macro_rules! provider_enum {
    ($name:ident { $($variant:ident => $id:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ts_rs::TS)]
        #[ts(rename_all = "lowercase")]
        #[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const fn id(self) -> &'static str {
                match self { $(Self::$variant => $id),+ }
            }
            pub const fn as_str(&self) -> &'static str { (*self).id() }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.id()) }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where S: Serializer { serializer.serialize_str(self.id()) }
        }
    };
}

provider_enum!(WebSearchProvider {
    Disabled => "disabled",
    Exa => "exa",
    Parallel => "parallel",
    DuckDuckGo => "duckduckgo",
    Searxng => "searxng",
    Tavily => "tavily",
    Bocha => "bocha",
});

impl FromStr for WebSearchProvider {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" => Ok(Self::Disabled),
            "exa" => Ok(Self::Exa),
            "parallel" => Ok(Self::Parallel),
            "duckduckgo" => Ok(Self::DuckDuckGo),
            "searxng" => Ok(Self::Searxng),
            "tavily" => Ok(Self::Tavily),
            "bocha" => Ok(Self::Bocha),
            other => Err(format!("unsupported web search provider `{other}`")),
        }
    }
}

impl WebSearchProvider {
    /// Parse canonical IDs plus historical config/connection spellings.
    /// Runtime serde and wire requests intentionally use strict `FromStr`.
    pub fn parse_legacy(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::Disabled),
            "exa-default" => Ok(Self::Exa),
            "parallel-default" => Ok(Self::Parallel),
            "ddg" | "duckduckgo-default" => Ok(Self::DuckDuckGo),
            "searxng-default" => Ok(Self::Searxng),
            "tavily-default" => Ok(Self::Tavily),
            "bocha-default" => Ok(Self::Bocha),
            _ => value.parse(),
        }
    }
}

impl<'de> Deserialize<'de> for WebSearchProvider {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

provider_enum!(WebReaderProvider {
    Disabled => "disabled",
    Jina => "jina",
});

impl FromStr for WebReaderProvider {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" => Ok(Self::Disabled),
            "jina" => Ok(Self::Jina),
            other => Err(format!("unsupported web reader provider `{other}`")),
        }
    }
}

impl WebReaderProvider {
    /// Parse canonical IDs plus historical config/connection spellings.
    /// `builtin` was once advertised by a UI but never existed at runtime.
    pub fn parse_legacy(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "builtin" => Ok(Self::Disabled),
            "jina-default" => Ok(Self::Jina),
            _ => value.parse(),
        }
    }
}

impl<'de> Deserialize<'de> for WebReaderProvider {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl WebSearchProvider {
    pub fn capability(self) -> Option<WebProviderCapability> {
        let (display_name, description, credential, endpoint, default_endpoint, env) = match self {
            Self::Disabled => return None,
            Self::Exa => (
                "Exa",
                "Hosted semantic search",
                WebCredentialRequirement::Optional,
                WebEndpointRequirement::Fixed,
                Some(EXA_SEARCH_ENDPOINT),
                Some("EXA_API_KEY"),
            ),
            Self::Parallel => (
                "Parallel",
                "Hosted agent search",
                WebCredentialRequirement::Optional,
                WebEndpointRequirement::Fixed,
                Some(PARALLEL_SEARCH_ENDPOINT),
                Some("PARALLEL_API_KEY"),
            ),
            Self::DuckDuckGo => (
                "DuckDuckGo",
                "Keyless HTML search",
                WebCredentialRequirement::None,
                WebEndpointRequirement::Fixed,
                Some(DUCKDUCKGO_LITE_ENDPOINT),
                None,
            ),
            Self::Searxng => (
                "SearXNG",
                "Self-hosted privacy metasearch",
                WebCredentialRequirement::None,
                WebEndpointRequirement::UserSupplied,
                None,
                None,
            ),
            Self::Tavily => (
                "Tavily",
                "Hosted search for agents",
                WebCredentialRequirement::Required,
                WebEndpointRequirement::Fixed,
                Some(TAVILY_SEARCH_ENDPOINT),
                Some("TAVILY_API_KEY"),
            ),
            Self::Bocha => (
                "Bocha",
                "Hosted AI search",
                WebCredentialRequirement::Required,
                WebEndpointRequirement::Fixed,
                Some(BOCHA_SEARCH_ENDPOINT),
                Some("BOCHA_API_KEY"),
            ),
        };
        Some(WebProviderCapability {
            axis: WebProviderAxis::Search,
            id: self.id().into(),
            display_name: display_name.into(),
            description: description.into(),
            credential,
            endpoint,
            default_endpoint: default_endpoint.map(Into::into),
            default_env_var: env.map(Into::into),
        })
    }
}

impl WebReaderProvider {
    pub fn capability(self) -> Option<WebProviderCapability> {
        match self {
            Self::Disabled => None,
            Self::Jina => Some(WebProviderCapability {
                axis: WebProviderAxis::Reader,
                id: self.id().into(),
                display_name: "Jina Reader".into(),
                description: "Rendered page extraction to Markdown".into(),
                credential: WebCredentialRequirement::Optional,
                endpoint: WebEndpointRequirement::Fixed,
                default_endpoint: Some(JINA_READER_ENDPOINT.into()),
                default_env_var: Some("JINA_API_KEY".into()),
            }),
        }
    }
}

pub fn web_provider_capabilities() -> Vec<WebProviderCapability> {
    [
        WebSearchProvider::Exa,
        WebSearchProvider::Parallel,
        WebSearchProvider::DuckDuckGo,
        WebSearchProvider::Searxng,
        WebSearchProvider::Tavily,
        WebSearchProvider::Bocha,
    ]
    .into_iter()
    .filter_map(WebSearchProvider::capability)
    .chain(
        [WebReaderProvider::Jina]
            .into_iter()
            .filter_map(WebReaderProvider::capability),
    )
    .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    pub provider: WebSearchProvider,
    pub reader: WebReaderProvider,
    pub timeout_secs: u64,
    pub searxng_url: Option<String>,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            provider: WebSearchProvider::Exa,
            reader: WebReaderProvider::Disabled,
            timeout_secs: 20,
            searxng_url: None,
        }
    }
}

/// Backward compatibility alias for [`WebConfig`].
pub type WebSearchConfig = WebConfig;

#[derive(Clone, Default)]
pub struct WebRuntimeConfig {
    pub behavior: WebConfig,
    pub search_credential: Option<SecretString>,
    pub reader_credential: Option<SecretString>,
}

impl fmt::Debug for WebRuntimeConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebRuntimeConfig")
            .field("behavior", &self.behavior)
            .field(
                "search_credential",
                &self.search_credential.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "reader_credential",
                &self.reader_credential.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct SharedWebConfig(Arc<RwLock<VersionedWebConfig>>);

#[derive(Debug, Clone)]
struct VersionedWebConfig {
    revision: u64,
    config: WebRuntimeConfig,
}

impl SharedWebConfig {
    pub fn new(initial: WebRuntimeConfig) -> Self {
        Self(Arc::new(RwLock::new(VersionedWebConfig {
            revision: 0,
            config: initial,
        })))
    }

    pub fn replace(&self, config: WebRuntimeConfig) -> u64 {
        let mut state = self
            .0
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.revision = state.revision.saturating_add(1);
        state.config = config;
        state.revision
    }

    pub fn snapshot(&self) -> (u64, WebRuntimeConfig) {
        let state = self
            .0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (state.revision, state.config.clone())
    }

    pub fn revision(&self) -> u64 {
        self.snapshot().0
    }
    pub fn get(&self) -> WebRuntimeConfig {
        self.snapshot().1
    }
}

impl Default for SharedWebConfig {
    fn default() -> Self {
        Self::new(WebRuntimeConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_aliases_are_canonicalized() {
        assert_eq!(
            WebSearchProvider::parse_legacy("ddg").unwrap(),
            WebSearchProvider::DuckDuckGo
        );
        assert_eq!(
            WebReaderProvider::parse_legacy("builtin").unwrap(),
            WebReaderProvider::Disabled
        );
        assert!(toml::from_str::<WebConfig>("reader = 'builtin'").is_err());
    }

    #[test]
    fn only_implemented_reader_is_advertised() {
        let readers: Vec<_> = web_provider_capabilities()
            .into_iter()
            .filter(|capability| capability.axis == WebProviderAxis::Reader)
            .map(|capability| capability.id)
            .collect();
        assert_eq!(readers, ["jina"]);
    }

    #[test]
    fn capability_endpoints_share_the_runtime_constants() {
        assert_eq!(
            WebSearchProvider::Exa
                .capability()
                .unwrap()
                .default_endpoint
                .as_deref(),
            Some(EXA_SEARCH_ENDPOINT)
        );
        assert_eq!(
            WebSearchProvider::Parallel
                .capability()
                .unwrap()
                .default_endpoint
                .as_deref(),
            Some(PARALLEL_SEARCH_ENDPOINT)
        );
        assert_eq!(
            WebSearchProvider::Bocha
                .capability()
                .unwrap()
                .default_endpoint
                .as_deref(),
            Some(BOCHA_SEARCH_ENDPOINT)
        );
        assert_eq!(
            WebReaderProvider::Jina
                .capability()
                .unwrap()
                .default_endpoint
                .as_deref(),
            Some(JINA_READER_ENDPOINT)
        );
    }

    #[test]
    fn shared_config_revision_advances_on_replace() {
        let shared = SharedWebConfig::default();
        assert_eq!(shared.revision(), 0);
        assert_eq!(shared.replace(shared.get()), 1);
    }
}
