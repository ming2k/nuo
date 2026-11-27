//! Unified `read`: one model-facing capability, internal dispatch by content.
//!
//! `read` returns the content of one named file (or virtual spool object),
//! bounded and rendered for the model. It never discovers, searches, filters,
//! transforms, batches, or fetches. See ADR-0176, ADR-0193, ADR-0213, and
//! ADR-0257 for the full contract and invariants.

mod document;
mod image;
mod line_index;
mod text;

use std::fs;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use nuo_tool::{
    BuiltinTool, DocumentExtractor, ImageNormalizer, PageRange, RiskProfile, Tool, ToolAccesses,
    ToolContext, ToolError, ToolOutput, ToolSchema, ToolScope,
};
use serde::Deserialize;
use serde_json::Value;

use super::{SystemToolContext, read_accesses};
use crate::version::{file_version, file_version_from_bytes};

pub(crate) use document::DocumentTextCache;
use document::{
    DOCUMENT_EXTRACTION_TIMEOUT, MAX_DOCUMENT_BYTES, is_pdf, run_document_extraction,
    window_page_span,
};
use image::{ImageIngest, image_mime, image_output_from_bytes, image_output_from_open_file};
pub(crate) use line_index::LineIndexCache;
pub(crate) use text::clamp_line_to;
use text::{
    BinaryLocation, MAX_READ_LINES, MAX_READ_SCAN_BYTES, TAIL_REVERSE_SEEK_THRESHOLD,
    binary_file_error, decode_utf8, lang_for, page_from_text, stream_lines_indexed, tail_from_eof,
};

/// Single-read prefix ceiling: files at or below this size are read once from
/// one file descriptor for magic-byte classification, optional SHA-256 version
/// hashing, and rendering (`[INV-READ-IO-01]`, ADR-0257).
const FIRST_CHUNK: usize = 256 * 1024;

/// Typed parameters for [`ReadTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct ReadArgs {
    #[tool(
        desc = "File path: workspace-relative, absolute within workspace, or a virtual path from earlier output"
    )]
    pub path: String,
    #[tool(
        desc = "1-based first line to read (default 1); negative N reads the last N lines. Text and PDF only"
    )]
    pub offset: Option<i64>,
    #[tool(desc = "Max lines to return (default and max 2000; output is also capped at 50 KiB)")]
    pub limit: Option<usize>,
    #[tool(
        desc = "PDF-only 1-based inclusive page range (e.g. '3', '1-5', '12-', '-5'); offset/limit then page within extracted text. Rejected for non-PDF files"
    )]
    pub pages: Option<String>,
}

/// Reads a single file (UTF-8 text, image, or text-layer PDF) or virtual spool object.
pub struct ReadTool {
    ctx: Arc<SystemToolContext>,
}

impl ReadTool {
    pub fn new(ctx: Arc<SystemToolContext>) -> Self {
        Self { ctx }
    }

    fn normalizer(&self, call_ctx: &ToolContext) -> Option<Arc<dyn ImageNormalizer>> {
        self.ctx.image_normalizer.clone().or_else(|| {
            call_ctx
                .service::<Arc<dyn ImageNormalizer>>()
                .map(Arc::clone)
        })
    }

    fn document_extractor(&self, call_ctx: &ToolContext) -> Option<Arc<dyn DocumentExtractor>> {
        self.ctx.document_extractor.clone().or_else(|| {
            call_ctx
                .service::<Arc<dyn DocumentExtractor>>()
                .map(Arc::clone)
        })
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        BuiltinTool::Read.as_str()
    }

