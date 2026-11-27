//! Raster-image ingestion normalization.
//!
//! A pasted screenshot is whatever the display produced: Retina captures are
//! routinely 4–8 MB of PNG, and every provider request replays the attachment
//! verbatim for the rest of the session. The wire cost is paid on **every**
//! turn, so the ingest boundary is the only place where shrinking it is both
//! possible and free of history rewrites.
//!
//! The policy is deliberately conservative:
//!
//! - an image already within both the edge and byte budgets is returned
//!   untouched (`None`); its header is read to learn the dimensions, but no
//!   pixel buffer is decoded or re-encoded;
//! - oversized images are downscaled so their longest edge is
//!   [`MAX_IMAGE_EDGE`], then re-encoded as PNG (lossless, alpha preserved) or,
//!   when the source was JPEG or the PNG does not fit the byte budget, as JPEG;
//! - transparency is flattened onto white only when JPEG is the only way to fit
//!   the budget.
//!
//! Non-raster or unsupported formats (GIF/WebP) are left alone: flattening an
//! animation to its first frame would silently change meaning, and a bounded
//! byte budget is not worth that.
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, GenericImageView, ImageFormat};

/// Longest edge kept after normalization. Anthropic recommends 1568 px; the
/// same bound keeps OpenAI's tiled cost and Google's inline-data cost in the
/// linear (rather than quadratic) region.
pub const MAX_IMAGE_EDGE: u32 = 1568;
/// Encoded (not base64) byte budget. Base64 expands by ~33%, so 4 MiB encoded
/// stays under the common 5 MiB per-image provider limit.
pub const MAX_ENCODED_BYTES: usize = 4 * 1024 * 1024;
/// JPEG quality used only when lossless PNG does not fit the byte budget.
pub const JPEG_QUALITY: u8 = 82;

/// A normalized raster image ready for base64 attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedImage {
    pub data: Vec<u8>,
    pub mime: String,
    pub width: u32,
    pub height: u32,
}

/// Host implementation of the tool-boundary image normalizer.
///
/// `nuo-tool-fs` owns the read capability but must not link the codec stack;
/// the server injects this zero-sized adapter into the filesystem tool context.
/// The policy is the ingest policy above: PNG/JPEG only, everything else passes
/// through untouched.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostImageNormalizer;

impl nuo_tool::ImageNormalizer for HostImageNormalizer {
    fn normalize(&self, data: &[u8], mime: &str) -> Option<nuo_tool::NormalizedImage> {
        normalize_image(data, mime).map(|image| nuo_tool::NormalizedImage {
            data: image.data,
            mime: image.mime,
            width: image.width,
            height: image.height,
        })
    }
}

/// Normalize one raster image, or return `None` when it is already within
/// budget, cannot be decoded, or is a format this boundary deliberately does
/// not rewrite.
pub fn normalize_image(data: &[u8], mime: &str) -> Option<NormalizedImage> {
    let source_is_jpeg = matches!(mime, "image/jpeg" | "image/jpg");
    if !source_is_jpeg && mime != "image/png" {
        return None;
    }
    // Header-only pass: a small image answers "leave me alone" without paying
    // for a full pixel decode.
    let reader = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    if width <= MAX_IMAGE_EDGE && height <= MAX_IMAGE_EDGE && data.len() <= MAX_ENCODED_BYTES {
        return None;
    }

    let decoded = image::load_from_memory(data).ok()?;
    let scaled = resize_to_edge(decoded, MAX_IMAGE_EDGE);
    encode_within_budget(scaled, source_is_jpeg)
}

/// Normalize a base64-encoded image, returning `(new_base64, new_mime)`.
///
/// Used by the message constructors that receive provider-ready attachments.
pub fn normalize_base64_image(data: &str, mime: &str) -> Option<(String, String)> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let normalized = normalize_image(&bytes, mime)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&normalized.data);
    Some((encoded, normalized.mime))
}

fn resize_to_edge(image: DynamicImage, max_edge: u32) -> DynamicImage {
    let (width, height) = image.dimensions();
    let longest = width.max(height);
    if longest <= max_edge {
        return image;
    }
    let ratio = max_edge as f64 / longest as f64;
    let next_width = ((width as f64 * ratio).round() as u32).max(1);
    let next_height = ((height as f64 * ratio).round() as u32).max(1);
    image.resize_exact(
        next_width,
        next_height,
        image::imageops::FilterType::Lanczos3,
    )
}

