use nuo_wire::truncate_utf8;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::client::guarded_get;
use crate::html::{extract_html_title, html_to_text};

const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;

/// A bounded, content-addressed observation of one public web page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebPageSnapshot {
    pub requested_url: String,
    pub final_url: String,
    pub title: String,
    pub text_preview: String,
    pub content_hash: String,
    pub content_type: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub body_bytes: usize,
    pub checked_at_ms: u64,
}

/// Result of a conditional page observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSnapshotResult {
    Modified(WebPageSnapshot),
    NotModified { checked_at_ms: u64 },
}

pub fn header_text(
    headers: &http::header::HeaderMap,
    name: http::header::HeaderName,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub async fn take_snapshot(
    client: &crate::http::WebHttp,
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<WebSnapshotResult, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("URL must start with http:// or https://".to_string());
    }
    let mut headers = http::header::HeaderMap::new();
    if let Some(value) = etag.map(str::trim).filter(|value| !value.is_empty())
        && let Ok(v) = http::header::HeaderValue::from_str(value)
    {
        headers.insert(http::header::IF_NONE_MATCH, v);
    }
    if let Some(value) = last_modified
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && let Ok(v) = http::header::HeaderValue::from_str(value)
    {
        headers.insert(http::header::IF_MODIFIED_SINCE, v);
    }
    let checked_at_ms = unix_now_ms();
    let response = guarded_get(client, url, headers).await?;
    let final_url = response.final_url;
    let headers = response.headers;
    let sent_etag = etag.map(str::trim).filter(|v| !v.is_empty());
    let got_etag = header_text(&headers, http::header::ETAG);
    if sent_etag.is_some() && sent_etag == got_etag.as_deref() {
        return Ok(WebSnapshotResult::NotModified { checked_at_ms });
    }
    let body = response.body;
    if body.len() > MAX_SNAPSHOT_BYTES {
        return Err(format!(
            "Response for {url} exceeds the {} MiB tracking limit",
            MAX_SNAPSHOT_BYTES / 1024 / 1024
        ));
    }
    let content_type = header_text(&headers, http::header::CONTENT_TYPE)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let raw_text = String::from_utf8_lossy(&body);
    let title = if content_type.contains("html") {
        extract_html_title(&raw_text)
    } else {
        String::new()
    };
    let readable = if content_type.contains("html") {
        html_to_text(&raw_text)
    } else {
        raw_text.trim().to_string()
    };
    let text_preview = truncate_utf8(&readable, 800).to_string();
    let content_hash = format!("{:x}", Sha256::digest(&body));
    Ok(WebSnapshotResult::Modified(WebPageSnapshot {
        requested_url: url.to_string(),
        final_url,
        title,
        text_preview,
        content_hash,
        content_type,
        etag: header_text(&headers, http::header::ETAG),
        last_modified: header_text(&headers, http::header::LAST_MODIFIED),
        body_bytes: body.len(),
        checked_at_ms,
    }))
}