    fn aliases(&self) -> &'static [&'static str] {
        BuiltinTool::Read.aliases()
    }

    fn description(&self) -> &str {
        "Read one file (UTF-8 text, image, or text-layer PDF) or a virtual path from truncated tool output. \
         Text returns 1-based numbered lines (capped at 2000 lines / 50 KiB per call; lines >2000 chars clipped), \
         a `version` token for edit_text/write_file `expected_version`, and the next `offset` when more remains. \
         Images (PNG/JPEG/GIF/WebP) return as an image. PDFs return extracted text (no OCR). \
         Not for directories (use find_files), content search (use search_text), or URLs (use read_url)."
    }

    fn parameters_schema(&self) -> Value {
        ReadArgs::parameters_schema()
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::ReadOnly, ToolScope::Workspace]
    }

    fn path_bound(&self) -> bool {
        true
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(30))
    }

    fn accesses(&self, arguments: &Value) -> ToolAccesses {
        read_accesses(arguments)
    }

    async fn execute(
        &self,
        call_ctx: &ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReadArgs = serde_json::from_value(arguments).map_err(|err| {
            ToolError::execution(self.name(), format!("invalid arguments: {err}"))
        })?;
        let raw_path = args.path.trim();
        if raw_path.is_empty() {
            return Err(ToolError::execution(self.name(), "`path` cannot be empty"));
        }
        if args.offset == Some(0) {
            return Err(ToolError::execution(
                self.name(),
                "`offset` is 1-based; use 1 for the first line or a negative number to read from the end",
            ));
        }
        if args.limit == Some(0) {
            return Err(ToolError::execution(
                self.name(),
                "`limit` must be at least 1",
            ));
        }
        let limit = args.limit.unwrap_or(MAX_READ_LINES).min(MAX_READ_LINES);
        let page_range = args
            .pages
            .as_deref()
            .map(PageRange::parse)
            .transpose()
            .map_err(|msg| ToolError::execution(self.name(), msg))?;

        // Virtual text (spool/artifact/compacted history) is an in-memory
        // source. Any physical fallback runs off the async runtime and is
        // strictly confined to an owned spool root (`[INV-READ-VFS-01]`, ADR-0257).
        if nuo_tool::is_virtual_path(raw_path) {
            let resolved = if let Some(resolver) = &self.ctx.vfs_resolver {
                resolver.resolve_virtual(raw_path).await
            } else if let Some(resolver) =
                call_ctx.service::<Arc<dyn nuo_tool::VirtualFileResolver>>()
            {
                resolver.resolve_virtual(raw_path).await
            } else {
                None
            };
            return match resolved {
                Some(text) => {
                    reject_pages_for_non_pdf(page_range, raw_path, self.name())?;
                    self.text_output(&text, args.offset, limit, raw_path, None)
                }
                None => {
                    let raw_owned = raw_path.to_string();
                    let spool_path = tokio::task::spawn_blocking(move || {
                        nuo_tool::resolve_physical_spool_path(&raw_owned)
                    })
                    .await
                    .map_err(|error| {
                        ToolError::execution(self.name(), format!("read worker failed: {error}"))
                    })?;
                    if let Some(spool_path) = spool_path {
                        self.read_path(
                            &spool_path,
                            args.offset,
                            limit,
                            page_range,
                            raw_path,
                            call_ctx,
                        )
                        .await
                    } else {
                        Err(ToolError::execution(
                            self.name(),
                            format!("virtual path not found or expired: `{raw_path}`"),
                        ))
                    }
                }
            };
        }

        let path = self.ctx.resolve_path(raw_path);
        self.read_path(&path, args.offset, limit, page_range, raw_path, call_ctx)
            .await
    }
}

/// What the synchronous half of `read` produced. PDF is returned as a marker so
/// the only async dependency (the document extractor) runs on the caller.
enum BlockingRead {
    Output(ToolOutput),
    Pdf {
        size: u64,
        version: String,
        preloaded_bytes: Option<Vec<u8>>,
    },
}

fn reject_pages_for_non_pdf(
    page_range: Option<PageRange>,
    raw_path: &str,
    tool_name: &str,
) -> Result<(), ToolError> {
    if page_range.is_some() {
        return Err(ToolError::execution(
            tool_name,
            format!("`pages` is only valid for PDF files, but `{raw_path}` is not a PDF"),
        ));
    }
    Ok(())
}

