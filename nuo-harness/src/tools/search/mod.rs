//! Pluggable web-search backends.
//!
//! Each backend implements the `SearchProvider` trait and lives in its own
//! module (`exa`, `parallel`, `duckduckgo`, `searxng`, `tavily`). The tool layer
//! ([`crate::tools::WebSearchTool`]) is a thin shell that delegates to the one
//! provider selected in `[web]` via the `build_provider` factory. Adding a new backend is one new module + one
//! match arm in `build_provider`; the tool and the other backends never
//! change.
//!
//! Default backend is the hosted Exa MCP endpoint (`mcp.exa.ai`), used
//! keylessly and anonymously — mirroring the approach taken by other coding
//! agents. Be aware that with the default, search queries are sent to a
//! third-party service; set a different `provider` (e.g. self-hosted
//! `searxng`) in `config.toml` if that matters.

use async_trait::async_trait;
use nuo_wire::{WebRuntimeConfig, WebSearchProvider};

pub mod bocha;
pub mod duckduckgo;
pub mod exa;
pub mod parallel;
pub mod searxng;
pub mod tavily;

/// A single search hit. Backends that return structured results (DDG, SearXNG,
/// Tavily, Bocha) parse their responses into this; backends that return a
/// pre-rendered text blob (Exa, Parallel) return it as [`ProviderOutput::Blob`]
/// for the tool layer to budget.
#[derive(Debug, Clone)]
pub(crate) struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// What a backend produced for one query.
///
/// The tool layer owns the token budget for both shapes: structured results
/// are formatted entry-by-entry with titles+URLs never truncated (they are the
/// model's candidate list), and blobs go through the same token cap. This is
/// the ADR-0118 known-limitation fix: providers no longer pre-format.
pub(crate) enum ProviderOutput {
    /// Parsed, structured hits — preferred: the tool layer can dedupe URLs,
    /// cap per-domain counts, and budget each entry individually.
    Results(Vec<SearchResult>),
    /// A pre-rendered, model-optimized text blob from the backend (Exa,
    /// Parallel). Passed through with the shared token cap.
    Blob(String),
}

/// The plugin contract. A backend turns a query into either structured hits
/// or a pre-rendered blob; the tool layer owns formatting and budgets.
/// Implementations own their HTTP shape and parsing; the tool layer only
/// handles argument parsing and client/proxy setup.
#[async_trait]
pub(crate) trait SearchProvider: Send + Sync {
    /// Human-readable label included in the result header, e.g. `"Exa"`.
    fn name(&self) -> &'static str;
    /// Run the search, or return an error describing what went wrong
    /// (surfaced verbatim to the model/user).
    async fn search(
        &self,
        client: &crate::tools::web::http::WebHttp,
        query: &str,
    ) -> Result<ProviderOutput, String>;
    /// Duplicate the provider. Providers are tiny config-carrying structs
    /// (connection state lives in the shared HTTP handle), so the web
    /// tool's revision-keyed chain cache can hand each call a consistent
    /// chain snapshot even while a config reload swaps the cache.
    fn clone_box(&self) -> Box<dyn SearchProvider>;
}

#[derive(Clone)]
struct DisabledSearchProvider;

#[async_trait]
impl SearchProvider for DisabledSearchProvider {
    fn name(&self) -> &'static str {
        "disabled"
    }
    async fn search(
        &self,
        _client: &crate::tools::web::http::WebHttp,
        _query: &str,
    ) -> Result<ProviderOutput, String> {
        Err("websearch is disabled in configuration".to_string())
    }
    fn clone_box(&self) -> Box<dyn SearchProvider> {
        Box::new(self.clone())
    }
}

/// Construct exactly the typed backend selected in the resolved snapshot.
/// There is deliberately no unknown-provider or fallback branch.
pub(crate) fn build_provider(cfg: &WebRuntimeConfig) -> Box<dyn SearchProvider> {
    let api_key = cfg
        .search_credential
        .as_ref()
        .map(|key| key.expose_secret().to_string());
    match cfg.behavior.provider {
        WebSearchProvider::Disabled => Box::new(DisabledSearchProvider),
        WebSearchProvider::Exa => Box::new(exa::ExaProvider { api_key }),
        WebSearchProvider::Parallel => Box::new(parallel::ParallelProvider { api_key }),
        WebSearchProvider::DuckDuckGo => Box::new(duckduckgo::DdgProvider),
        WebSearchProvider::Searxng => Box::new(searxng::SearxngProvider {
            url: cfg.behavior.searxng_url.clone(),
        }),
        WebSearchProvider::Tavily => Box::new(tavily::TavilyProvider { api_key }),
        WebSearchProvider::Bocha => Box::new(bocha::BochaProvider { api_key }),
    }
}

