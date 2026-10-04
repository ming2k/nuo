use std::sync::{OnceLock, RwLock};

use async_trait::async_trait;
use nuo_wire::{SharedWebConfig, Tool, WebRuntimeConfig, WebSearchProvider};
use nuo_tool::ToolSchema;
use serde::Deserialize;

use crate::search::SearchProvider;

#[derive(ToolSchema, Deserialize)]
struct WebSearchArgs {
    #[tool(desc = "The search query")]
    query: String,
}

pub struct WebSearchTool {
    config: SharedWebConfig,
    provider: RwLock<Option<ProviderCache>>,
}

struct ProviderCache {
    revision: u64,
    provider: Box<dyn SearchProvider>,
    client: Result<std::sync::Arc<crate::http::WebHttp>, String>,
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

    pub(crate) fn current_provider(&self) -> Result<ProviderPair, String> {
        let (revision, snapshot) = self.config.snapshot();
        {
            let guard = self
                .provider
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cache) = guard.as_ref()
                && cache.revision == revision
            {
                return Ok((
                    clone_provider(cache.provider.as_ref()),
                    cache.client.clone().map_err(|e| e.clone())?,
                ));
            }
        }
        let provider = crate::search::build_provider(&snapshot);
        let client =
            crate::http::WebHttp::new(&snapshot.behavior).map(std::sync::Arc::new);
        *self
            .provider
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(ProviderCache {
            revision,
            provider: clone_provider(provider.as_ref()),
            client: client.clone(),
        });
        Ok((provider, client.map_err(|e| e.clone())?))
    }

    fn description_text() -> String {
        let year = chrono::Utc::now().format("%Y");
        format!(
            "Search the web for current information, documentation, or events. Current year is {year}."
        )
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
        "search_web"
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
    fn parameters(&self) -> serde_json::Value {
        WebSearchArgs::parameters_schema()
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_structured(arguments)
            .await
            .map(|out| out.to_text())
    }
    async fn call_structured(&self, arguments: &str) -> Result<nuo_wire::ToolOutput, String> {
        let args: WebSearchArgs =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let query = &args.query;
        let (provider, client) = self.current_provider()?;

        let output = provider.search(&client, query).await?;
        let (results, truncated) = match output {
            crate::search::ProviderOutput::Results(results) => {
                crate::search::results_to_hits(results)
            }
            crate::search::ProviderOutput::Blob(text) => {
                crate::search::blob_to_hits(query, provider.name(), &text)
            }
        };
        Ok(nuo_wire::ToolOutput::WebSearch {
            query: query.to_string(),
            provider: provider.name().to_string(),
            results,
            truncated,
        })
    }
}
