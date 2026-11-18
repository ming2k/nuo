//! Host PDF text extraction.
//!
//! `read` owns the model-facing document path, but the filesystem tool crate
//! must not link a PDF parser (ADR-0193). This module is the host half of the
//! `DocumentExtractor` seam: the server injects [`HostDocumentExtractor`] into
//! the session tool context, and the tool asks for bounded UTF-8 text.
//!
//! The policy is deliberately conservative:
//!
//! - a source larger than the tool's ingest ceiling is rejected before it is
//!   handed to this module, so extraction never sees an unbounded byte stream;
//! - at most [`MAX_DOCUMENT_PAGES`] pages and [`MAX_EXTRACTED_TEXT_BYTES`] of
//!   text are produced; stopping early is reported as
//!   [`ExtractedDocument::truncated`];
//! - page boundaries are preserved as `[page N]` markers so the model can cite
//!   a page without mistaking extracted line numbers for page coordinates;
//! - encrypted documents and pages without a text layer are typed failures,
//!   not empty strings. OCR is not performed here.
use pdf_extract::{Document, PlainTextOutput};
use tool::{DocumentError, DocumentExtractor, ExtractedDocument, PageRange};

/// Maximum number of pages inspected from one PDF.
pub const MAX_DOCUMENT_PAGES: usize = 1_000;
/// Maximum UTF-8 text produced for one PDF.
pub const MAX_EXTRACTED_TEXT_BYTES: usize = 4 * 1024 * 1024;

/// Host implementation of the tool-boundary document extractor.
///
/// The parser is pure Rust and never spawns an external process. Extraction is
/// synchronous and CPU-bound; the tool runs it on a blocking thread with a
/// wall-clock bound.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostDocumentExtractor;

impl DocumentExtractor for HostDocumentExtractor {
    fn extract(
        &self,
        mime: &str,
        data: &[u8],
        page_range: Option<PageRange>,
    ) -> Result<ExtractedDocument, DocumentError> {
        if mime != "application/pdf" {
            return Err(DocumentError::Unsupported {
                mime: mime.to_string(),
            });
        }
        // The parser is third-party code operating on untrusted input. Catch
        // panics at the seam so a malformed document becomes a typed failure
        // instead of poisoning the blocking-thread join or the runtime.
        match std::panic::catch_unwind(|| {
            extract_pdf(
                data,
                MAX_DOCUMENT_PAGES,
                MAX_EXTRACTED_TEXT_BYTES,
                page_range,
            )
        }) {
            Ok(result) => result,
            Err(_) => Err(DocumentError::Malformed {
                detail: "PDF parser panicked".to_string(),
            }),
        }
    }
}

fn extract_pdf(
    data: &[u8],
    page_limit: usize,
    byte_limit: usize,
    page_range: Option<PageRange>,
) -> Result<ExtractedDocument, DocumentError> {
    let mut doc =
        Document::load_mem(data).map_err(|err| malformed(format!("cannot load PDF: {err}")))?;

    if doc.is_encrypted() {
        // Match the parser's own convention: a permissions-only document may
        // open with the empty user password. Anything else is a real password
        // failure, not a malformed file.
        doc.decrypt("").map_err(|_| DocumentError::Encrypted)?;
    }

    let pages = doc.get_pages();
    let total_pages = pages.len();
    if total_pages == 0 {
        return Err(malformed("PDF has no pages".to_string()));
    }
    if let Some(range) = page_range
        && range.start > total_pages
    {
        return Err(DocumentError::PageOutOfRange {
            requested: range.start,
            total_pages,
        });
    }
    let page_numbers: Vec<u32> = pages.keys().copied().collect();

    let mut text = String::new();
    let mut inspected = 0usize;
    let mut included = 0usize;
    let mut truncated = false;
    let mut next_page = None;

    for (idx, page_number) in page_numbers.into_iter().enumerate() {
        let ordinal = idx + 1;
        if page_range.is_some_and(|range| range.before_start(ordinal)) {
            continue;
        }
        if page_range.is_some_and(|range| range.past_end(ordinal)) {
            next_page = Some(ordinal);
            break;
        }
        if inspected >= page_limit {
            truncated = true;
            next_page = Some(ordinal);
            break;
        }
        inspected += 1;

        let raw = extract_page(&doc, page_number)?;
        let page = normalize_page_text(&raw);
        if page.is_empty() {
            continue;
        }

        let before = text.len();
        let complete = append_page(&mut text, page_number, &page, byte_limit);
        if text.len() == before {
            truncated = true;
            next_page = Some(ordinal);
            break;
        }
        included += 1;
        if !complete {
            truncated = true;
            if ordinal < total_pages {
                next_page = Some(ordinal + 1);
            }
            break;
        }
    }

    let text = text.trim_end().to_string();
    if text.is_empty() {
        // `inspected` is the honest count: it names how many pages actually
        // yielded no text before extraction stopped, not how many exist.
        return Err(DocumentError::NoText { pages: inspected });
    }

    Ok(ExtractedDocument {
        text,
        pages: included,
        total_pages,
        truncated,
        next_page,
    })
}

