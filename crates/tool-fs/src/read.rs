//! Unified `read`: one model-facing capability, internal dispatch by content.
//!
//! A path is a path. Whether it names a source file, a directory, an image, a
//! PDF, or a binary is a fact about the bytes, not a decision the model should
//! have to make before calling. See ADR-0176 for the contract and caps, and
//! ADR-0193 for the bounded PDF text path.

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
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

/// Typed parameters for [`ReadTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct ReadArgs {
    #[tool(desc = "Path to a file or directory; relative paths use the workspace root")]
    pub path: String,
    #[tool(
        desc = "1-based line or directory-entry offset. Negative reads the tail of a text file (default 1)."
    )]
    pub offset: Option<i64>,
    #[tool(desc = "Maximum lines or entries to return (default and hard cap: 2000)")]
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

    fn description(&self) -> &str {
        "Read a file or directory. Text files and PDFs are line-numbered and \
         paginated; directories list one level; PNG/JPEG/GIF/WebP images are \
         returned to a vision-capable model. PDF text is extracted locally \
         (no OCR): encrypted or scanned PDFs are reported, not decoded. Binary \
         files are reported, not decoded. Use find_files for recursive globs \
         and search_text for content search."
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

    fn accesses(&self, arguments: &str) -> ToolAccesses {
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
                Some(text) => self.text_output(&text, args.offset, limit, raw_path),
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

impl ReadTool {
    async fn read_path(
        &self,
        path: &Path,
        offset: Option<i64>,
        limit: usize,
        raw_path: &str,
        call_ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let meta = match fs::metadata(path) {
            Ok(meta) => meta,
            Err(err) => {
                return Err(ToolError::execution(
                    self.name(),
                    format!("cannot read `{raw_path}`: {err}"),
                ));
            }
        };

        if meta.is_dir() {
            return self.list_directory(path, offset, limit, raw_path);
        }
        if !meta.is_file() {
            return Err(ToolError::execution(
                self.name(),
                format!("`{raw_path}` is neither a regular file nor a directory"),
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
            return self.image_output(path, mime, meta.len(), raw_path, call_ctx);
        }
        if is_pdf(&first) {
            return self
                .document_output(path, meta.len(), offset, limit, raw_path, call_ctx)
                .await;
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
            return self.text_output(&text, offset, limit, raw_path);
        }

        file.seek(SeekFrom::Start(0)).map_err(|err| {
            ToolError::execution(self.name(), format!("cannot rewind `{raw_path}`: {err}"))
        })?;
        let reader = BufReader::new(file);
        self.stream_text(reader, offset, limit, raw_path)
    }

    fn image_output(
        &self,
        path: &Path,
        mime: &str,
        size: u64,
        raw_path: &str,
        call_ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
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
        let (mime, bytes) = match self
            .normalizer(call_ctx)
            .and_then(|n| n.normalize(&bytes, mime))
        {
            Some(normalized) => (normalized.mime, normalized.data),
            None => (mime.to_string(), bytes),
        };
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(ToolOutput::Image { mime, data })
    }

    async fn document_output(
        &self,
        path: &Path,
        size: u64,
        offset: Option<i64>,
        limit: usize,
        raw_path: &str,
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

        let bytes = {
            let file = fs::File::open(path).map_err(|err| {
                ToolError::execution(self.name(), format!("cannot open `{raw_path}`: {err}"))
            })?;
            // Read at most one byte past the ceiling: metadata is a hint, not a
            // guarantee that the file cannot grow before the read.
            let mut bytes = Vec::with_capacity(size.min(MAX_DOCUMENT_BYTES) as usize);
            file.take(MAX_DOCUMENT_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|err| {
                    ToolError::execution(self.name(), format!("cannot read `{raw_path}`: {err}"))
                })?;
            if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
                return Err(ToolError::execution(
                    self.name(),
                    format!(
                        "`{raw_path}` exceeds the PDF ingest ceiling of {MAX_DOCUMENT_BYTES} bytes"
                    ),
                ));
            }
            bytes
        };

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
        Ok(page.into_output_for(None, raw_path, Some(&document)))
    }

    fn list_directory(
        &self,
        path: &Path,
        offset: Option<i64>,
        limit: usize,
        raw_path: &str,
    ) -> Result<ToolOutput, ToolError> {
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
        for entry in read_dir {
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

    fn stream_text(
        &self,
        reader: impl BufRead,
        offset: Option<i64>,
        limit: usize,
        raw_path: &str,
    ) -> Result<ToolOutput, ToolError> {
        let page = stream_lines(reader, offset, limit, raw_path)
            .map_err(|message| ToolError::execution(self.name(), message))?;
        Ok(page.into_output_for(lang_for(raw_path), raw_path, None))
    }

    fn text_output(
        &self,
        text: &str,
        offset: Option<i64>,
        limit: usize,
        raw_path: &str,
    ) -> Result<ToolOutput, ToolError> {
        let page = page_from_text(text, offset, limit, raw_path)
            .map_err(|message| ToolError::execution(self.name(), message))?;
        Ok(page.into_output_for(lang_for(raw_path), raw_path, None))
    }
}

/// One page of text plus the framing the model needs to continue.
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
        let prefix = match self.total {
            Some(total) if lines > 0 => format!(
                "[Lines {}-{} of {} from `{raw_path}`{}]",
                self.start_line, end_line, total, note
            ),
            Some(total) => format!("[0 lines of {total} from `{raw_path}`{note}]"),
            None if lines > 0 => {
                format!(
                    "[Lines {}-{} from `{raw_path}`{note}]",
                    self.start_line, end_line
                )
            }
            None => format!("[no lines from `{raw_path}`{note}]"),
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
        }
    }
}

fn stream_lines(
    reader: impl BufRead,
    offset: Option<i64>,
    limit: usize,
    raw_path: &str,
) -> Result<TextPage, String> {
    let offset = offset.unwrap_or(1);
    let tail = offset < 0;
    let requested = if tail {
        (offset.unsigned_abs() as usize).min(limit).min(TAIL_LINES)
    } else {
        offset.max(1) as usize
    };

    let mut reader = reader;
    let mut line_no = 0usize;
    let mut page: VecDeque<String> = VecDeque::new();
    let mut bytes = 0usize;
    let mut buf = Vec::new();

    loop {
        buf.clear();
        let read = reader
            .read_until(b'\n', &mut buf)
            .map_err(|err| format!("cannot read `{raw_path}`: {err}"))?;
        if read == 0 {
            break;
        }
        line_no += 1;

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
            // Peek one line to decide whether a next page exists, then stop.
            let mut probe = Vec::new();
            let found = reader
                .read_until(b'\n', &mut probe)
                .map_err(|err| format!("cannot read `{raw_path}`: {err}"))?;
            let next = (found > 0).then_some(line_no + 1);
            let text = page.into_iter().collect::<Vec<_>>().join("\n");
            return Ok(TextPage {
                text,
                start_line: requested.max(1),
                total: None,
                next,
            });
        }
    }

    let start_line = if tail {
        line_no.saturating_sub(page.len()) + 1
    } else {
        requested.max(1)
    };
    let text = page.into_iter().collect::<Vec<_>>().join("\n");
    Ok(TextPage {
        text,
        start_line,
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

fn clamp_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let mut end = line.len();
    for (count, (offset, _)) in line.char_indices().enumerate() {
        if count == MAX_LINE_CHARS {
            end = offset;
            break;
        }
    }
    let mut clamped = line[..end].to_string();
    clamped.push_str(&format!(" … (line truncated to {MAX_LINE_CHARS} chars)"));
    clamped
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
    use serde_json::json;
    use tempfile::tempdir;
    use tool::ToolContext;

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
    use serde_json::json;
    use tempfile::tempdir;
    use tool::ToolContext;

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
