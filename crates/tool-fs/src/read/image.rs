//! Image magic-byte detection and bounded boundary normalization for [`super::ReadTool`].

use std::fs;
use std::io::Read;
use std::sync::Arc;

use base64::Engine as _;
use tool::{ImageNormalizer, ToolContext, ToolError, ToolOutput};

/// Hard cap on an image file ingested into the transcript.
pub const MAX_MEDIA_BYTES: u64 = 20 * 1024 * 1024;

pub(super) fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Inputs for normalizing and encoding an image read from disk.
pub(super) struct ImageIngest<'a> {
    pub(super) mime: &'a str,
    pub(super) raw_path: &'a str,
    pub(super) normalizer: Option<&'a Arc<dyn ImageNormalizer>>,
    pub(super) call_ctx: &'a ToolContext,
    pub(super) tool_name: &'a str,
}

pub(super) fn image_output_from_bytes(
    bytes: Vec<u8>,
    ingest: &ImageIngest<'_>,
) -> Result<ToolOutput, ToolError> {
    ingest.call_ctx.check_cancelled(ingest.tool_name)?;
    let size = bytes.len() as u64;
    if size > MAX_MEDIA_BYTES {
        return Err(ToolError::execution(
            ingest.tool_name,
            format!(
                "`{}` is {size} bytes; the image ingest ceiling is {MAX_MEDIA_BYTES} bytes",
                ingest.raw_path
            ),
        ));
    }
    let (mime, bytes) = match ingest
        .normalizer
        .and_then(|n| n.normalize(&bytes, ingest.mime))
    {
        Some(normalized) => (normalized.mime, normalized.data),
        None => (ingest.mime.to_string(), bytes),
    };
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(ToolOutput::Image { mime, data })
}

/// Finish reading a large image from an already-open file descriptor whose
/// leading bytes were already sniffed into `prefix`.
///
/// Uses `take(remaining + 1)` so a file that grows between `metadata` and
/// reading can never allocate past `MAX_MEDIA_BYTES + 1` bytes.
pub(super) fn image_output_from_open_file(
    mut file: fs::File,
    mut prefix: Vec<u8>,
    declared_size: u64,
    ingest: &ImageIngest<'_>,
) -> Result<ToolOutput, ToolError> {
    ingest.call_ctx.check_cancelled(ingest.tool_name)?;
    if declared_size > MAX_MEDIA_BYTES {
        return Err(ToolError::execution(
            ingest.tool_name,
            format!(
                "`{}` is {declared_size} bytes; the image ingest ceiling is {MAX_MEDIA_BYTES} bytes",
                ingest.raw_path
            ),
        ));
    }
    let already = prefix.len() as u64;
    let remaining_cap = MAX_MEDIA_BYTES.saturating_sub(already).saturating_add(1);
    prefix.reserve(declared_size.min(MAX_MEDIA_BYTES).saturating_sub(already) as usize);
    file.by_ref()
        .take(remaining_cap)
        .read_to_end(&mut prefix)
        .map_err(|err| {
            ToolError::execution(
                ingest.tool_name,
                format!("cannot read `{}`: {err}", ingest.raw_path),
            )
        })?;
    if prefix.len() as u64 > MAX_MEDIA_BYTES {
        return Err(ToolError::execution(
            ingest.tool_name,
            format!(
                "`{}` exceeds the image ingest ceiling of {MAX_MEDIA_BYTES} bytes",
                ingest.raw_path
            ),
        ));
    }
    image_output_from_bytes(prefix, ingest)
}
