//! Bounded PDF detection, extraction, LRU caching, and page-span mapping.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tool::{DocumentError, DocumentExtractor, ExtractedDocument, PageRange, ToolError};

use super::line_index::normalize_cache_path;

/// Hard cap on a PDF source read for text extraction. PDFs are parsed locally
/// and reduced to text, so the source may be larger than the image ingest cap
/// without inflating every request.
pub const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;
/// Wall-clock bound on one PDF text extraction.
pub(super) const DOCUMENT_EXTRACTION_TIMEOUT: Duration = Duration::from_secs(30);
/// The PDF header may be preceded by bytes; the specification bounds the
/// search window at 1 KiB.
const PDF_HEADER_WINDOW: usize = 1024;

const MAX_CACHED_DOCUMENTS: usize = 4;
const MAX_CACHED_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheKey {
    path: PathBuf,
    version: String,
    page_range: Option<PageRange>,
}

struct CachedDocument {
    key: CacheKey,
    document: Arc<ExtractedDocument>,
}

/// Bounded LRU cache of extracted PDF text keyed by `(path, version, page_range)`.
///
/// Paginating through a PDF with `offset`/`limit` hits memory instead of
/// re-parsing the PDF on every turn (`[INV-READ-IO-01]`, ADR-0257).
#[derive(Default)]
pub(crate) struct DocumentTextCache {
    entries: Mutex<Vec<CachedDocument>>,
}

impl DocumentTextCache {
    pub(super) fn get(
        &self,
        path: &Path,
        version: &str,
        page_range: Option<PageRange>,
    ) -> Option<Arc<ExtractedDocument>> {
        let normalized = normalize_cache_path(path);
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let pos = entries.iter().position(|entry| {
            entry.key.path == normalized
                && entry.key.version == version
                && entry.key.page_range == page_range
        })?;
        let entry = entries.remove(pos);
        let doc = Arc::clone(&entry.document);
        entries.push(entry);
        Some(doc)
    }

    pub(super) fn insert(
        &self,
        path: &Path,
        version: String,
        page_range: Option<PageRange>,
        document: Arc<ExtractedDocument>,
    ) {
        if document.text.len() > MAX_CACHED_DOCUMENT_BYTES {
            return;
        }
        let key = CacheKey {
            path: normalize_cache_path(path),
            version,
            page_range,
        };
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(pos) = entries.iter().position(|entry| entry.key == key) {
            entries.remove(pos);
        }
        entries.push(CachedDocument { key, document });
        while entries.len() > MAX_CACHED_DOCUMENTS
            || total_cached_bytes(&entries) > MAX_CACHED_DOCUMENT_BYTES
        {
            entries.remove(0);
        }
    }
}

fn total_cached_bytes(entries: &[CachedDocument]) -> usize {
    entries.iter().map(|entry| entry.document.text.len()).sum()
}

/// A PDF is identified by its header, not by its extension. The specification
/// permits leading bytes, so the marker is searched inside the first
/// [`PDF_HEADER_WINDOW`] bytes. A valid UTF-8 prose prefix keeps the file on
/// the text branch: documentation that merely mentions `%PDF-` must not become
/// a parse failure. Whitespace, a BOM, or non-UTF-8 junk may precede the header.
pub(super) fn is_pdf(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(PDF_HEADER_WINDOW)];
    let Some(offset) = window
        .windows(b"%PDF-".len())
        .position(|candidate| candidate == b"%PDF-")
    else {
        return false;
    };
    if offset == 0 {
        return true;
    }
    let leading = &window[..offset];
    let leading = leading.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(leading);
    std::str::from_utf8(leading).is_err() || leading.iter().all(|byte| byte.is_ascii_whitespace())
}

pub(super) fn document_error_message(raw_path: &str, error: DocumentError) -> String {
    match error {
        DocumentError::Encrypted => format!(
            "`{raw_path}` is an encrypted PDF; reading password-protected documents is not supported"
        ),
        DocumentError::NoText { pages } => format!(
            "`{raw_path}` has no extractable text in {pages} page{}; it may be a scanned PDF \
             (OCR is not performed by read)",
            if pages == 1 { "" } else { "s" }
        ),
        DocumentError::Unsupported { mime } => {
            format!("no document text extractor is registered for `{mime}`")
        }
        DocumentError::Malformed { detail } => {
            format!("`{raw_path}` could not be parsed as a PDF: {detail}")
        }
        DocumentError::PageOutOfRange {
            requested,
            total_pages,
        } => format!(
            "`{raw_path}` has {total_pages} page{}; requested start page {requested} is out of range",
            if total_pages == 1 { "" } else { "s" }
        ),
    }
}

/// Run extraction off the async runtime with a wall-clock bound.
///
/// The host seam catches parser panics; a join failure is still reported rather
/// than ignored. A timeout returns control to the model, but cannot cancel a
/// blocking call already in progress — the source and page ceilings in the host
/// are the primary bound.
pub(super) async fn run_document_extraction(
    extractor: Arc<dyn DocumentExtractor>,
    bytes: Vec<u8>,
    page_range: Option<PageRange>,
    timeout: Duration,
    tool_name: &str,
    raw_path: &str,
) -> Result<ExtractedDocument, ToolError> {
    let extraction = tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || {
            extractor.extract("application/pdf", &bytes, page_range)
        }),
    )
    .await;

    match extraction {
        Err(_) => Err(ToolError::execution(
            tool_name,
            format!("PDF text extraction for `{raw_path}` exceeded {timeout:?}"),
        )),
        Ok(Err(join)) => Err(ToolError::execution(
            tool_name,
            format!("PDF text extraction for `{raw_path}` failed: {join}"),
        )),
        Ok(Ok(Err(err))) => Err(ToolError::execution(
            tool_name,
            document_error_message(raw_path, err),
        )),
        Ok(Ok(Ok(document))) => Ok(document),
    }
}

/// Parse a standalone `[page N]` marker emitted by the host document extractor.
fn parse_page_marker(line: &str) -> Option<usize> {
    line.trim()
        .strip_prefix("[page ")?
        .strip_suffix(']')?
        .parse::<usize>()
        .ok()
        .filter(|page| *page > 0)
}

/// Compute the inclusive PDF page span covered by a 1-based line window
/// (`start_line..start_line + line_count`) inside extracted document text.
///
/// When the window starts mid-page (after a `[page N]` line), the preceding
/// marker establishes the starting page so the header always maps lines back
/// to PDF pages accurately.
pub(super) fn window_page_span(
    full_text: &str,
    start_line: usize,
    line_count: usize,
) -> Option<(usize, usize)> {
    if start_line == 0 || line_count == 0 {
        return None;
    }
    let end_line = start_line.saturating_add(line_count - 1);
    let mut active_before: Option<usize> = None;
    let mut first_in_window: Option<usize> = None;
    let mut last_in_window: Option<usize> = None;

    for (idx, line) in full_text.lines().enumerate() {
        let line_no = idx + 1;
        if line_no > end_line {
            break;
        }
        let marker = parse_page_marker(line);
        if line_no < start_line {
            if let Some(page) = marker {
                active_before = Some(page);
            }
            continue;
        }
        if line_no == start_line {
            let start_page = marker.or(active_before);
            if let Some(page) = start_page {
                first_in_window = Some(page);
                last_in_window = Some(page);
            }
        } else if let Some(page) = marker {
            if first_in_window.is_none() {
                first_in_window = Some(page);
            }
            last_in_window = Some(page);
        }
    }

    match (first_in_window, last_in_window) {
        (Some(first), Some(last)) => Some((first, last)),
        _ => None,
    }
}
