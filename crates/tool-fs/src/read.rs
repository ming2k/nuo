//! Unified `read`: one model-facing capability, internal dispatch by content.
//!
//! A path is a path. Whether it names a source file, a directory, an image, a
//! PDF, or a binary is a fact about the bytes, not a decision the model should
//! have to make before calling. See ADR-0176 for the contract and caps, and
//! ADR-0193 for the bounded PDF text path.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::Value;
use tool::{
    BuiltinTool, DocumentError, DocumentExtractor, ExtractedDocument, ImageNormalizer, RiskProfile,
    Tool, ToolAccesses, ToolContext, ToolError, ToolOutput, ToolSchema, ToolScope,
};

use super::{SystemToolContext, read_accesses};

/// Hard cap on returned text lines.
pub const MAX_READ_LINES: usize = 2_000;
/// Hard cap on returned text bytes (joined with `\n`).
pub const MAX_READ_BYTES: usize = 50 * 1024;
/// Hard cap on one rendered line. Longer lines are clamped and labelled.
pub const MAX_LINE_CHARS: usize = 2_000;
/// Maximum bytes a single read may scan before returning a page. A 1 GiB file
/// with `offset = 900_000` must not be read end-to-end just to return 50 KiB.
pub const MAX_READ_SCAN_BYTES: u64 = 64 * 1024 * 1024;
/// Cancellation is observed at this line cadence inside the blocking scan.
const CANCEL_CHECK_LINES: usize = 512;
/// A single directory listing is bounded; millions of entries would spend more
/// memory than the model can use and should be queried with `find_files`.
const MAX_DIRECTORY_ENTRIES: usize = 100_000;
/// Maximum bytes retained from one line. The model-facing clamp is 2000
/// characters; 4 bytes per character covers the worst-case UTF-8 line without
/// letting a minified single-line file exhaust memory.
const MAX_LINE_BYTES: usize = MAX_LINE_CHARS * 4;
/// Hard cap on an image file ingested into the transcript.
pub const MAX_MEDIA_BYTES: u64 = 20 * 1024 * 1024;
/// Hard cap on a PDF source read for text extraction. PDFs are parsed locally
/// and reduced to text, so the source may be larger than the image ingest cap
/// without inflating every request.
pub const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;
/// Wall-clock bound on one PDF text extraction.
const DOCUMENT_EXTRACTION_TIMEOUT: Duration = Duration::from_secs(30);
/// Prefix inspected for magic-byte classification.
const FIRST_CHUNK: usize = 256 * 1024;
/// The PDF header may be preceded by bytes; the specification bounds the
/// search window at 1 KiB.
const PDF_HEADER_WINDOW: usize = 1024;
/// Default tail length for a negative text offset.
const TAIL_LINES: usize = 100;

/// A sparse line checkpoint: byte offset of a line together with the number of
/// complete lines before it. A later read can seek here instead of rescanning
/// from byte zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineCheckpoint {
    offset: u64,
    line: usize,
}

const LINE_INDEX_INTERVAL_BYTES: u64 = 1024 * 1024;
const MAX_LINE_INDEX_FILES: usize = 16;
const MAX_LINE_INDEX_CHECKPOINTS: usize = 128;

struct CachedLineIndex {
    version: String,
    checkpoints: Vec<LineCheckpoint>,
}

/// Per-workspace sparse line index for large text reads.
///
/// The version token is the same cheap length+mtime fingerprint used for
/// optimistic writes. A changed file invalidates its checkpoints rather than
/// risking a page that starts at the wrong byte. Entries are bounded by file
/// count and checkpoint count; this is a latency optimisation, never a source
/// of truth.
#[derive(Default)]
pub(crate) struct LineIndexCache {
    entries: std::sync::Mutex<HashMap<PathBuf, CachedLineIndex>>,
}

impl LineIndexCache {
    fn load(&self, path: &Path, version: &str) -> Vec<LineCheckpoint> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(path)
            .filter(|entry| entry.version == version)
            .map(|entry| entry.checkpoints.clone())
            .unwrap_or_default()
    }

    /// Load the checkpoints for `path` and return the best checkpoint strictly
    /// before `requested` (1-based line number), together with the checkpoint
    /// set so the caller can merge newly observed positions back in.
    fn resume(
        &self,
        path: &Path,
        version: &str,
        requested: usize,
    ) -> Option<(u64, usize, Vec<LineCheckpoint>)> {
        let checkpoints = self.load(path, version);
        let checkpoint = checkpoints
            .iter()
            .rev()
            .find(|checkpoint| checkpoint.line < requested)
            .copied()?;
        Some((checkpoint.offset, checkpoint.line, checkpoints))
    }

    fn store(&self, path: &Path, version: String, checkpoints: &mut Vec<LineCheckpoint>) {
        if checkpoints.is_empty() {
            return;
        }
        checkpoints.sort_by_key(|checkpoint| checkpoint.offset);
        checkpoints.dedup_by_key(|checkpoint| checkpoint.offset);
        if checkpoints.len() > MAX_LINE_INDEX_CHECKPOINTS {
            let excess = checkpoints.len() - MAX_LINE_INDEX_CHECKPOINTS;
            checkpoints.drain(..excess);
        }
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if entries.len() >= MAX_LINE_INDEX_FILES
            && !entries.contains_key(path)
            && let Some(key) = entries.keys().next().cloned()
        {
            entries.remove(&key);
        }
        entries.insert(
            path.to_path_buf(),
            CachedLineIndex {
                version,
                checkpoints: checkpoints.clone(),
            },
        );
    }
}

