//! Parallel hosted search via its MCP endpoint
//! (`https://search.parallel.ai/mcp`). Anonymous use works; an optional
//! `Authorization: Bearer <key>` header routes through the caller's own quota.
//! Like Exa it returns a pre-rendered text blob, passed through verbatim.

use super::{ProviderOutput, SearchProvider, mcp_tools_call};
use async_trait::async_trait;
use nuo_contracts::PARALLEL_SEARCH_ENDPOINT;
const PARALLEL_TOOL: &str = "web_search";

pub(crate) struct ParallelProvider {
    pub api_key: Option<String>,
}

#[async_trait]
impl SearchProvider for ParallelProvider {
    fn name(&self) -> &'static str {
        "Parallel"
    }

    fn clone_box(&self) -> Box<dyn SearchProvider> {
        Box::new(Self {
            api_key: self.api_key.clone(),
        })
    }

    async fn search(
        &self,
        client: &crate::tools::web::http::WebHttp,
        query: &str,
    ) -> Result<ProviderOutput, String> {
        let mut headers: Vec<(String, String)> =
            vec![("User-Agent".to_string(), "nuo/0.1".to_string())];
        if let Some(key) = self
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            headers.push(("Authorization".to_string(), format!("Bearer {key}")));
        }
        let text = mcp_tools_call(
            client,
            PARALLEL_SEARCH_ENDPOINT,
            PARALLEL_TOOL,
            serde_json::json!({
                "objective": query,
                "search_queries": [query],
            }),
            &headers,
        )
        .await?;
        Ok(ProviderOutput::Blob(text))
    }
}
