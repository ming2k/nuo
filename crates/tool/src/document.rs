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
}

/// Converts a bounded document byte stream into bounded UTF-8 text.
///
/// The trait is synchronous because document parsers are CPU-bound; callers
/// must run it on a blocking thread and apply a wall-clock bound.
pub trait DocumentExtractor: Send + Sync {
    /// Extract text from `data`, whose content is `mime`.
    ///
    /// Implementations must not return unbounded output. A parser that stops
    /// at a configured ceiling returns [`ExtractedDocument::truncated`] with
    /// the prefix it could extract.
    fn extract(&self, mime: &str, data: &[u8]) -> Result<ExtractedDocument, DocumentError>;
}