/// A realistic browser User-Agent, shared by the scraping-style backends.
pub(super) const MOZILLA_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Render structured results under a token budget.
///
/// **Titles and URLs are never truncated** — they are the model's candidate
/// list; losing the tail of the list is strictly worse than a shorter
/// snippet. Snippets are budgeted individually: when the remaining budget is
/// exhausted, later entries degrade to title+URL only, and if even that does
/// not fit, the list is cut with an explicit notice naming the number of
/// dropped hits (so the model knows to narrow the query).
///
/// This replaces the old behaviour where a pre-formatted list was simply
/// chopped at 4 000 tokens — which could cut mid-entry and swallow the URLs
/// of every result after the cut.
#[allow(dead_code)]
pub(super) fn format_results(query: &str, source: &str, results: Vec<SearchResult>) -> String {
    if results.is_empty() {
        return format!("No results found for '{query}' (via {source}).");
    }
    let header = format!("Search results for '{query}' (via {source}):\n\n");
    let mut remaining =
        MAX_RESULT_TOKENS.saturating_sub(nuo_wire::tokenizer::count_tokens(&header));
    let mut out = String::with_capacity(header.len() + 1024);
    out.push_str(&header);
    let mut dropped = 0usize;
    for (idx, result) in results.iter().enumerate() {
        // The always-kept line: title + URL. Never truncated.
        let head = format!("{}. {}\n   {}\n", idx + 1, result.title, result.url);
        let head_tokens = nuo_wire::tokenizer::count_tokens(&head);
        if head_tokens + 12 >= remaining {
            // Not even the title+URL line fits (12 ≈ the omitted-snippet
            // marker's cost). Drop the entry; count it in the notice.
            dropped = results.len() - idx;
            break;
        }
        remaining -= head_tokens;
        let snippet_tokens = nuo_wire::tokenizer::count_tokens(&result.snippet);
        if snippet_tokens <= remaining {
            out.push_str(&head);
            out.push_str(&format!("   {}\n", result.snippet));
            remaining -= snippet_tokens;
        } else {
            // Degrade to title+URL only rather than cutting the list.
            out.push_str(&head);
            out.push_str("   [snippet omitted to fit the result budget]\n");
            remaining = remaining.saturating_sub(12);
        }
    }
    if dropped > 0 {
        out.push_str(&format!(
            "\n[... {dropped} more results omitted to fit the {}-token budget — narrow the query if the tail matters ...]",
            MAX_RESULT_TOKENS
        ));
    }
    out
}

pub(crate) fn results_to_hits(
    results: Vec<SearchResult>,
) -> (Vec<nuo_wire::WebSearchHit>, bool) {
    let hits = results
        .into_iter()
        .map(|r| {
            let domain =
                crate::tools::ssrf::extract_host(&r.url).unwrap_or_else(|| "web".to_string());
            nuo_wire::WebSearchHit {
                title: r.title,
                url: r.url,
                domain,
                snippet: r.snippet,
            }
        })
        .collect();
    budget_web_hits(hits)
}

pub(crate) fn blob_to_hits(
    query: &str,
    provider: &str,
    text: &str,
) -> (Vec<nuo_wire::WebSearchHit>, bool) {
    let mut hits = Vec::new();
    for block in text.split("\n\n") {
        let trimmed = block.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(pos) = trimmed.find("http://").or_else(|| trimmed.find("https://")) {
            let rest = &trimmed[pos..];
            let url_end = rest
                .find([' ', '\n', '\t', ')', '"', ']'])
                .unwrap_or(rest.len());
            let url = rest[..url_end]
                .trim_end_matches(['.', ',', ';'])
                .to_string();
            let domain =
                crate::tools::ssrf::extract_host(&url).unwrap_or_else(|| "web".to_string());
            let before = trimmed[..pos].trim();
            let title = if before.is_empty() {
                format!("{query} hit")
            } else {
                before
                    .lines()
                    .last()
                    .unwrap_or(before)
                    .trim_start_matches([
                        '*', '#', '-', '1', '2', '3', '4', '5', '6', '7', '8', '9', '.', ' ',
                    ])
                    .trim()
                    .to_string()
            };
            let snippet = rest[url_end..].trim().to_string();
            hits.push(nuo_wire::WebSearchHit {
                title: if title.is_empty() { url.clone() } else { title },
                url,
                domain,
                snippet: if snippet.is_empty() {
                    trimmed.to_string()
                } else {
                    snippet
                },
            });
        }
    }
    if hits.is_empty() {
        hits.push(nuo_wire::WebSearchHit {
            title: format!("Search results for '{query}'"),
            url: String::new(),
            domain: provider.to_lowercase(),
            snippet: text.to_string(),
        });
    }
    budget_web_hits(hits)
}