impl ReadTool {
    /// Async read: the blocking half runs on the blocking pool, so a slow
    /// filesystem or a huge line scan cannot stall a Tokio worker shared by
    /// other sessions.
    async fn read_path(
        &self,
        path: &Path,
        offset: Option<i64>,
        limit: usize,
        page_range: Option<PageRange>,
        raw_path: &str,
        call_ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let normalizer = self.normalizer(call_ctx);
        let ctx = Arc::clone(&self.ctx);
        let blocking_path = path.to_path_buf();
        let path = path.to_path_buf();
        let raw = raw_path.to_string();
        let tool_ctx = call_ctx.clone();
        let blocking = tokio::task::spawn_blocking(move || {
            let tool = ReadTool::new(ctx);
            tool.read_path_sync(
                &blocking_path,
                offset,
                limit,
                page_range,
                &raw,
                normalizer,
                &tool_ctx,
            )
        })
        .await
        .map_err(|error| {
            ToolError::execution(self.name(), format!("read worker failed: {error}"))
        })??;
        match blocking {
            BlockingRead::Output(output) => Ok(output),
            BlockingRead::Pdf {
                size,
                version,
                preloaded_bytes,
            } => {
                self.document_output(
                    &path,
                    size,
                    preloaded_bytes,
                    offset,
                    limit,
                    page_range,
                    raw_path,
                    &version,
                    call_ctx,
                )
                .await
            }
        }
    }

