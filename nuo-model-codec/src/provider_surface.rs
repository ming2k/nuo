//! Provider service roots and catalog declarations shared by configuration and routing.

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ApiRoot(String);

impl ApiRoot {
    pub fn parse(value: &str) -> Result<Self, String> {
        let url = url::Url::parse(value).map_err(|e| format!("invalid API root: {e}"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err("API root must be an absolute HTTP(S) URL".into());
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("API root must not contain credentials, a query, or a fragment".into());
        }
        Ok(Self(url.as_str().trim_end_matches('/').to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Append a protocol-defined relative path without stripping or replacing root segments.
    pub fn append(&self, path: &str) -> String {
        format!("{}/{}", self.0, path)
    }
}

impl TryFrom<String> for ApiRoot {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}
impl From<ApiRoot> for String {
    fn from(value: ApiRoot) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogShape {
    #[serde(rename = "openai")]
    OpenAi,
    Anthropic,
    Google,
    GoogleCloudCode,
    Codex,
    /// OpenCode Console account catalog: `{"config":{"provider":{"opencode":
    /// {"api","npm","models":{…}}}}}` over plain bearer + `x-org-id`
    /// workspace scoping (ADR-0269). The response carries per-model
    /// `provider:{npm,api}` routing overrides, so this catalog is also the
    /// inference routing authority for the account.
    OpencodeConsole,
    /// Qoder's scene-keyed map: `{"<scene>": [{"key": …}]}` over the signed
    /// transport. The catalog rides the same COSY signing as inference, and
    /// `scene` selects which catalog the server returns.
    SceneMap,
}
impl CatalogShape {
    pub fn from_wire_protocol(protocol: crate::WireProtocol) -> Self {
        match protocol {
            crate::WireProtocol::AnthropicMessages => Self::Anthropic,
            crate::WireProtocol::GoogleGemini => Self::Google,
            crate::WireProtocol::ChatCompletions | crate::WireProtocol::Responses => Self::OpenAi,
        }
    }

    /// The catalog path, relative to the provider's API root.
    ///
    /// Under ADR-0259 the endpoint is `root_url + path()`. The path is a
    /// property of the *shape*, so a new provider reusing an existing shape
    /// inherits it with no new code.
    pub fn path(self) -> &'static str {
        match self {
            Self::GoogleCloudCode => "v1internal:fetchAvailableModels",
            Self::OpencodeConsole => "api/config",
            Self::SceneMap => "algo/api/v2/model/list",
            _ => "models",
        }
    }

    /// The path form the request signature covers, when the shape's transport
    /// signs (the URL pathname minus the service's routing prefix, no query).
    /// `None` for shapes on an unsigned transport.
    pub fn signed_path(self) -> Option<&'static str> {
        match self {
            Self::SceneMap => Some("/api/v2/model/list"),
            _ => None,
        }
    }

    /// Fixed query parameters for the catalog request, in wire order.
    pub fn query(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::SceneMap => &[("Encode", "1")],
            _ => &[],
        }
    }

    /// How the catalog request authenticates.
    pub fn auth(self) -> CatalogAuth {
        match self {
            Self::SceneMap => CatalogAuth::Dialect,
            _ => CatalogAuth::Bearer,
        }
    }

    /// Request dimensions that select *which* catalog the server returns.
    ///
    /// These are part of the discovery identity hash, so two connections that
    /// differ only here cache independently. A connection may override them
    /// (e.g. Qoder's `scene`: `assistant` vs `experts`).
    pub fn dimensions(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::SceneMap => &[("scene", "assistant")],
            _ => &[],
        }
    }
}

/// How a catalog request authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogAuth {
    /// Standard bearer token (or no header when keyless).
    Bearer,
    /// No auth (public catalog).
    None,
    /// The dialect's own inference signing (Qoder's COSY bundle). One signing
    /// implementation serves both inference and catalog.
    Dialect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "source", content = "format", rename_all = "kebab-case")]
pub enum RemoteCatalogSource {
    Endpoint(CatalogShape),
    None,
}

/// Declarative cache capabilities, with exact model exceptions over a default.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderPromptCache {
    pub default: Option<crate::PromptCacheCapabilities>,
    pub models: std::collections::BTreeMap<String, crate::PromptCacheCapabilities>,
}
impl ProviderPromptCache {
    pub fn resolve(&self, model: &str) -> crate::PromptCacheCapabilities {
        self.models
            .get(model)
            .or(self.default.as_ref())
            .cloned()
            .unwrap_or_else(crate::PromptCacheCapabilities::unsupported)
    }
    pub fn validate(&self) -> Result<(), String> {
        for capabilities in self.default.iter().chain(self.models.values()) {
            capabilities.validate().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