/// Typed parameters for [`ReadTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct ReadArgs {
    #[tool(desc = "File or directory path (relative to workspace root)")]
    pub path: String,
    #[tool(desc = "1-based line or entry offset; negative reads text tail (default 1)")]
    pub offset: Option<i64>,
    #[tool(desc = "Max lines or entries to return (default/max 2000)")]
    pub limit: Option<usize>,
}

/// Reads a file or directory, dispatching on the actual content.
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
        "Read a file (text, PDF, or image) or list one directory level. \
         Use find_files for recursive file discovery and search_text for content search."
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
        let limit = args
            .limit
            .unwrap_or(MAX_READ_LINES)
            .clamp(1, MAX_READ_LINES);

        // Virtual text (spool/artifact/compacted history) is an in-memory
        // source. It never carries media: the offstream registry owns that
        // channel and injects it through its own projection.
        if tool::is_virtual_path(raw_path) {
            let resolved = if let Some(resolver) = &self.ctx.vfs_resolver {
                resolver.resolve_virtual(raw_path).await
            } else if let Some(resolver) = call_ctx.service::<Arc<dyn tool::VirtualFileResolver>>()
            {
                resolver.resolve_virtual(raw_path).await
            } else {
                None
            };
            return match resolved {
                Some(text) => self.text_output(&text, args.offset, limit, raw_path, None),
                None => {
                    let path = Path::new(raw_path);
                    if path.is_file() {
                        self.read_path(path, args.offset, limit, raw_path, call_ctx)
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
        self.read_path(&path, args.offset, limit, raw_path, call_ctx)
            .await
    }
}

/// What the synchronous half of `read` produced. PDF is returned as a marker so
/// the only async dependency (the document extractor) runs on the caller.
enum BlockingRead {
    Output(ToolOutput),
    Pdf { size: u64, version: String },
}

impl ReadTool {
    /// Async read: the blocking half runs on the blocking pool, so a slow
    /// filesystem, a huge line scan, or a directory listing cannot stall a
    /// Tokio worker shared by other sessions.
    async fn read_path(
        &self,
        path: &Path,
        offset: Option<i64>,
        limit: usize,
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
            tool.read_path_sync(&blocking_path, offset, limit, &raw, normalizer, &tool_ctx)
        })
        .await
        .map_err(|error| {
            ToolError::execution(self.name(), format!("read worker failed: {error}"))
        })??;
        match blocking {
            BlockingRead::Output(output) => Ok(output),
            BlockingRead::Pdf { size, version } => {
                self.document_output(&path, size, offset, limit, raw_path, &version, call_ctx)
                    .await
            }
        }
    }

    /// The synchronous half: metadata, directory listing, text pagination, and
    /// image ingest. PDF parsing is the only branch that needs the async
    /// document extractor, so it returns a marker and the caller runs it.
    fn read_path_sync(
        &self,
        path: &Path,
        offset: Option<i64>,
        limit: usize,
        raw_path: &str,
        normalizer: Option<Arc<dyn ImageNormalizer>>,
        call_ctx: &ToolContext,
    ) -> Result<BlockingRead, ToolError> {
        call_ctx.check_cancelled(self.name())?;
        let meta = match fs::metadata(path) {
            Ok(meta) => meta,
            Err(err) => {
                return Err(ToolError::execution(
                    self.name(),
                    format!("cannot read `{raw_path}`: {err}"),
                ));
            }
        };

        let version = file_version(path, &meta);
        if meta.is_dir() {
            return self
                .list_directory(path, offset, limit, raw_path, call_ctx)
                .map(BlockingRead::Output);
        }
        if !meta.is_file() {
            return Err(ToolError::execution(
                self.name(),
                format!("`{raw_path}` is neither a regular file nor a directory"),
            ));
        }

        // Tail reads must scan to EOF; refuse a pathological scan before
        // opening the file rather than after minutes of I/O.
        if offset.unwrap_or(1) < 0 && meta.len() > MAX_READ_SCAN_BYTES {
            return Err(ToolError::execution(
                self.name(),
                scan_budget_message(raw_path),
            ));
        }

        let mut file = fs::File::open(path).map_err(|err| {
            ToolError::execution(self.name(), format!("cannot open `{raw_path}`: {err}"))
        })?;
        let mut first = Vec::with_capacity(FIRST_CHUNK.min(meta.len() as usize));
        file.by_ref()
            .take(FIRST_CHUNK as u64)
            .read_to_end(&mut first)
            .map_err(|err| {
                ToolError::execution(self.name(), format!("cannot read `{raw_path}`: {err}"))
            })?;

        if let Some(mime) = image_mime(&first) {
            return self
                .image_output(
                    path,
                    mime,
                    meta.len(),
                    raw_path,
                    normalizer.as_ref(),
                    call_ctx,
                )
                .map(BlockingRead::Output);
        }
        if is_pdf(&first) {
            return Ok(BlockingRead::Pdf {
                size: meta.len(),
                version,
            });
        }
        if first.contains(&0) {
            return Err(ToolError::execution(
                self.name(),
                format!(
                    "`{raw_path}` is a binary file (NUL byte in the first {} bytes); \
                     only UTF-8 text and recognized images can be read.",
                    first.len()
                ),
            ));
        }

        // A small file fits in one bounded read; do not reopen or stream it.
        if meta.len() <= MAX_READ_BYTES as u64 {
            let bytes = fs::read(path).map_err(|err| {
                ToolError::execution(self.name(), format!("cannot read `{raw_path}`: {err}"))
            })?;
            let text = decode_utf8(&bytes, raw_path, 1)?;
            return self
                .text_output(&text, offset, limit, raw_path, Some(version))
                .map(BlockingRead::Output);
        }

        let requested = offset.unwrap_or(1);
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
        let result = self.stream_text(
            reader,
            offset,
            limit,
            raw_path,
            Some(version),
            call_ctx,
            initial_line,
            base_offset,
            &mut checkpoints,
        );
        self.ctx
            .line_index
            .store(path, version_for_cache, &mut checkpoints);
        result.map(BlockingRead::Output)
    }

    fn image_output(
        &self,
        path: &Path,
        mime: &str,
        size: u64,
        raw_path: &str,
        normalizer: Option<&Arc<dyn ImageNormalizer>>,
        call_ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        call_ctx.check_cancelled(self.name())?;
        if size > MAX_MEDIA_BYTES {
            return Err(ToolError::execution(
                self.name(),
                format!(
                    "`{raw_path}` is {size} bytes; the image ingest ceiling is {MAX_MEDIA_BYTES} bytes"
                ),
            ));
        }
        let bytes = fs::read(path).map_err(|err| {
            ToolError::execution(self.name(), format!("cannot read `{raw_path}`: {err}"))
        })?;
        let (mime, bytes) = match normalizer.and_then(|n| n.normalize(&bytes, mime)) {
            Some(normalized) => (normalized.mime, normalized.data),
            None => (mime.to_string(), bytes),
        };
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(ToolOutput::Image { mime, data })
    }

    #[allow(clippy::too_many_arguments)]
    async fn document_output(
        &self,
        path: &Path,
        size: u64,
        offset: Option<i64>,
        limit: usize,
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
        let extractor = self.document_extractor(call_ctx).ok_or_else(|| {
            ToolError::execution(
                self.name(),
                format!(
                    "`{raw_path}` is a PDF, but no document text extractor is installed in this build"
                ),
            )
        })?;

        let path = path.to_path_buf();
        let raw_owned = raw_path.to_string();
        let tool_name = self.name().to_string();
        let bytes = tokio::task::spawn_blocking(move || {
            let file = fs::File::open(&path).map_err(|err| {
                ToolError::execution(&tool_name, format!("cannot open `{raw_owned}`: {err}"))
            })?;
            // Read at most one byte past the ceiling: metadata is a hint, not a
            // guarantee that the file cannot grow before the read.
            let mut bytes = Vec::with_capacity(size.min(MAX_DOCUMENT_BYTES) as usize);
            file.take(MAX_DOCUMENT_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|err| {
                    ToolError::execution(&tool_name, format!("cannot read `{raw_owned}`: {err}"))
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
        })??;

        let document = run_document_extraction(
            extractor,
            bytes,
            DOCUMENT_EXTRACTION_TIMEOUT,
            self.name(),
            raw_path,
        )
        .await?;

        let page = page_from_text(&document.text, offset, limit, raw_path)
            .map_err(|message| ToolError::execution(self.name(), message))?;
        Ok(page.into_output_for(None, raw_path, Some(&document), Some(version.to_string())))
    }

    fn list_directory(
        &self,
        path: &Path,
        offset: Option<i64>,
        limit: usize,
        raw_path: &str,
        call_ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        call_ctx.check_cancelled(self.name())?;
        let offset = offset.unwrap_or(1);
        if offset < 1 {
            return Err(ToolError::execution(
                self.name(),
                format!(
                    "negative offset is only valid for text files; `{raw_path}` is a directory"
                ),
            ));
        }
        let offset = offset as usize;

        let mut entries: Vec<(u8, String, String)> = Vec::new();
        let read_dir = fs::read_dir(path).map_err(|err| {
            ToolError::execution(self.name(), format!("cannot list `{raw_path}`: {err}"))
        })?;
        for (index, entry) in read_dir.enumerate() {
            if index % CANCEL_CHECK_LINES == 0 {
                call_ctx.check_cancelled(self.name())?;
            }
            if index >= MAX_DIRECTORY_ENTRIES {
                return Err(ToolError::execution(
                    self.name(),
                    format!(
                        "`{raw_path}` has more than {MAX_DIRECTORY_ENTRIES} entries; \
                         use find_files with a pattern to narrow it"
                    ),
                ));
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                entries.push((0, name.to_lowercase(), format!("{name}/")));
            } else if file_type.is_symlink() {
                entries.push((2, name.to_lowercase(), format!("{name}@")));
            } else {
                entries.push((1, name.to_lowercase(), name));
            }
        }
        entries.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.2.cmp(&right.2))
        });

        let total = entries.len();
        let start = (offset - 1).min(total);
        let mut page = Vec::new();
        let mut bytes = 0usize;
        let mut index = start;
        while index < total && page.len() < limit {
            let display = entries[index].2.clone();
            let cost = display.len() + 1;
            if !page.is_empty() && bytes + cost > MAX_READ_BYTES {
                break;
            }
            bytes += cost;
            page.push(display);
            index += 1;
        }
        let next = (index < total).then_some(index + 1);

        Ok(ToolOutput::Listing {
            entries: page,
            total: Some(total),
            offset,
            next,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn stream_text(
        &self,
        reader: impl BufRead,
        offset: Option<i64>,
        limit: usize,
        raw_path: &str,
        version: Option<String>,
        call_ctx: &ToolContext,
        initial_line: usize,
        base_offset: u64,
        checkpoints: &mut Vec<LineCheckpoint>,
    ) -> Result<ToolOutput, ToolError> {
        let page = stream_lines_indexed(
            reader,
            offset,
            limit,
            raw_path,
            call_ctx,
            MAX_READ_SCAN_BYTES,
            initial_line,
            base_offset,
            checkpoints,
        )
        .map_err(|message| ToolError::execution(self.name(), message))?;
        Ok(page.into_output_for(lang_for(raw_path), raw_path, None, version))
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
        Ok(page.into_output_for(lang_for(raw_path), raw_path, None, version))
    }
}

/// One page of text plus the framing the model needs to continue.
#[derive(Debug)]
struct TextPage {
    text: String,
    start_line: usize,
    total: Option<usize>,
    next: Option<usize>,
}

impl TextPage {
    fn into_output_for(
        self,
        lang: Option<String>,
        raw_path: &str,
        document: Option<&ExtractedDocument>,
        version: Option<String>,
    ) -> ToolOutput {
        let lines = self.text.lines().count();
        let end_line = self.start_line + lines.saturating_sub(1);
        let document_note = document.map(|document| {
            let pages = if document.truncated && document.pages < document.total_pages {
                format!(
                    "{} of {} pages, extraction truncated",
                    document.pages, document.total_pages
                )
            } else if document.truncated {
                format!("{} pages, text truncated", document.total_pages)
            } else if document.total_pages == 1 {
                "1 page".to_string()
            } else {
                format!("{} pages", document.total_pages)
            };
            format!(" — PDF text, {pages}")
        });
        let note = document_note.as_deref().unwrap_or("");
        let version_note = version
            .as_deref()
            .map(|version| format!("; version {version}"))
            .unwrap_or_default();
        let prefix = match self.total {
            Some(total) if lines > 0 => {
                format!(
                    "[Lines {}-{} of {} from `{raw_path}`{}]",
                    self.start_line, end_line, total, note
                ) + &version_note
            }
            Some(total) => format!("[0 lines of {total} from `{raw_path}`{note}{version_note}]"),
            None if lines > 0 => {
                format!(
                    "[Lines {}-{} from `{raw_path}`{note}]",
                    self.start_line, end_line
                ) + &version_note
            }
            None => format!("[no lines from `{raw_path}`{note}{version_note}]"),
        };
        let suffix = match (self.next, self.total) {
            (Some(next), Some(total)) => {
                let remaining = total.saturating_sub(end_line);
                Some(format!(
                    "[{remaining} more line{} — read with offset={next}]",
                    if remaining == 1 { "" } else { "s" }
                ))
            }
            (Some(next), None) => Some(format!("[more lines — read with offset={next}]")),
            (None, _) => None,
        };
        ToolOutput::Code {
            lang,
            text: self.text,
            start_line: self.start_line,
            prefix: Some(prefix),
            suffix,
            version,
        }
    }
}

fn scan_budget_message(raw_path: &str) -> String {
    format!(
        "`{raw_path}` needs more than {} MiB of scanning to reach the requested offset. \
         Use search_text or code_query to locate the line, or read the tail with a negative offset.",
        MAX_READ_SCAN_BYTES / (1024 * 1024)
    )
}

/// Read one `\n`-terminated line while retaining at most [`MAX_LINE_BYTES`].
///
/// `read_until` grows without bound on a single-line file; this helper consumes
/// the rest of an oversized line without storing it, checks NUL bytes on every
/// buffer chunk, and enforces the same scan budget during the read so one
/// pathological line cannot defeat the budget.
fn read_bounded_line(
    reader: &mut impl BufRead,
    out: &mut Vec<u8>,
    raw_path: &str,
    line_no: usize,
    call_ctx: &ToolContext,
) -> Result<usize, String> {
    let mut consumed = 0usize;
    let mut truncated = false;
    loop {
        if call_ctx.is_cancelled() {
            return Err("tool `read` execution was cancelled".to_string());
        }
        let available = reader
            .fill_buf()
            .map_err(|err| format!("cannot read `{raw_path}`: {err}"))?;
        if available.is_empty() {
            break;
        }
        if available.contains(&0) {
            return Err(format!(
                "`{raw_path}` is a binary file (NUL byte on line {line_no})"
            ));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map(|index| index + 1).unwrap_or(available.len());
        if !truncated {
            let room = MAX_LINE_BYTES.saturating_sub(out.len());
            let copy = take.min(room);
            out.extend_from_slice(&available[..copy]);
            if copy < take {
                truncated = true;
            }
        }
        reader.consume(take);
        consumed = consumed.saturating_add(take);
        if consumed as u64 > MAX_READ_SCAN_BYTES {
            return Err(scan_budget_message(raw_path));
        }
        if newline.is_some() {
            break;
        }
    }
    if truncated {
        // The byte ceiling can land inside a multi-byte UTF-8 character. Back
        // off to the last valid boundary so `decode_line` reports a truncated
        // line, not an invalid-UTF-8 file.
        let mut end = out.len();
        while end > 0 && std::str::from_utf8(&out[..end]).is_err() {
            end -= 1;
        }
        out.truncate(end);
    }
    Ok(consumed)
}

#[cfg(test)]
fn stream_lines_with_budget(
    reader: impl BufRead,
    offset: Option<i64>,
    limit: usize,
    raw_path: &str,
    call_ctx: &ToolContext,
    scan_budget: u64,
) -> Result<TextPage, String> {
    let mut checkpoints = Vec::new();
    stream_lines_indexed(
        reader,
        offset,
        limit,
        raw_path,
        call_ctx,
        scan_budget,
        0,
        0,
        &mut checkpoints,
    )
}

#[allow(clippy::too_many_arguments)]
fn stream_lines_indexed(
    reader: impl BufRead,
    offset: Option<i64>,
    limit: usize,
    raw_path: &str,
    call_ctx: &ToolContext,
    scan_budget: u64,
    initial_line: usize,
    base_offset: u64,
    checkpoints: &mut Vec<LineCheckpoint>,
) -> Result<TextPage, String> {
    let offset = offset.unwrap_or(1);
    let tail = offset < 0;
    let requested = if tail {
        (offset.unsigned_abs() as usize).min(limit).min(TAIL_LINES)
    } else {
        offset.max(1) as usize
    };

    let mut reader = reader;
    let mut line_no = initial_line;
    let mut page: VecDeque<String> = VecDeque::new();
    let mut bytes = 0usize;
    let mut scanned = 0u64;
    let mut next_checkpoint = base_offset.saturating_add(LINE_INDEX_INTERVAL_BYTES);
    let mut buf = Vec::new();

    loop {
        if line_no.is_multiple_of(CANCEL_CHECK_LINES) && call_ctx.is_cancelled() {
            return Err("tool `read` execution was cancelled".to_string());
        }
        let absolute = base_offset.saturating_add(scanned);
        if absolute >= next_checkpoint {
            checkpoints.push(LineCheckpoint {
                offset: absolute,
                line: line_no,
            });
            next_checkpoint = absolute.saturating_add(LINE_INDEX_INTERVAL_BYTES);
        }
        buf.clear();
        let read = read_bounded_line(&mut reader, &mut buf, raw_path, line_no + 1, call_ctx)?;
        if read == 0 {
            break;
        }
        line_no += 1;
        scanned = scanned.saturating_add(read as u64);
        let still_scanning = tail || line_no <= requested;
        if still_scanning && scanned > scan_budget {
            return Err(scan_budget_message(raw_path));
        }

        if buf.contains(&0) {
            return Err(format!(
                "`{raw_path}` is a binary file (NUL byte on line {line_no})"
            ));
        }
        if tail {
            let line = decode_line(&buf, raw_path, line_no)?;
            page.push_back(line);
            if page.len() > requested {
                page.pop_front();
            }
            continue;
        }

        if line_no < requested {
            continue;
        }
        let line = decode_line(&buf, raw_path, line_no)?;
        let cost = line.len() + usize::from(!page.is_empty());
        if !page.is_empty() && bytes + cost > MAX_READ_BYTES {
            // The page is full by bytes. This line is the first one that did
            // not fit; the next page starts here.
            let text = page.into_iter().collect::<Vec<_>>().join("\n");
            return Ok(TextPage {
                text,
                start_line: requested.max(1),
                total: None,
                next: Some(line_no),
            });
        }
        bytes += cost;
        page.push_back(line);
        if page.len() >= limit {
            // Peek one buffer-sized chunk to decide whether a next page
            // exists, without consuming (or allocating) the next line.
            let found = reader
                .fill_buf()
                .map_err(|err| format!("cannot read `{raw_path}`: {err}"))?
                .is_empty();
            let next = (!found).then_some(line_no + 1);
            let text = page.into_iter().collect::<Vec<_>>().join("\n");
            return Ok(TextPage {
                text,
                start_line: requested.max(1),
                total: None,
                next,
            });
        }
    }

    let page_start = if tail {
        line_no.saturating_sub(page.len()) + 1
    } else {
        requested.max(1)
    };
    let text = page.into_iter().collect::<Vec<_>>().join("\n");
    Ok(TextPage {
        text,
        start_line: page_start,
        total: Some(line_no),
        next: None,
    })
}

fn page_from_text(
    text: &str,
    offset: Option<i64>,
    limit: usize,
    _raw_path: &str,
) -> Result<TextPage, String> {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let offset = offset.unwrap_or(1);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();

    if offset < 0 {
        let requested = (offset.unsigned_abs() as usize).min(limit).min(TAIL_LINES);
        let count = requested.min(total);
        let start = total - count;
        let page = lines[start..]
            .iter()
            .map(|line| clamp_line(line))
            .collect::<Vec<_>>();
        return Ok(TextPage {
            text: page.join("\n"),
            start_line: start + 1,
            total: Some(total),
            next: None,
        });
    }

    let start = (offset.max(1) as usize - 1).min(total);
    let mut page = Vec::new();
    let mut bytes = 0usize;
    let mut index = start;
    while index < total && page.len() < limit {
        let line = clamp_line(lines[index]);
        let cost = line.len() + usize::from(!page.is_empty());
        if !page.is_empty() && bytes + cost > MAX_READ_BYTES {
            break;
        }
        bytes += cost;
        page.push(line);
        index += 1;
    }
    let next = (index < total).then_some(index + 1);
    Ok(TextPage {
        text: page.join("\n"),
        start_line: start + 1,
        total: Some(total),
        next,
    })
}

fn decode_line(buf: &[u8], raw_path: &str, line_no: usize) -> Result<String, String> {
    let bytes = buf.strip_suffix(b"\n").unwrap_or(buf);
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|err| {
        format!(
            "`{raw_path}` is not valid UTF-8 at line {line_no} \
             (byte offset {} within the line): {err}",
            err.valid_up_to()
        )
    })?;
    let text = if line_no == 1 {
        text.strip_prefix('\u{FEFF}').unwrap_or(text)
    } else {
        text
    };
    Ok(clamp_line(text))
}

fn decode_utf8(bytes: &[u8], raw_path: &str, start_line: usize) -> Result<String, ToolError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|err| {
        ToolError::execution(
            "read",
            format!(
                "`{raw_path}` is not valid UTF-8 at line {start_line} (byte {}); \
                 only UTF-8 text can be read as text",
                err.valid_up_to()
            ),
        )
    })?;
    Ok(text.to_string())
}