    /// The synchronous half: single-fd metadata + bounded prefix read,
    /// magic-byte classification, text pagination, and image ingest.
    ///
    /// Files at or below [`FIRST_CHUNK`] (256 KiB) are read completely in one
    /// bounded read with zero TOCTOU window and zero duplicate disk I/O
    /// (`[INV-READ-IO-01]`, ADR-0257).
    #[allow(clippy::too_many_arguments)]
    fn read_path_sync(
        &self,
        path: &Path,
        offset: Option<i64>,
        limit: usize,
        page_range: Option<PageRange>,
        raw_path: &str,
        normalizer: Option<Arc<dyn ImageNormalizer>>,
        call_ctx: &ToolContext,
    ) -> Result<BlockingRead, ToolError> {
        call_ctx.check_cancelled(self.name())?;
        // Check directory/non-file before opening so Windows (which refuses
        // `File::open` on directories with AccessDenied) gets the same helpful
        // `find_files` error as Unix.
        let path_meta = fs::metadata(path).map_err(|err| {
            ToolError::execution(self.name(), format!("cannot read `{raw_path}`: {err}"))
        })?;
        if path_meta.is_dir() {
            return Err(ToolError::execution(
                self.name(),
                format!(
                    "`{raw_path}` is a directory; use `find_files` (e.g. with `max_depth=1`) to list its contents"
                ),
            ));
        }
        if !path_meta.is_file() {
            return Err(ToolError::execution(
                self.name(),
                format!("`{raw_path}` is not a regular file"),
            ));
        }

        let mut file = fs::File::open(path).map_err(|err| {
            ToolError::execution(self.name(), format!("cannot open `{raw_path}`: {err}"))
        })?;
        let meta = file.metadata().map_err(|err| {
            ToolError::execution(self.name(), format!("cannot read `{raw_path}`: {err}"))
        })?;
        if !meta.is_file() {
            return Err(ToolError::execution(
                self.name(),
                format!("`{raw_path}` is not a regular file"),
            ));
        }
        let file_len = meta.len();

        // Bounded read of up to FIRST_CHUNK + 1 bytes on the single open fd.
        // If <= FIRST_CHUNK bytes are returned, EOF was reached and `first`
        // holds the exact complete file contents with no TOCTOU risk.
        let mut first = Vec::with_capacity((FIRST_CHUNK + 1).min(file_len as usize + 1));
        file.by_ref()
            .take((FIRST_CHUNK + 1) as u64)
            .read_to_end(&mut first)
            .map_err(|err| {
                ToolError::execution(self.name(), format!("cannot read `{raw_path}`: {err}"))
            })?;
        let read_all = first.len() <= FIRST_CHUNK;
        let observed_len = if read_all {
            first.len() as u64
        } else {
            file_len.max(first.len() as u64)
        };

        if let Some(mime) = image_mime(&first) {
            reject_pages_for_non_pdf(page_range, raw_path, self.name())?;
            let ingest = ImageIngest {
                mime,
                raw_path,
                normalizer: normalizer.as_ref(),
                call_ctx,
                tool_name: self.name(),
            };
            let output = if read_all {
                image_output_from_bytes(first, &ingest)?
            } else {
                image_output_from_open_file(file, first, observed_len, &ingest)?
            };
            return Ok(BlockingRead::Output(output));
        }

        if is_pdf(&first) {
            let (version, preloaded_bytes) = if read_all {
                (file_version_from_bytes(&first), Some(first))
            } else {
                (file_version(path, &meta), None)
            };
            return Ok(BlockingRead::Pdf {
                size: observed_len,
                version,
                preloaded_bytes,
            });
        }

        if first.contains(&0) {
            return Err(ToolError::execution(
                self.name(),
                binary_file_error(
                    raw_path,
                    &first,
                    observed_len,
                    BinaryLocation::Prefix(first.len()),
                ),
            ));
        }

        reject_pages_for_non_pdf(page_range, raw_path, self.name())?;

        // Complete in-memory text fast path for all files <= FIRST_CHUNK (256 KiB).
        if read_all {
            let version = file_version_from_bytes(&first);
            let text = decode_utf8(&first, raw_path, 1)?;
            return self
                .text_output(&text, offset, limit, raw_path, Some(version))
                .map(BlockingRead::Output);
        }

        let version = file_version(path, &meta);
        let requested = offset.unwrap_or(1);

        // Large-file tail reads reverse-seek from EOF instead of sequentially
        // decoding megabytes from byte 0 (`[INV-READ-TAIL-01]`, ADR-0257).
        if requested < 0 && observed_len > TAIL_REVERSE_SEEK_THRESHOLD {
            let tail_lines = usize::try_from(requested.unsigned_abs())
                .unwrap_or(usize::MAX)
                .min(limit);
            let page = tail_from_eof(
                &mut file,
                observed_len,
                tail_lines,
                raw_path,
                &first,
                call_ctx,
                MAX_READ_SCAN_BYTES,
            )
            .map_err(|message| ToolError::execution(self.name(), message))?;
            return Ok(BlockingRead::Output(page.into_output_for(
                lang_for(raw_path),
                raw_path,
                None,
                None,
                Some(version),
            )));
        }

        let resume = (requested > 1)
            .then(|| {
                self.ctx.line_index.resume(
                    path,
                    &version,
                    usize::try_from(requested).unwrap_or(usize::MAX),
                )
            })
            .flatten();
        let (initial_line, base_offset, mut checkpoints) = match resume {
            Some((offset, line, checkpoints)) => (line, offset, checkpoints),
            None => (0, 0, Vec::new()),
        };
        file.seek(SeekFrom::Start(base_offset)).map_err(|err| {
            ToolError::execution(
                self.name(),
                format!("cannot seek `{raw_path}` to byte {base_offset}: {err}"),
            )
        })?;
        let reader = BufReader::new(file);
        let version_for_cache = version.clone();
        let page = stream_lines_indexed(
            reader,
            offset,
            limit,
            raw_path,
            &first,
            observed_len,
            call_ctx,
            MAX_READ_SCAN_BYTES,
            initial_line,
            base_offset,
            &mut checkpoints,
        )
        .map_err(|message| ToolError::execution(self.name(), message))?;
        self.ctx
            .line_index
            .store(path, version_for_cache, &mut checkpoints);
        Ok(BlockingRead::Output(page.into_output_for(
            lang_for(raw_path),
            raw_path,
            None,
            None,
            Some(version),
        )))
    }

