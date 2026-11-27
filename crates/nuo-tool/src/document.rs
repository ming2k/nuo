//! Document text-extraction contracts shared by tools and the host.
//!
//! `read` owns the model-facing path, but it must not link a PDF parser: a
//! minimal embedding of the filesystem tools should not compile a document
//! codec stack, and the host must not depend on the filesystem tool crate.
//! This module is the seam, analogous to [`crate::ImageNormalizer`]. The host
//! implements extraction; the server injects it into the session tool context.
//!
//! Implementations must be bounded by construction. The exact ceilings are
//! host policy, but a result larger than the caller's text page budget must be
//! reported through [`ExtractedDocument::truncated`] rather than silently
//! returned in full.

use thiserror::Error;

/// Inclusive 1-based page range selector for document extraction (`"1-5"`, `"10"`, `"12-"`, `"-5"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRange {
    pub start: usize,
    pub end: Option<usize>,
}

impl PageRange {
    /// Parse a human/model-supplied page range string.
    ///
    /// Accepted forms: `"N"` (single page), `"N-M"` (inclusive range),
    /// `"N-"` (from page `N` to EOF), `"-M"` (pages `1..=M`).
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("`pages` cannot be empty; use e.g. '1-5', '10', or '12-'".to_string());
        }
        let Some((lhs, rhs)) = trimmed.split_once('-') else {
            let page = parse_positive_page(trimmed, raw)?;
            return Ok(Self {
                start: page,
                end: Some(page),
            });
        };
        if rhs.contains('-') {
            return Err(format!(
                "invalid `pages` range `{raw}`; expected 'N', 'N-M', 'N-', or '-M'"
            ));
        }
        let start = if lhs.trim().is_empty() {
            1
        } else {
            parse_positive_page(lhs.trim(), raw)?
        };
        let end = if rhs.trim().is_empty() {
            None
        } else {
            let end_page = parse_positive_page(rhs.trim(), raw)?;
            if end_page < start {
                return Err(format!(
                    "invalid `pages` range `{raw}`: end page ({end_page}) is less than start page ({start})"
                ));
            }
            Some(end_page)
        };
        Ok(Self { start, end })
    }

    /// Whether a 1-based page number is before this range starts.
    pub fn before_start(&self, page: usize) -> bool {
        page < self.start
    }

    /// Whether a 1-based page number is past the inclusive end of this range.
    pub fn past_end(&self, page: usize) -> bool {
        self.end.is_some_and(|end| page > end)
    }
}

fn parse_positive_page(token: &str, raw: &str) -> Result<usize, String> {
    let page: usize = token
        .parse()
        .map_err(|_| format!("invalid `pages` range `{raw}`: `{token}` is not a page number"))?;
    if page == 0 {
        return Err(format!(
            "invalid `pages` range `{raw}`: page numbers are 1-based"
        ));
    }
    Ok(page)
}

/// Text extracted from a document format that is not plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedDocument {
    /// UTF-8 text with page boundaries marked as `[page N]` lines.
    pub text: String,
    /// Number of pages whose text is present in [`Self::text`].
    pub pages: usize,
    /// Total number of pages in the source document.
    pub total_pages: usize,
    /// `true` when a page, byte, or parser bound stopped extraction early.
    ///
    /// A truncated result is still useful, but callers must surface the bound
    /// in the model-facing framing; a truncated document must never look like
    /// it ended naturally.
    pub truncated: bool,
    /// 1-based page number to request next when additional unread pages remain.
    pub next_page: Option<usize>,
}

/// Why a document could not be converted to text.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DocumentError {
    /// The document is password-protected and the empty user password failed.
    #[error("document is encrypted")]
    Encrypted,
    /// The document parsed, but no page yielded non-whitespace text.
    ///
    /// This is the scanned-PDF case. OCR is deliberately not part of text
    /// extraction, so the caller must report it instead of returning an empty
    /// string.
    #[error("document has no extractable text in {pages} page(s)")]
    NoText { pages: usize },
    /// No extractor is registered for this MIME type.
    #[error("no document extractor for `{mime}`")]
    Unsupported { mime: String },
    /// The bytes are not a readable document.
    #[error("document is malformed: {detail}")]
    Malformed { detail: String },
    /// The caller asked for pages outside the document's bounds.
    #[error("requested page {requested} exceeds document length ({total_pages} page(s))")]
    PageOutOfRange {
        requested: usize,
        total_pages: usize,
    },
}

/// Converts a bounded document byte stream into bounded UTF-8 text.
///
/// The trait is synchronous because document parsers are CPU-bound; callers
/// must run it on a blocking thread and apply a wall-clock bound.
pub trait DocumentExtractor: Send + Sync {
    /// Extract text from `data`, whose content is `mime`, optionally scoped to `page_range`.
    ///
    /// Implementations must not return unbounded output. A parser that stops
    /// at a configured ceiling returns [`ExtractedDocument::truncated`] with
    /// the prefix it could extract.
    fn extract(
        &self,
        mime: &str,
        data: &[u8],
        page_range: Option<PageRange>,
    ) -> Result<ExtractedDocument, DocumentError>;
}

#[cfg(test)]
mod tests {
    use super::PageRange;

    #[test]
    fn parses_valid_page_ranges() {
        assert_eq!(
            PageRange::parse("5").unwrap(),
            PageRange {
                start: 5,
                end: Some(5)
            }
        );
        assert_eq!(
            PageRange::parse(" 1 - 10 ").unwrap(),
            PageRange {
                start: 1,
                end: Some(10)
            }
        );
        assert_eq!(
            PageRange::parse("12-").unwrap(),
            PageRange {
                start: 12,
                end: None
            }
        );
        assert_eq!(
            PageRange::parse("-4").unwrap(),
            PageRange {
                start: 1,
                end: Some(4)
            }
        );
    }

    #[test]
    fn rejects_invalid_page_ranges() {
        assert!(PageRange::parse("").is_err());
        assert!(PageRange::parse("0").is_err());
        assert!(PageRange::parse("5-2").is_err());
        assert!(PageRange::parse("1-2-3").is_err());
        assert!(PageRange::parse("abc").is_err());
    }
}
