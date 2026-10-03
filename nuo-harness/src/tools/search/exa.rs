//! Exa hosted search via its MCP endpoint (`https://mcp.exa.ai/mcp`).
//!
//! Works anonymously (no key) with generous rate limits; an optional key routes
//! through the caller's own quota. Returns a pre-rendered, model-optimized text
//! blob, which we pass through largely verbatim. This is the default backend.

use super::{ProviderOutput, SearchProvider, mcp_tools_call};
use async_trait::async_trait;

use nuo_wire::EXA_SEARCH_ENDPOINT;
const EXA_TOOL: &str = "web_search_exa";

pub(crate) struct ExaProvider {
    pub api_key: Option<String>,
}

#[async_trait]
impl SearchProvider for ExaProvider {
    fn name(&self) -> &'static str {
        "Exa"
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
        let url = endpoint_with_key(self.api_key.as_deref());
        let text = mcp_tools_call(
            client,
            &url,
            EXA_TOOL,
            serde_json::json!({
                "query": query,
                "type": "auto",
                // 5 hits: measured 10 hits ≈ 14k tokens, which the 4k-token
                // budget would chop to ~28% — losing the URLs of the tail
                // results. 5 keeps the whole list inside the budget.
                "numResults": 5,
                "livecrawl": "fallback",
            }),
            &[],
        )
        .await?;
        Ok(ProviderOutput::Blob(text))
    }
}

/// Build the Exa MCP URL, appending `?exaApiKey=` when a key is configured.
/// The shared query encoder keeps keys with special characters intact.
fn endpoint_with_key(api_key: Option<&str>) -> String {
    let key = api_key.map(str::trim).filter(|s| !s.is_empty());
    match key {
        Some(key) => {
            crate::tools::web::http::with_query(EXA_SEARCH_ENDPOINT, &[("exaApiKey", key)])
        }
        None => EXA_SEARCH_ENDPOINT.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_omits_key_when_absent() {
        assert_eq!(endpoint_with_key(None), EXA_SEARCH_ENDPOINT);
    }

    #[test]
    fn endpoint_appends_encoded_key_when_present() {
        let url = endpoint_with_key(Some("secret key&more"));
        assert!(url.contains("exaApiKey=secret+key%26more"), "got: {url}");
    }
}