fn extract_page(doc: &Document, page_number: u32) -> Result<String, DocumentError> {
    let mut text = String::new();
    {
        let mut output = PlainTextOutput::new(&mut text);
        pdf_extract::output_doc_page(doc, &mut output, page_number)
            .map_err(|err| malformed(format!("cannot extract page {page_number}: {err}")))?;
    }
    Ok(text)
}

/// Collapse parser noise without inventing paragraph structure.
///
/// The extractor emits a word stream with line hints, not a layout model.
/// Trailing spaces and runs of blank lines are noise; everything else is kept
/// so the model can still see column and line breaks the parser reported.
fn normalize_page_text(raw: &str) -> String {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len());
    let mut blank_run = 0usize;
    for line in normalized.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    out.trim().to_string()
}

/// Append one page with its provenance marker, or the longest prefix that
/// fits.
///
/// A partial append never leaves a dangling marker with no body: if the marker
/// itself does not fit, nothing is appended and the caller stops. Returns
/// `true` only when the whole page (including its trailing newline) fit.
fn append_page(target: &mut String, page_number: u32, page: &str, cap: usize) -> bool {
    let marker = format!("[page {page_number}]\n");
    let needed = marker.len() + page.len() + 1;
    if needed <= cap.saturating_sub(target.len()) {
        target.push_str(&marker);
        target.push_str(page);
        target.push('\n');
        return true;
    }
    let remaining = cap.saturating_sub(target.len());
    if remaining <= marker.len() {
        return false;
    }
    let before = target.len();
    target.push_str(&marker);
    let body_start = target.len();
    push_within_cap(target, page, cap);
    if target.len() == body_start {
        // No body byte fit after the marker; leave no dangling provenance.
        target.truncate(before);
    }
    false
}

/// Append `chunk` up to `cap`, never splitting a UTF-8 scalar.
///
/// Returns `true` when the whole chunk fit. A partial append still leaves the
/// useful prefix in place; the caller records the truncation.
fn push_within_cap(target: &mut String, chunk: &str, cap: usize) -> bool {
    let remaining = cap.saturating_sub(target.len());
    if chunk.len() <= remaining {
        target.push_str(chunk);
        return true;
    }
    let mut end = remaining.min(chunk.len());
    while end > 0 && !chunk.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&chunk[..end]);
    false
}

