use serde_json::Value;
use std::sync::RwLock;

use async_trait::async_trait;
use nuo_protocol::{SharedWebConfig, WebReaderProvider, WebRuntimeConfig};
use nuo_tool::Tool;

use nuo_tool::{BuiltinTool, ToolSchema};
use serde::Deserialize;

use crate::snapshot::{WebSnapshotResult, take_snapshot};

pub const WEB_READER_MAX_TOKENS: usize = 4_000;

#[derive(ToolSchema, Deserialize)]
struct WebReaderArgs {
    #[tool(desc = "HTTP/HTTPS URL to fetch")]
    url: String,
    #[tool(desc = "Return raw content without HTML-to-Markdown conversion (default false)")]
    raw: Option<bool>,
}

struct CachedClients {
    revision: u64,
    api: std::sync::Arc<crate::http::WebHttp>,
    guarded: std::sync::Arc<crate::http::GuardedWebHttp>,
}

/// Read a web page URL and extract its clean Markdown content via the configured Reader.
pub struct WebReaderTool {
    config: SharedWebConfig,
    clients: RwLock<Option<CachedClients>>,
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
            clients: RwLock::new(None),
        }
    }

    pub fn client(&self) -> std::sync::Arc<crate::http::WebHttp> {
        let (revision, snapshot) = self.config.snapshot();
        self.clients_for(revision, &snapshot).0
    }

    pub fn guarded_client(&self) -> std::sync::Arc<crate::http::GuardedWebHttp> {
        let (revision, snapshot) = self.config.snapshot();
        self.clients_for(revision, &snapshot).1
    }

    /// Resolve the HTTP clients against the exact runtime snapshot used by the
    /// rest of one operation. This prevents a hot update from mixing a reader
    /// built at revision N+1 with timeout state built at revision N.
    fn clients_for(
        &self,
        revision: u64,
        snapshot: &WebRuntimeConfig,
    ) -> (
        std::sync::Arc<crate::http::WebHttp>,
        std::sync::Arc<crate::http::GuardedWebHttp>,
    ) {
        {
            let guard = self
                .clients
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = guard.as_ref()
                && cached.revision == revision
            {
                return (cached.api.clone(), cached.guarded.clone());
            }
        }
        let api = std::sync::Arc::new(crate::http::WebHttp::new(&snapshot.behavior));
        let guarded = std::sync::Arc::new(crate::http::GuardedWebHttp::new(&snapshot.behavior));
        *self
            .clients
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(CachedClients {
            revision,
            api: api.clone(),
            guarded: guarded.clone(),
        });
        (api, guarded)
    }

    pub async fn snapshot(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<WebSnapshotResult, String> {
        let (revision, snapshot) = self.config.snapshot();
        let client = self.clients_for(revision, &snapshot).1;
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
        BuiltinTool::ReadUrl.as_str()
    }
    fn aliases(&self) -> &'static [&'static str] {
        BuiltinTool::ReadUrl.aliases()
    }
    fn is_available(&self) -> bool {
        let snapshot = self.config.get();
        matches!(snapshot.behavior.reader, WebReaderProvider::Jina)
    }
    fn description(&self) -> &str {
        "Fetch a URL and return its content as Markdown."
    }
    fn timeout(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(30))
    }

    fn parameters_schema(&self) -> serde_json::Value {
        WebReaderArgs::parameters_schema()
    }
    async fn execute(
        &self,
        _ctx: &nuo_tool::ToolContext,
        arguments: Value,
    ) -> Result<nuo_tool::ToolOutput, nuo_tool::ToolError> {
        let args: WebReaderArgs = serde_json::from_value(arguments)
            .map_err(|e| nuo_tool::ToolError::invalid_args(self.name(), e.to_string()))?;
        let url = &args.url;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(nuo_tool::ToolError::invalid_args(
                self.name(),
                "URL must start with http:// or https://",
            ));
        }
        crate::ssrf::assert_public_url(url).await?;
        let raw = args.raw.unwrap_or(false);
        let (revision, snapshot) = self.config.snapshot();
        let client = self.clients_for(revision, &snapshot).0;
        let reader = crate::reader::build_reader(&snapshot);
        let reader_name = reader.name();
        let output = reader.read(&client, url, raw).await?;
        let body = output.text;
        let domain = crate::ssrf::extract_host(url).unwrap_or_else(|| "web".to_string());
        let title = extract_page_title(&body);
        let tokens = nuo_tool::tokenizer::count_tokens(&body);
        let (markdown, truncated) = if tokens > WEB_READER_MAX_TOKENS {
            let (keep, _kept) =
                nuo_tool::tokenizer::truncate_to_tokens(&body, WEB_READER_MAX_TOKENS / 2);
            (keep.to_string(), true)
        } else {
            (body, false)
        };

        Ok(nuo_tool::ToolOutput::WebArticle {
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
