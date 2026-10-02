//! SearXNG backend — queries a self-hosted or trusted instance's JSON API.
//! Keyless and fully under the operator's control, making it the recommended
//! backend for users behind censored networks or who want query privacy.

use super::{MOZILLA_UA, ProviderOutput, SearchProvider, SearchResult};
use async_trait::async_trait;

pub(crate) struct SearxngProvider {
    pub url: Option<String>,
}

#[async_trait]
impl SearchProvider for SearxngProvider {
    fn name(&self) -> &'static str {
        "SearXNG"
    }

    fn clone_box(&self) -> Box<dyn SearchProvider> {
        Box::new(Self {
            url: self.url.clone(),
        })
    }

    async fn search(
        &self,
        client: &crate::tools::web::http::WebHttp,
        query: &str,
    ) -> Result<ProviderOutput, String> {
        let base = self
            .url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                "SearXNG backend selected but `[websearch].searxng_url` is not set. \
                 Configure a JSON endpoint, e.g. \"http://localhost:8080/search\"."
                    .to_string()
            })?;
        let url = crate::tools::web::http::with_query(
            base,
            &[
                ("q", query),
                ("format", "json"),
                ("categories", "general"),
                ("pageno", "1"),
            ],
        );
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_static(MOZILLA_UA),
        );
        let response = client
            .get(&url, headers)
            .await
            .map_err(|e| format!("SearXNG request failed: {e}"))?;
        let status = response.status;
        if !status.is_success() {
            return Err(format!("SearXNG returned HTTP {status} for {base}"));
        }
        let body = response.body;
        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| format!("SearXNG returned invalid JSON: {e}"))?;
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