pub(crate) fn budget_web_hits(
    hits: Vec<nuo_wire::WebSearchHit>,
) -> (Vec<nuo_wire::WebSearchHit>, bool) {
    let mut out = Vec::with_capacity(hits.len());
    let mut remaining = MAX_RESULT_TOKENS;
    let mut truncated = false;
    for hit in hits {
        let head = format!("{}\n{}", hit.title, hit.url);
        let head_tokens = nuo_wire::tokenizer::count_tokens(&head);
        if head_tokens + 12 >= remaining {
            truncated = true;
            break;
        }
        remaining -= head_tokens;
        let snippet_tokens = nuo_wire::tokenizer::count_tokens(&hit.snippet);
        let snippet = if snippet_tokens <= remaining {
            remaining -= snippet_tokens;
            hit.snippet
        } else {
            remaining = remaining.saturating_sub(12);
            "[snippet omitted to fit the result budget]".to_string()
        };
        out.push(nuo_wire::WebSearchHit {
            title: hit.title,
            url: hit.url,
            domain: hit.domain,
            snippet,
        });
    }
    (out, truncated)
}

/// Token budget for one `websearch` result (ADR-0120). Shared by the structured
/// renderer and the blob pass-through.
pub(super) const MAX_RESULT_TOKENS: usize = 4_000;

/// Guard the model's context window against huge provider payloads.
/// Token-bounded (ADR-0120): the cut lands on an exact token boundary and the
/// notice reports the context cost in the model's own unit.
#[allow(dead_code)]
pub(super) fn cap_output(text: &str) -> String {
    const MAX_TOKENS: usize = 4_000;
    let total = nuo_wire::tokenizer::count_tokens(text);
    if total <= MAX_TOKENS {
        return text.to_string();
    }
    let (prefix, kept) = nuo_wire::tokenizer::truncate_to_tokens(text, MAX_TOKENS);
    let dropped = total - kept;
    format!("{prefix}\n\n[... {dropped} more tokens truncated ...]")
}

/// Invoke a hosted MCP-style search endpoint via JSON-RPC `tools/call` and
/// extract the first `text` content block from the response. Handles both the
/// single-JSON and Server-Sent-Events (`data: {...}`) response shapes used by
/// the Exa and Parallel endpoints.
pub(super) async fn mcp_tools_call(
    client: &crate::tools::web::http::WebHttp,
    url: &str,
    tool: &str,
    arguments: serde_json::Value,
    extra_headers: &[(String, String)],
) -> Result<String, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments }
    });
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::ACCEPT,
        http::HeaderValue::from_static("application/json, text/event-stream"),
    );
    for (name, value) in extra_headers {
        if let Ok(v) = http::header::HeaderValue::from_str(value)
            && let Ok(n) = http::header::HeaderName::from_bytes(name.as_bytes())
        {
            headers.insert(n, v);
        }
    }
    let response = client
        .post_json(url, headers, &body)
        .await
        .map_err(|e| format!("{tool} request failed: {e}"))?;
    let status = response.status;
    if !status.is_success() {
        return Err(format!(
            "{tool} returned HTTP {status}: {}",
            response.body.chars().take(300).collect::<String>()
        ));
    }
    let text = response.body;
    extract_mcp_text(&text).ok_or_else(|| format!("{tool} returned no content (HTTP {status})"))
}

/// Pull the first `text` block out of an MCP response, trying the whole body as
/// JSON first, then each SSE `data:` line.
fn extract_mcp_text(body: &str) -> Option<String> {
    if let Some(text) = parse_mcp_payload(body.trim()) {
        return Some(text);
    }
    for line in body.lines() {
        if let Some(rest) = line.trim().strip_prefix("data:")
            && let Some(text) = parse_mcp_payload(rest.trim())
        {
            return Some(text);
        }
    }
    None
}

