use std::sync::RwLock;

use async_trait::async_trait;
use nuo_wire::{SharedWebConfig, Tool, WebReaderProvider, WebRuntimeConfig};
use nuo_tool::ToolSchema;
use serde::Deserialize;

use crate::snapshot::{WebSnapshotResult, take_snapshot};

pub const WEB_READER_MAX_TOKENS: usize = 4_000;

#[derive(ToolSchema, Deserialize)]
struct WebReaderArgs {
    #[tool(desc = "The fully-qualified URL to read (http/https)")]
    url: String,
    #[tool(desc = "If true, return raw content without HTML stripping (default false)")]
    raw: Option<bool>,
}

type CachedClient = (
    u64,
    Result<std::sync::Arc<crate::http::WebHttp>, String>,
);

/// Read a web page URL and extract its clean Markdown content via the configured Reader.
pub struct WebReaderTool {
    config: SharedWebConfig,
    client: RwLock<Option<CachedClient>>,
}

impl WebReaderTool {
    pub fn new() -> Self {
        Self::with_config(WebRuntimeConfig::default())
    }
    pub fn with_config(config: WebRuntimeConfig) -> Self {
        Self::with_shared_config(SharedWebConfig::new(config))
    }
    pub fn with_shared_config(config: SharedWebConfig) -> Self {
        Self {
            config,
            client: RwLock::new(None),
        }
    }

    pub fn client(&self) -> Result<std::sync::Arc<crate::http::WebHttp>, String> {
        let (revision, snapshot) = self.config.snapshot();
        self.client_for(revision, &snapshot)
    }

    /// Resolve the HTTP client against the exact runtime snapshot used by the
    /// rest of one operation. This prevents a hot update from mixing a reader
    /// built at revision N+1 with timeout state built at revision N.
    fn client_for(
        &self,
        revision: u64,
        snapshot: &WebRuntimeConfig,
    ) -> Result<std::sync::Arc<crate::http::WebHttp>, String> {
        {
            let guard = self
                .client
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some((cached_revision, built)) = guard.as_ref()
                && *cached_revision == revision
            {
                return built.clone().map_err(|e| e.clone());
            }
        }
        let built =
            crate::http::WebHttp::new(&snapshot.behavior).map(std::sync::Arc::new);
        *self
            .client
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((revision, built.clone()));
        built.map_err(|e| e.clone())
    }

    pub async fn snapshot(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<WebSnapshotResult, String> {
        let (revision, snapshot) = self.config.snapshot();
        let client = self.client_for(revision, &snapshot)?;
        take_snapshot(&client, url, etag, last_modified).await
    }
}

impl Default for WebReaderTool {
    fn default() -> Self {
        Self::new()
    }
}

fn extract_page_title(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(h1) = trimmed.strip_prefix("# ") {
            let title = h1.trim().trim_start_matches('#').trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    let html_title = crate::html::extract_html_title(content);
    if !html_title.is_empty() {
        return Some(html_title);
    }
    None
}

#[async_trait]
impl Tool for WebReaderTool {
    fn name(&self) -> &str {
        "read_url"
    }
    fn is_available(&self) -> bool {
        let snapshot = self.config.get();
        matches!(snapshot.behavior.reader, WebReaderProvider::Jina)
    }
    fn description(&self) -> &str {
        "Read a web page and return its text content as clean Markdown."
    }
    fn parameters(&self) -> serde_json::Value {
        WebReaderArgs::parameters_schema()
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_structured(arguments)
            .await
            .map(|out| out.to_text())
    }
    async fn call_structured(&self, arguments: &str) -> Result<nuo_wire::ToolOutput, String> {
        let args: WebReaderArgs =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let url = &args.url;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("URL must start with http:// or https://".to_string());
        }
        crate::ssrf::assert_public_url(url).await?;
        let raw = args.raw.unwrap_or(false);
        let (revision, snapshot) = self.config.snapshot();
        let client = self.client_for(revision, &snapshot)?;
        let reader = crate::reader::build_reader(&snapshot);
        let reader_name = reader.name();
        let output = reader.read(&client, url, raw).await?;
        let body = output.text;
        let domain = crate::ssrf::extract_host(url).unwrap_or_else(|| "web".to_string());
        let title = extract_page_title(&body);
        let tokens = nuo_wire::tokenizer::count_tokens(&body);
        let (markdown, truncated) = if tokens > WEB_READER_MAX_TOKENS {
            let (keep, _kept) =
                nuo_wire::tokenizer::truncate_to_tokens(&body, WEB_READER_MAX_TOKENS / 2);
            (keep.to_string(), true)
        } else {
            (body, false)
        };

        Ok(nuo_wire::ToolOutput::WebArticle {
            url: url.to_string(),
            title,
            domain,
            markdown,
            reader: reader_name.to_string(),
            tokens,
            truncated,
        })
    }
}