fn encode_within_budget(mut image: DynamicImage, source_is_jpeg: bool) -> Option<NormalizedImage> {
    let mut candidate = if source_is_jpeg {
        encode_jpeg(&image)
    } else {
        encode_png(&image)
    };
    if candidate
        .as_ref()
        .is_some_and(|encoded| encoded.data.len() <= MAX_ENCODED_BYTES)
    {
        return candidate;
    }

    // Alpha-preserving lossless did not fit. JPEG is the escalation: flatten
    // transparency onto white, which is the honest neutral for screenshots.
    if let Some(encoded) = encode_jpeg(&image)
        && encoded.data.len() <= MAX_ENCODED_BYTES
    {
        return Some(encoded);
    }

    // Still oversized (very noisy or very large): shrink the long edge until it
    // fits. Every step is deterministic, so a re-run is idempotent.
    for _ in 0..6 {
        let (width, height) = image.dimensions();
        let next_width = ((width as f64 * 0.8).round() as u32).max(1);
        let next_height = ((height as f64 * 0.8).round() as u32).max(1);
        if next_width < 256 || next_height < 256 {
            break;
        }
        image = image.resize_exact(
            next_width,
            next_height,
            image::imageops::FilterType::Lanczos3,
        );
        candidate = encode_jpeg(&image);
        if candidate
            .as_ref()
            .is_some_and(|encoded| encoded.data.len() <= MAX_ENCODED_BYTES)
        {
            return candidate;
        }
    }
    candidate
}

fn encode_png(image: &DynamicImage) -> Option<NormalizedImage> {
    let mut data = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut data), ImageFormat::Png)
        .ok()?;
    let (width, height) = image.dimensions();
    Some(NormalizedImage {
        data,
        mime: "image/png".to_string(),
        width,
        height,
    })
}

fn encode_jpeg(image: &DynamicImage) -> Option<NormalizedImage> {
    let rgb = flatten_alpha(image);
    let mut data = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut data, JPEG_QUALITY);
    encoder.encode_image(&rgb).ok()?;
    let (width, height) = rgb.dimensions();
    Some(NormalizedImage {
        data,
        mime: "image/jpeg".to_string(),
        width,
        height,
    })
}

/// Composite an RGBA image onto white so JPEG encoding has a defined result.
fn flatten_alpha(image: &DynamicImage) -> image::RgbImage {
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut flattened = image::RgbImage::new(width, height);
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let alpha = u32::from(pixel[3]);
        let blend = |channel: u8| -> u8 {
            ((u32::from(channel) * alpha + 255 * (255 - alpha)) / 255) as u8
        };
        flattened.put_pixel(
            x,
            y,
            image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]),
        );
    }
    flattened
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut image = RgbImage::new(width, height);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            *pixel = Rgb([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8]);
        }
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(image)
            .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn small_images_are_left_untouched() {
        let bytes = png(32, 32);
        assert!(normalize_image(&bytes, "image/png").is_none());
    }

    #[test]
    fn oversized_images_are_downscaled_to_the_long_edge() {
        let bytes = png(3_000, 1_000);
        let normalized = normalize_image(&bytes, "image/png").expect("normalized");
        assert_eq!(normalized.width.max(normalized.height), MAX_IMAGE_EDGE);
        assert!(normalized.data.len() < bytes.len());
        assert_eq!(normalized.mime, "image/png");
    }

    #[test]
    fn unsupported_formats_are_not_rewritten() {
        assert!(normalize_image(b"GIF89a", "image/gif").is_none());
        assert!(normalize_image(b"RIFF....WEBP", "image/webp").is_none());
    }

    #[test]
    fn base64_normalization_round_trips_a_small_image_unchanged() {
        // Small image => `normalize_image` returns None => caller keeps the
        // original base64 and mime.
        use base64::Engine;
        let bytes = png(16, 16);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        assert!(normalize_base64_image(&encoded, "image/png").is_none());
    }
}
