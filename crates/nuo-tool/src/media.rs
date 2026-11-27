//! Media ingestion contracts shared by tools and the host.
//!
//! A tool that returns an image must not depend on the host's image codec
//! stack, and the host must not depend on the filesystem tool crate. This
//! module is the seam: `nuo-tool-fs` asks the injected [`ImageNormalizer`] to
//! shrink an image before it enters the durable transcript; `nuo-host` provides
//! the implementation. When no normalizer is installed (unit tests, a
//! deliberately minimal embedding), the tool passes the original bytes through
//! and the request projection still handles them correctly — it just pays the
//! full byte cost.

/// One image after boundary normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedImage {
    /// Encoded image bytes (not base64).
    pub data: Vec<u8>,
    /// MIME type of the encoded bytes.
    pub mime: String,
    /// Pixel width after normalization.
    pub width: u32,
    /// Pixel height after normalization.
    pub height: u32,
}

/// Boundary policy for raster images read from disk.
///
/// Implementations must be deterministic and idempotent: the same input bytes
/// and MIME must produce the same output, and normalizing an already-normalized
/// image must not change it again. Returning `None` means "leave the bytes
/// alone" — the tool then passes them through unchanged.
pub trait ImageNormalizer: Send + Sync {
    fn normalize(&self, data: &[u8], mime: &str) -> Option<NormalizedImage>;
}
