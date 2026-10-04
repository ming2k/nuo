//! Web search, HTTP retrieval, reader and SSRF-safe scraper tools for Nuo.

pub mod client;
pub mod html;
pub mod http;
pub mod reader;
pub mod reader_tool;
pub mod search;
pub mod search_tool;
pub mod snapshot;
pub mod ssrf;

#[cfg(test)]
mod tests;

pub use html::{extract_html_title, html_to_text};
pub use reader_tool::WebReaderTool;
pub use search_tool::WebSearchTool;
pub use snapshot::{WebPageSnapshot, WebSnapshotResult};
pub use ssrf::{assert_public_url, extract_host, is_public_ip};

nuo_wire::register_tool!(WebReaderFactory => |ctx| {
    ctx.get::<nuo_wire::SharedWebConfig>()
        .cloned()
        .map(WebReaderTool::with_shared_config)
        .unwrap_or_else(|| {
            WebReaderTool::with_config(
                ctx.get::<nuo_wire::WebRuntimeConfig>()
                    .cloned()
                    .unwrap_or_default(),
            )
        })
});

nuo_wire::register_tool!(WebSearchFactory => |ctx| {
    ctx.get::<nuo_wire::SharedWebConfig>()
        .cloned()
        .map(WebSearchTool::with_shared_config)
        .unwrap_or_else(|| {
            WebSearchTool::with_config(
                ctx.get::<nuo_wire::WebRuntimeConfig>()
                    .cloned()
                    .unwrap_or_default(),
            )
        })
});
