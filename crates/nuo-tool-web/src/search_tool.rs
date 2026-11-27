use serde_json::Value;
use std::sync::{OnceLock, RwLock};

use async_trait::async_trait;
use nuo_protocol::{SharedWebConfig, WebRuntimeConfig, WebSearchProvider};
use nuo_tool::Tool;

use nuo_tool::{BuiltinTool, ToolSchema};
use serde::Deserialize;

use crate::search::SearchProvider;

#[derive(ToolSchema, Deserialize)]
struct WebSearchArgs {
    #[tool(desc = "Search query")]
    query: String,
}

pub struct WebSearchTool {
    config: SharedWebConfig,
    provider: RwLock<Option<ProviderCache>>,
}

struct ProviderCache {
    revision: u64,
    provider: Box<dyn SearchProvider>,
    client: std::sync::Arc<crate::http::WebHttp>,
}

type ProviderPair = (
    Box<dyn SearchProvider>,
    std::sync::Arc<crate::http::WebHttp>,
);

impl WebSearchTool {
    pub fn new() -> Self {
        Self::with_config(WebRuntimeConfig::default())
    }

    pub fn with_config(config: WebRuntimeConfig) -> Self {
        Self::with_shared_config(SharedWebConfig::new(config))
    }

    pub fn with_shared_config(config: SharedWebConfig) -> Self {
        Self {
            config,
            provider: RwLock::new(None),
        }
    }

    pub(crate) fn current_provider(&self) -> ProviderPair {
        let (revision, snapshot) = self.config.snapshot();
        {
            let guard = self
                .provider
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cache) = guard.as_ref()
                && cache.revision == revision
            {
                return (
                    clone_provider(cache.provider.as_ref()),
                    cache.client.clone(),
                );
            }
        }
        let provider = crate::search::build_provider(&snapshot);
        let client = std::sync::Arc::new(crate::http::WebHttp::new(&snapshot.behavior));
        *self
            .provider
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(ProviderCache {
            revision,
            provider: clone_provider(provider.as_ref()),
            client: client.clone(),
        });
        (provider, client)
    }

    fn description_text() -> String {
        let year = jiff::Timestamp::now().strftime("%Y");
        format!("Search the web for current information or documentation (current year: {year}).")
    }
}

fn clone_provider(p: &dyn SearchProvider) -> Box<dyn SearchProvider> {
    p.clone_box()
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        BuiltinTool::SearchWeb.as_str()
    }
    fn aliases(&self) -> &'static [&'static str] {
        BuiltinTool::SearchWeb.aliases()
    }
    fn is_available(&self) -> bool {
        let snapshot = self.config.get();
        match snapshot.behavior.provider {
            WebSearchProvider::Disabled => false,
            WebSearchProvider::Tavily | WebSearchProvider::Bocha => snapshot
                .search_credential
                .as_ref()
                .map(|key| !key.expose_secret().trim().is_empty())
                .unwrap_or(false),
            WebSearchProvider::Searxng => snapshot
                .behavior
                .searxng_url
                .as_deref()
                .is_some_and(|url| !url.trim().is_empty()),
            WebSearchProvider::Exa
            | WebSearchProvider::Parallel
            | WebSearchProvider::DuckDuckGo => true,
        }
    }
    fn description(&self) -> &str {
        static DESC: OnceLock<String> = OnceLock::new();
        DESC.get_or_init(Self::description_text)
    }
    fn timeout(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(30))
    }

    fn parameters_schema(&self) -> serde_json::Value {
        WebSearchArgs::parameters_schema()
    }
    async fn execute(
        &self,
        _ctx: &nuo_tool::ToolContext,
        arguments: Value,
    ) -> Result<nuo_tool::ToolOutput, nuo_tool::ToolError> {
        let args: WebSearchArgs = serde_json::from_value(arguments)
            .map_err(|e| nuo_tool::ToolError::invalid_args(self.name(), e.to_string()))?;
        let query = &args.query;
        let (provider, client) = self.current_provider();

        let output = provider.search(&client, query).await?;
        let (results, truncated) = match output {
            crate::search::ProviderOutput::Results(results) => {
                crate::search::results_to_hits(results)
            }
            crate::search::ProviderOutput::Blob(text) => {
                crate::search::blob_to_hits(query, provider.name(), &text)
            }
        };
        Ok(nuo_tool::ToolOutput::WebSearch {
            query: query.to_string(),
            provider: provider.name().to_string(),
            results,
            truncated,
        })
    }
}