/// Maximum file size for which a read computes a full content hash.
///
/// Optimistic concurrency should not double the I/O of every large read. Small
/// files get a content-addressed token; large files get a metadata identity
/// that includes inode and change time, which catches same-size rewrites
/// without hashing gigabytes.
const MAX_CONTENT_VERSION_BYTES: u64 = 1024 * 1024;

/// Content-addressed version for small files, metadata identity for large ones.
///
/// This is an optimistic-concurrency token for the common "read, edit, write"
/// sequence. A small file hashes its bytes, so a same-length replacement is
/// always detected. A large file uses length, mtime, device, inode, and ctime
/// (including nanoseconds on Unix); a writer that changes bytes necessarily
/// advances ctime or replaces the inode.
pub(crate) fn file_version(path: &Path, meta: &fs::Metadata) -> String {
    if meta.is_file()
        && meta.len() <= MAX_CONTENT_VERSION_BYTES
        && let Ok(bytes) = fs::read(path)
    {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(&bytes);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut hex = String::with_capacity(12);
        for byte in digest.iter().take(6) {
            hex.push(HEX[(byte >> 4) as usize] as char);
            hex.push(HEX[(byte & 0x0f) as usize] as char);
        }
        return format!("c{hex}-{:x}", bytes.len());
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        format!(
            "-{:x}-{:x}-{:x}-{:x}",
            meta.dev(),
            meta.ino(),
            meta.ctime(),
            meta.ctime_nsec()
        )
    };
    #[cfg(not(unix))]
    let identity = String::new();
    format!("m{:x}-{mtime:x}{identity}", meta.len())
}