fn malformed(detail: String) -> DocumentError {
    DocumentError::Malformed { detail }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_PAGE: &[u8] = include_bytes!("../testdata/two_page.pdf");
    const NO_TEXT: &[u8] = include_bytes!("../testdata/no_text.pdf");
    const EMBEDDED_FONT: &[u8] = include_bytes!("../testdata/embedded_font.pdf");

    fn extract(data: &[u8]) -> ExtractedDocument {
        HostDocumentExtractor
            .extract("application/pdf", data, None)
            .expect("extract PDF text")
    }

    #[test]
    fn extracts_pages_with_markers_and_known_text() {
        let document = extract(TWO_PAGE);
        assert_eq!(document.total_pages, 2);
        assert_eq!(document.pages, 2);
        assert!(!document.truncated);
        assert!(document.text.contains("[page 1]"));
        assert!(document.text.contains("Page one hello"));
        assert!(document.text.contains("[page 2]"));
        assert!(document.text.contains("Page two world"));
    }

    #[test]
    fn extracts_text_from_a_real_embedded_font_pdf() {
        // Generated by Typst, so the text uses an embedded subset font with a
        // Unicode map rather than one of the standard PDF base fonts.
        let document = extract(EMBEDDED_FONT);
        assert_eq!(document.total_pages, 2);
        assert_eq!(document.pages, 2);
        assert!(!document.truncated);
        assert!(document.text.contains("Page one hello"));
        assert!(document.text.contains("Page two world"));
    }

    #[test]
    fn no_text_layer_is_a_typed_failure() {
        let error = HostDocumentExtractor
            .extract("application/pdf", NO_TEXT, None)
            .expect_err("scanned/empty PDF must not look like a successful read");
        assert!(matches!(error, DocumentError::NoText { pages: 1 }));
    }

    #[test]
    fn encrypted_documents_are_reported_as_encrypted() {
        // Build a password-protected PDF at test time so the fixture stays
        // small and the encryption version is explicit.
        use pdf_extract::{Document, EncryptionState, EncryptionVersion, Permissions};

        let mut document = Document::load_mem(TWO_PAGE).expect("load fixture");
        let version = EncryptionVersion::V2 {
            document: &document,
            owner_password: "owner",
            user_password: "user",
            key_length: 128,
            permissions: Permissions::all(),
        };
        let state = EncryptionState::try_from(version).expect("encryption state");
        document.encrypt(&state).expect("encrypt");
        let mut encrypted = Vec::new();
        document
            .save_to(&mut encrypted)
            .expect("serialize encrypted PDF");

        let error = HostDocumentExtractor
            .extract("application/pdf", &encrypted, None)
            .expect_err("a password-protected PDF must not read as empty text");
        assert_eq!(error, DocumentError::Encrypted);
    }

    #[test]
    fn malformed_bytes_are_a_typed_failure_not_a_panic() {
        let error = HostDocumentExtractor
            .extract("application/pdf", b"%PDF-1.7\nnot a real document", None)
            .expect_err("truncated PDF must be reported");
        assert!(matches!(error, DocumentError::Malformed { .. }));
    }

    #[test]
    fn unsupported_mime_is_named() {
        let error = HostDocumentExtractor
            .extract("application/msword", b"whatever", None)
            .expect_err("only PDF is registered");
        assert_eq!(
            error,
            DocumentError::Unsupported {
                mime: "application/msword".to_string()
            }
        );
    }

    #[test]
    fn page_limit_reports_truncation_without_hiding_the_total() {
        let document = extract_pdf(TWO_PAGE, 1, MAX_EXTRACTED_TEXT_BYTES, None).expect("one page");
        assert_eq!(document.total_pages, 2);
        assert_eq!(document.pages, 1);
        assert!(document.truncated);
        assert_eq!(document.next_page, Some(2));
        assert!(document.text.contains("Page one hello"));
        assert!(!document.text.contains("Page two world"));
    }

    #[test]
    fn page_range_selects_requested_slice_and_reports_next_page() {
        let range = PageRange::parse("2").unwrap();
        let document = extract_pdf(
            TWO_PAGE,
            MAX_DOCUMENT_PAGES,
            MAX_EXTRACTED_TEXT_BYTES,
            Some(range),
        )
        .expect("second page");
        assert_eq!(document.total_pages, 2);
        assert_eq!(document.pages, 1);
        assert!(!document.truncated);
        assert_eq!(document.next_page, None);
        assert!(!document.text.contains("Page one hello"));
        assert!(document.text.contains("Page two world"));

        let first_only = PageRange::parse("1").unwrap();
        let document = extract_pdf(
            TWO_PAGE,
            MAX_DOCUMENT_PAGES,
            MAX_EXTRACTED_TEXT_BYTES,
            Some(first_only),
        )
        .expect("first page");
        assert!(!document.truncated);
        assert_eq!(document.next_page, Some(2));

        let oob = PageRange::parse("5").unwrap();
        let err = extract_pdf(
            TWO_PAGE,
            MAX_DOCUMENT_PAGES,
            MAX_EXTRACTED_TEXT_BYTES,
            Some(oob),
        )
        .expect_err("page 5 out of range");
        assert_eq!(
            err,
            DocumentError::PageOutOfRange {
                requested: 5,
                total_pages: 2
            }
        );
    }

    #[test]
    fn byte_limit_keeps_a_utf8_prefix_and_reports_truncation() {
        let document = extract_pdf(TWO_PAGE, MAX_DOCUMENT_PAGES, 32, None).expect("prefix");
        assert!(document.truncated);
        assert!(document.text.len() <= 32);
        assert!(std::str::from_utf8(document.text.as_bytes()).is_ok());
        // The second page's marker does not fit, so it must not leave a
        // dangling `[page 2` fragment.
        assert!(!document.text.contains("[page 2"));
    }

    #[test]
    fn push_within_cap_never_splits_a_scalar() {
        let mut text = String::new();
        // "é" is two bytes; a cap of 1 must append nothing rather than half a
        // scalar.
        assert!(!push_within_cap(&mut text, "é", 1));
        assert!(text.is_empty());
        let mut text = String::new();
        assert!(push_within_cap(&mut text, "ok", 2));
        assert_eq!(text, "ok");
    }

    #[test]
    fn append_page_never_leaves_a_marker_without_body() {
        let mut text = String::new();
        // The marker is 9 bytes and the body's first scalar needs 2; a 10-byte
        // cap cannot hold both, so neither is written.
        assert!(!append_page(&mut text, 1, "é", 10));
        assert!(text.is_empty());
    }
}
