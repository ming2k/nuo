//! Bocha backend — hosted AI search REST API (requires an API key). Directly
//! reachable from mainland China networks (unlike Exa/Parallel/Tavily), which
//! makes it a useful key-based provider when proxy access is unavailable.

use super::{ProviderOutput, SearchProvider, SearchResult};
use async_trait::async_trait;
use nuo_wire::BOCHA_SEARCH_ENDPOINT;

pub(crate) struct BochaProvider {
    pub api_key: Option<String>,
}

#[async_trait]
impl SearchProvider for BochaProvider {
    fn name(&self) -> &'static str {
        "Bocha"
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
                "Bocha backend selected but `[websearch].bocha_api_key` is not set.".to_string()
            })?;
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| "Bocha key is not a valid header value".to_string())?,
        );
        let response = client
            .post_json(
                BOCHA_SEARCH_ENDPOINT,
                headers,
                &serde_json::json!({
                    "query": query,
                    "summary": true,
                    "freshness": "noLimit",
                    "count": 10
                }),
            )
            .await
            .map_err(|e| format!("Bocha request failed: {e}"))?;
        let status = response.status;
        if !status.is_success() {
            return Err(format!(
                "Bocha returned HTTP {status} (check bocha_api_key): {}",
                response.body.chars().take(300).collect::<String>()
            ));
        }
        let body = response.body;
        let json: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| format!("Bocha returned invalid JSON: {e}"))?;
        let results = json
            .get("data")
            .and_then(|d| d.get("webPages"))
            .and_then(|w| w.get("value"))
            .and_then(serde_json::Value::as_array)
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
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let snippet = item
        .get("summary")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_item_maps_name_url_summary() {
        let raw = serde_json::json!({
            "name": "Reqwest docs",
            "url": "https://docs.rs/reqwest",
            "summary": "An ergonomic HTTP client",
            "siteName": "docs.rs",
            "datePublished": "2026-01-01"
        });
        let r = parse_item(&raw).unwrap();
        assert_eq!(r.title, "Reqwest docs");
        assert_eq!(r.url, "https://docs.rs/reqwest");
        assert_eq!(r.snippet, "An ergonomic HTTP client");
    }

    #[test]
    fn parse_item_rejects_missing_url_or_title() {
        assert!(parse_item(&serde_json::json!({ "name": "x" })).is_none());
        assert!(parse_item(&serde_json::json!({ "url": "https://e.com" })).is_none());
    }
}