fn parse_mcp_payload(payload: &str) -> Option<String> {
    if payload.is_empty() || !payload.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    let content = value.get("result")?.get("content")?.as_array()?;
    for item in content {
        let is_text = item
            .get("type")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t == "text");
        if is_text && let Some(text) = item.get("text").and_then(|t| t.as_str()) {
            return Some(text.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_results_handles_empty_and_nonempty() {
        assert!(format_results("q", "SearXNG", Vec::new()).contains("No results found"));
        let r = vec![SearchResult {
            title: "T".to_string(),
            url: "https://e.com".to_string(),
            snippet: "S".to_string(),
        }];
        assert!(format_results("q", "Tavily", r).contains("1. T\n   https://e.com"));
    }

    #[test]
    fn format_results_never_drops_titles_or_urls() {
        // 30 results with long snippets: the budget cannot hold them all, but
        // every title+URL that fits must survive — they are the candidate
        // list. Snippets degrade first, entries drop last, with a notice.
        let results: Vec<SearchResult> = (0..30)
            .map(|i| SearchResult {
                title: format!("Result number {i} with a title"),
                url: format!("https://example.com/page/{i}"),
                snippet: "filler snippet. ".repeat(60),
            })
            .collect();
        let out = format_results("q", "Exa", results.clone());
        // Early entries keep their full snippet.
        assert!(out.contains("1. Result number 0 with a title"));
        assert!(out.contains("https://example.com/page/0"));
        // The very first entries must never lose their URL.
        assert!(out.contains("https://example.com/page/1"));
        assert!(out.contains("https://example.com/page/2"));
        // Budget engagement is visible one way or another.
        let degraded = out.contains("[snippet omitted to fit the result budget]");
        let dropped = out.contains("more results omitted to fit");
        assert!(
            degraded || dropped,
            "expected either snippet degradation or dropped-entry notice:\n{out}"
        );
        // Total stays inside the budget.
        let body = out.split("\n[... ").next().unwrap_or(&out).to_string();
        assert!(
            nuo_wire::tokenizer::count_tokens(&body) <= MAX_RESULT_TOKENS + 40,
            "body tokens = {}",
            nuo_wire::tokenizer::count_tokens(&body)
        );
    }

    #[test]
    fn format_results_prefers_full_snippet_over_degradation() {
        // Small result set with short snippets: nothing degrades, nothing is
        // dropped, and no budget notices appear.
        let results: Vec<SearchResult> = (0..3)
            .map(|i| SearchResult {
                title: format!("T{i}"),
                url: format!("https://e.com/{i}"),
                snippet: format!("snippet {i}"),
            })
            .collect();
        let out = format_results("q", "Exa", results);
        assert!(!out.contains("[snippet omitted"));
        assert!(!out.contains("more results omitted"));
        assert!(out.contains("snippet 0"));
        assert!(out.contains("snippet 2"));
    }

    #[test]
    fn extract_mcp_text_parses_single_json_payload() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"hello world"}]}}"#;
        assert_eq!(extract_mcp_text(body), Some("hello world".to_string()));
    }

    #[test]
    fn extract_mcp_text_parses_sse_stream() {
        let body = "event: message\ndata: {\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n\n";
        assert_eq!(extract_mcp_text(body), Some("hi".to_string()));
    }

    #[test]
    fn extract_mcp_text_ignores_non_text_blocks() {
        let body = r#"{"result":{"content":[{"type":"image","data":"x"}]}}"#;
        assert_eq!(extract_mcp_text(body), None);
    }

    #[test]
    fn cap_output_truncates_long_text() {
        // Token-dense text (words, not merge-friendly runs) so the 4 000-token
        // cap actually engages: 'a'*20 000 is only ~2 500 tokens because cl100k
        // merges long 'a' runs into single tokens.
        let long = "the quick brown fox jumps over the lazy dog. ".repeat(2_000);
        let out = cap_output(&long);
        assert!(
            out.contains("tokens truncated"),
            "got tail: {}",
            &out[out.len().saturating_sub(80)..]
        );
        // Within the budget by the exact tokenizer's own measure.
        let body = out.split("\n\n[... ").next().unwrap_or("");
        assert!(
            nuo_wire::tokenizer::count_tokens(body) <= 4_000,
            "body tokens = {}",
            nuo_wire::tokenizer::count_tokens(body)
        );
    }

    #[test]
    fn an_unknown_provider_name_is_rejected_at_parse_time() {
        let error = toml::from_str::<nuo_wire::WebConfig>("provider = \"totally-bogus\"")
            .expect_err("unknown providers must not parse");
        assert!(
            error
                .to_string()
                .contains("unsupported web search provider"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn the_default_provider_is_exa() {
        let cfg = nuo_wire::WebRuntimeConfig::default();
        assert_eq!(build_provider(&cfg).name(), "Exa");
    }
}
