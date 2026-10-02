//! Tavily backend — hosted search REST API (requires an API key). A reliable
//! drop-in for users who want a key-based hosted backend rather than the
//! anonymous Exa/Parallel MCP endpoints.

use super::{ProviderOutput, SearchProvider, SearchResult};
use async_trait::async_trait;
use nuo_contracts::TAVILY_SEARCH_ENDPOINT;

pub(crate) struct TavilyProvider {
    pub api_key: Option<String>,
}

#[async_trait]
impl SearchProvider for TavilyProvider {
    fn name(&self) -> &'static str {
        "Tavily"
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
        let key = self
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                "Tavily backend selected but `[websearch].tavily_api_key` is not set.".to_string()
            })?;
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| "Tavily key is not a valid header value".to_string())?,
        );
        let response = client
            .post_json(
                TAVILY_SEARCH_ENDPOINT,
                headers,
                &serde_json::json!({
                    "query": query,
                    "search_depth": "advanced",
                    "include_answer": false,
                    "max_results": 10
                }),
            )
            .await
            .map_err(|e| format!("Tavily request failed: {e}"))?;
        let status = response.status;
        if !status.is_success() {
            return Err(format!(
                "Tavily returned HTTP {status} (check tavily_api_key): {}",
                response.body.chars().take(300).collect::<String>()
            ));
        }
        let body = response.body;
        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| format!("Tavily returned invalid JSON: {e}"))?;
        let results = json
            .get("results")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(parse_item)
            .take(10)
            .collect();
        Ok(ProviderOutput::Results(results))
    }
}

fn parse_item(item: &serde_json::Value) -> Option<SearchResult> {
    let url = item.get("url")?.as_str()?.to_string();
    let title = item
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let snippet = item
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if url.is_empty() || title.trim().is_empty() {
        return None;
    }
    Some(SearchResult {
        title,
        url,
        snippet,
    })
}