    #[allow(clippy::too_many_arguments)]
    async fn document_output(
        &self,
        path: &Path,
        size: u64,
        preloaded_bytes: Option<Vec<u8>>,
        offset: Option<i64>,
        limit: usize,
        page_range: Option<PageRange>,
        raw_path: &str,
        version: &str,
        call_ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        if size > MAX_DOCUMENT_BYTES {
            return Err(ToolError::execution(
                self.name(),
                format!(
                    "`{raw_path}` is {size} bytes; the PDF ingest ceiling is {MAX_DOCUMENT_BYTES} bytes"
                ),
            ));
        }

        let document = if let Some(cached) = self.ctx.document_cache.get(path, version, page_range)
        {
            cached
        } else {
            let extractor = self.document_extractor(call_ctx).ok_or_else(|| {
                ToolError::execution(
                    self.name(),
                    format!(
                        "`{raw_path}` is a PDF, but no document text extractor is installed in this build"
                    ),
                )
            })?;

            let bytes = match preloaded_bytes {
                Some(bytes) => bytes,
                None => {
                    let path_owned = path.to_path_buf();
                    let raw_owned = raw_path.to_string();
                    let tool_name = self.name().to_string();
                    tokio::task::spawn_blocking(move || {
                        let file = fs::File::open(&path_owned).map_err(|err| {
                            ToolError::execution(
                                &tool_name,
                                format!("cannot open `{raw_owned}`: {err}"),
                            )
                        })?;
                        // Read at most one byte past the ceiling: metadata is a hint, not a
                        // guarantee that the file cannot grow before the read.
                        let mut bytes = Vec::with_capacity(size.min(MAX_DOCUMENT_BYTES) as usize);
                        file.take(MAX_DOCUMENT_BYTES + 1)
                            .read_to_end(&mut bytes)
                            .map_err(|err| {
                                ToolError::execution(
                                    &tool_name,
                                    format!("cannot read `{raw_owned}`: {err}"),
                                )
                            })?;
                        if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
                            return Err(ToolError::execution(
                                &tool_name,
                                format!(
                                    "`{raw_owned}` exceeds the PDF ingest ceiling of {MAX_DOCUMENT_BYTES} bytes"
                                ),
                            ));
                        }
                        Ok::<Vec<u8>, ToolError>(bytes)
                    })
                    .await
                    .map_err(|error| {
                        ToolError::execution(self.name(), format!("PDF worker failed: {error}"))
                    })??
                }
            };

            let extracted = Arc::new(
                run_document_extraction(
                    extractor,
                    bytes,
                    page_range,
                    DOCUMENT_EXTRACTION_TIMEOUT,
                    self.name(),
                    raw_path,
                )
                .await?,
            );
            self.ctx.document_cache.insert(
                path,
                version.to_string(),
                page_range,
                Arc::clone(&extracted),
            );
            extracted
        };

        let page = page_from_text(&document.text, offset, limit, raw_path)
            .map_err(|message| ToolError::execution(self.name(), message))?;
        let window_pages =
            window_page_span(&document.text, page.start_line, page.text.lines().count());
        Ok(page.into_output_for(
            None,
            raw_path,
            Some(&document),
            window_pages,
            Some(version.to_string()),
        ))
    }

    fn text_output(
        &self,
        text: &str,
        offset: Option<i64>,
        limit: usize,
        raw_path: &str,
        version: Option<String>,
    ) -> Result<ToolOutput, ToolError> {
        let page = page_from_text(text, offset, limit, raw_path)
            .map_err(|message| ToolError::execution(self.name(), message))?;
        Ok(page.into_output_for(lang_for(raw_path), raw_path, None, None, version))
    }
}

#[cfg(test)]
mod tests;
