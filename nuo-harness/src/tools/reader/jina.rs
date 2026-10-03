//! Jina Reader backend (`https://r.jina.ai/<url>`) — server-side rendering
//! (including JavaScript-heavy SPA pages) plus readability-style main-content
//! extraction, returned as Markdown. Works anonymously with a modest rate
//! limit; an optional `Authorization: Bearer` key raises the quota.
//!
//! Chosen over a local readability port because it needs no DOM crate and
//! handles the SPA case (which `html_to_text` cannot: an unrendered SPA shell
//! strips to near-empty text). The trade — one extra network hop and sending
//! the URL to a third party — is opt-in via `[web] reader = "jina"`.
//!
//! On transport error or HTTP >= 400 from Jina itself we return `Err`; there
//! is no implicit reader fallback. Jina's "Warning: Target URL returned error" lines (it
//! relays the *target's* status) are passed through as content, not errors,
//! because the page text is often still useful.

use super::ReaderOutput;
use async_trait::async_trait;
use nuo_wire::JINA_READER_ENDPOINT;

pub(crate) struct JinaReader {
    pub api_key: Option<String>,
}

#[async_trait]
pub(crate) trait ReadPage {
    async fn read(
        &self,
        client: &crate::tools::web::http::WebHttp,
        url: &str,
    ) -> Result<ReaderOutput, String>;
}

#[async_trait]
impl ReadPage for JinaReader {
    async fn read(
        &self,
        client: &crate::tools::web::http::WebHttp,
        url: &str,
    ) -> Result<ReaderOutput, String> {
        let reader_url = format!("{JINA_READER_ENDPOINT}{url}");
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::ACCEPT,
            http::HeaderValue::from_static("text/plain"),
        );
        if let Some(key) = self
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            && let Ok(value) = http::HeaderValue::from_str(&format!("Bearer {key}"))
        {
            headers.insert(http::header::AUTHORIZATION, value);
        }
        let response = client
            .get(&reader_url, headers)
            .await
            .map_err(|e| format!("Jina reader request failed: {e}"))?;
        let status = response.status;
        if !status.is_success() {
            return Err(format!(
                "Jina reader returned HTTP {status}: {}",
                response.body.chars().take(200).collect::<String>()
            ));
        }
        let text = response.body;
        Ok(ReaderOutput {
            text,
            content_type: "text/markdown".to_string(),
        })
    }
}