pub(crate) fn clamp_line_to(line: &str, max_chars: usize) -> String {
    if line.chars().count() <= max_chars {
        return line.to_string();
    }
    let mut end = line.len();
    for (count, (offset, _)) in line.char_indices().enumerate() {
        if count == max_chars {
            end = offset;
            break;
        }
    }
    let mut clamped = line[..end].to_string();
    clamped.push_str(&format!(" … (line truncated to {max_chars} chars)"));
    clamped
}

fn clamp_line(line: &str) -> String {
    clamp_line_to(line, MAX_LINE_CHARS)
}

fn lang_for(raw_path: &str) -> Option<String> {
    Path::new(raw_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
}

/// A PDF is identified by its header, not by its extension. The specification
/// permits leading bytes, so the marker is searched inside the first
/// [`PDF_HEADER_WINDOW`] bytes. A valid UTF-8 prose prefix keeps the file on
/// the text branch: documentation that merely mentions `%PDF-` must not become
/// a parse failure. Whitespace, a BOM, or non-UTF-8 junk may precede the header.
fn is_pdf(bytes: &[u8]) -> bool {
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

fn document_error_message(raw_path: &str, error: DocumentError) -> String {
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
    }
}

/// Run extraction off the async runtime with a wall-clock bound.
///
/// The host seam catches parser panics; a join failure is still reported rather
/// than ignored. A timeout returns control to the model, but cannot cancel a
/// blocking call already in progress — the source and page ceilings in the host
/// are the primary bound.
async fn run_document_extraction(
    extractor: Arc<dyn DocumentExtractor>,
    bytes: Vec<u8>,
    timeout: Duration,
    tool_name: &str,
    raw_path: &str,
) -> Result<ExtractedDocument, ToolError> {
    let extraction = tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || extractor.extract("application/pdf", &bytes)),
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

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod stream_tests {
    use super::*;
    use ::tool::ToolContext;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn large_text_file_streams_the_requested_page() {
        let dir = tempdir().unwrap();
        let content = (0..3_000)
            .map(|index| format!("{index:04} {}", "x".repeat(80)))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(content.len() > MAX_READ_BYTES);
        std::fs::write(dir.path().join("big.log"), content).unwrap();

        let out = ReadTool::new(Arc::new(SystemToolContext::new(dir.path())))
            .execute(
                &ToolContext::default(),
                json!({"path": "big.log", "offset": 2500, "limit": 3}),
            )
            .await
            .unwrap();

        match out {
            ToolOutput::Code {
                text, start_line, ..
            } => {
                assert_eq!(start_line, 2500);
                assert_eq!(text.lines().count(), 3);
                assert!(text.starts_with("2499 "));
            }
            other => panic!("expected Code, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn large_text_file_tail_read_is_bounded() {
        let dir = tempdir().unwrap();
        let content = (0..3_000)
            .map(|index| format!("{index:04}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("big.log"), content).unwrap();

        let out = ReadTool::new(Arc::new(SystemToolContext::new(dir.path())))
            .execute(
                &ToolContext::default(),
                json!({"path": "big.log", "offset": -3}),
            )
            .await
            .unwrap();

        match out {
            ToolOutput::Code {
                text, start_line, ..
            } => {
                assert_eq!(text, "2997\n2998\n2999");
                assert_eq!(start_line, 2998);
            }
            other => panic!("expected Code, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    use ::tool::ToolContext;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn long_lines_are_clamped_and_labelled() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("long.txt"), "x".repeat(2_500)).unwrap();
        let out = ReadTool::new(Arc::new(SystemToolContext::new(dir.path())))
            .execute(&ToolContext::default(), json!({"path": "long.txt"}))
            .await
            .unwrap();
        match out {
            ToolOutput::Code { text, .. } => {
                let prefix: String = text.chars().take(MAX_LINE_CHARS).collect();
                assert_eq!(prefix.chars().count(), MAX_LINE_CHARS);
                assert!(text.contains("line truncated to 2000 chars"));
            }
            other => panic!("expected Code, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn utf8_bom_is_stripped_once() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("bom.txt"), "\u{FEFF}alpha\nbeta\n").unwrap();
        let out = ReadTool::new(Arc::new(SystemToolContext::new(dir.path())))
            .execute(&ToolContext::default(), json!({"path": "bom.txt"}))
            .await
            .unwrap();
        match out {
            ToolOutput::Code { text, .. } => assert_eq!(text, "alpha\nbeta"),
            other => panic!("expected Code, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod blocking_tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufReader, Cursor};
    use tempfile::tempdir;

    #[test]
    fn a_single_long_line_is_consumed_but_not_retained() {
        let text = format!("{}\n", "a".repeat(100_000));
        let mut reader = BufReader::new(Cursor::new(text.into_bytes()));
        let mut line = Vec::new();
        let consumed = read_bounded_line(
            &mut reader,
            &mut line,
            "long.log",
            1,
            &ToolContext::default(),
        )
        .expect("bounded line");
        assert_eq!(consumed, 100_001);
        assert!(line.len() <= MAX_LINE_BYTES);
        let mut next = Vec::new();
        assert_eq!(
            read_bounded_line(
                &mut reader,
                &mut next,
                "long.log",
                2,
                &ToolContext::default()
            )
            .expect("eof"),
            0
        );
    }

    #[test]
    fn a_truncated_line_stays_on_a_utf8_boundary() {
        let text = format!("{}\n", "€".repeat(3_000));
        let mut reader = BufReader::new(Cursor::new(text.into_bytes()));
        let mut line = Vec::new();
        read_bounded_line(
            &mut reader,
            &mut line,
            "utf8.log",
            1,
            &ToolContext::default(),
        )
        .expect("bounded line");
        assert!(line.len() <= MAX_LINE_BYTES);
        assert!(std::str::from_utf8(&line).is_ok());
    }

    #[tokio::test]
    async fn a_repeated_deep_read_uses_the_sparse_index() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("big.log");
        let content = "line\n".repeat(300_000);
        std::fs::write(&path, content).unwrap();
        let ctx = Arc::new(SystemToolContext::new(dir.path()));
        let tool = ReadTool::new(Arc::clone(&ctx));
        let args = json!({"path": "big.log", "offset": 250_000, "limit": 2});

        let first = tool
            .execute(&ToolContext::default(), args.clone())
            .await
            .unwrap();
        let version = file_version(&path, &std::fs::metadata(&path).unwrap());
        let checkpoints = ctx.line_index.load(&path, &version);
        assert!(
            !checkpoints.is_empty(),
            "the first scan must seed the index"
        );

        let second = tool.execute(&ToolContext::default(), args).await.unwrap();
        assert_eq!(first.to_text(), second.to_text());
        assert!(
            second.to_text().contains("Lines 250000-250001"),
            "{}",
            second.to_text()
        );
    }

    #[test]
    fn small_files_use_a_content_addressed_version() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("versioned.txt");
        std::fs::write(&path, "aaaa").unwrap();
        let first = file_version(&path, &std::fs::metadata(&path).unwrap());
        assert!(first.starts_with('c'), "{first}");

        std::fs::write(&path, "bbbb").unwrap();
        let second = file_version(&path, &std::fs::metadata(&path).unwrap());
        assert_ne!(first, second, "same-length content must change the version");
    }

    #[test]
    fn sparse_line_index_resumes_at_the_best_checkpoint() {
        let cache = LineIndexCache::default();
        let path = Path::new("indexed.log");
        let version = "v1";
        let mut checkpoints = vec![
            LineCheckpoint {
                offset: 200,
                line: 20,
            },
            LineCheckpoint { offset: 0, line: 0 },
            LineCheckpoint {
                offset: 100,
                line: 10,
            },
        ];
        cache.store(path, version.to_string(), &mut checkpoints);

        let (offset, line, all) = cache.resume(path, version, 15).expect("resume");
        assert_eq!((offset, line), (100, 10));
        assert_eq!(all.len(), 3);
        let (offset, line, _) = cache.resume(path, version, 21).expect("resume");
        assert_eq!((offset, line), (200, 20));
        assert!(cache.resume(path, "v2", 21).is_none());
    }

    #[test]
    fn a_forward_offset_beyond_the_scan_budget_is_refused() {
        let text = "line\n".repeat(10_000);
        let reader = BufReader::new(Cursor::new(text.into_bytes()));
        let error = stream_lines_with_budget(
            reader,
            Some(9_000),
            2,
            "big.log",
            &ToolContext::default(),
            128,
        )
        .expect_err("scan budget must stop a far offset");
        assert!(error.contains("scanning"), "{error}");
    }

    #[test]
    fn a_cancelled_read_stops_before_scanning_further() {
        let text = "line\n".repeat(10_000);
        let reader = BufReader::new(Cursor::new(text.into_bytes()));
        let ctx = ToolContext::default();
        ctx.cancel_token.cancel();
        let error = stream_lines_with_budget(reader, Some(5), 2, "big.log", &ctx, u64::MAX)
            .expect_err("a cancelled context must abort the scan");
        assert!(error.contains("cancelled"), "{error}");
    }
}
