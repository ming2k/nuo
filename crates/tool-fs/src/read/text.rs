//! Bounded UTF-8 text pagination, streaming line reads, reverse-seek tails,
//! and magic-byte binary diagnostics.

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::Path;

use tool::{ExtractedDocument, ToolContext, ToolError, ToolOutput};

use super::line_index::{LINE_INDEX_INTERVAL_BYTES, LineCheckpoint};

/// Hard cap on returned text lines.
pub const MAX_READ_LINES: usize = 2_000;
/// Hard cap on returned text bytes (joined with `\n`).
pub const MAX_READ_BYTES: usize = 50 * 1024;
/// Hard cap on one rendered line. Longer lines are clamped and labelled.
pub const MAX_LINE_CHARS: usize = 2_000;
/// Maximum bytes a single read may scan before returning a page. A 1 GiB file
/// with `offset = 900_000` must not be read end-to-end just to return 50 KiB.
pub const MAX_READ_SCAN_BYTES: u64 = 64 * 1024 * 1024;
/// Files above this size satisfy negative offsets by reverse-seeking from EOF
/// instead of sequentially decoding from byte 0.
pub(super) const TAIL_REVERSE_SEEK_THRESHOLD: u64 = 4 * 1024 * 1024;
/// Cancellation is observed at this line cadence inside the blocking scan.
const CANCEL_CHECK_LINES: usize = 512;
/// Maximum bytes retained from one line. The model-facing clamp is 2000
/// characters; 4 bytes per character covers the worst-case UTF-8 line without
/// letting a minified single-line file exhaust memory.
const MAX_LINE_BYTES: usize = MAX_LINE_CHARS * 4;
/// Block size for reverse-seek tail reads from EOF.
const TAIL_CHUNK_BYTES: usize = 64 * 1024;
/// Stop reverse-scanning an unbroken single-line file once this many bytes
/// have been read without encountering a single newline so memory stays bounded.
const MAX_REVERSE_NO_NEWLINE_BYTES: u64 = 256 * 1024;

/// Where a NUL byte was detected during file inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BinaryLocation {
    Prefix(usize),
    Line(usize),
    NearEof,
}

/// One page of text plus the framing the model needs to continue.
#[derive(Debug)]
pub(super) struct TextPage {
    pub(super) text: String,
    pub(super) start_line: usize,
    pub(super) total: Option<usize>,
    pub(super) next: Option<usize>,
    /// Set when a tail was produced by reverse-seeking from EOF on a large file
    /// whose total line count is unknown (`start_line == 0`).
    pub(super) tail_of_bytes: Option<u64>,
}

impl TextPage {
    pub(super) fn into_output_for(
        self,
        lang: Option<String>,
        raw_path: &str,
        document: Option<&ExtractedDocument>,
        window_pages: Option<(usize, usize)>,
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
            let span = match window_pages {
                Some((first, last)) if first == last => format!(", showing page {first}"),
                Some((first, last)) => format!(", showing pages {first}-{last}"),
                None => String::new(),
            };
            format!(" — PDF text, {pages}{span}")
        });
        let note = document_note.as_deref().unwrap_or("");
        let version_note = version
            .as_deref()
            .map(|version| format!("; version {version}"))
            .unwrap_or_default();
        let prefix = if let Some(file_bytes) = self.tail_of_bytes {
            format!(
                "[Last {lines} line{} of ~{} from `{raw_path}`]{version_note}",
                if lines == 1 { "" } else { "s" },
                format_bytes(file_bytes),
            )
        } else {
            match self.total {
                Some(total) if lines > 0 => {
                    format!(
                        "[Lines {}-{} of {} from `{raw_path}`{}]",
                        self.start_line, end_line, total, note
                    ) + &version_note
                }
                Some(total) => {
                    format!("[0 lines of {total} from `{raw_path}`{note}{version_note}]")
                }
                None if lines > 0 => {
                    format!(
                        "[Lines {}-{} from `{raw_path}`{note}]",
                        self.start_line, end_line
                    ) + &version_note
                }
                None => format!("[no lines from `{raw_path}`{note}{version_note}]"),
            }
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
            (None, _) => document.and_then(|doc| {
                doc.next_page.map(|next_page| {
                    format!("[more PDF pages remain — read with pages=\"{next_page}-\"]")
                })
            }),
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

pub(super) fn scan_budget_message(raw_path: &str) -> String {
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
pub(super) fn read_bounded_line(
    reader: &mut impl BufRead,
    out: &mut Vec<u8>,
    raw_path: &str,
    header_sample: &[u8],
    file_size: u64,
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
            return Err(binary_file_error(
                raw_path,
                header_sample,
                file_size,
                BinaryLocation::Line(line_no),
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

#[allow(clippy::too_many_arguments)]
pub(super) fn stream_lines_indexed(
    reader: impl BufRead,
    offset: Option<i64>,
    limit: usize,
    raw_path: &str,
    header_sample: &[u8],
    file_size: u64,
    call_ctx: &ToolContext,
    scan_budget: u64,
    initial_line: usize,
    base_offset: u64,
    checkpoints: &mut Vec<LineCheckpoint>,
) -> Result<TextPage, String> {
    let offset = offset.unwrap_or(1);
    let tail = offset < 0;
    let requested = if tail {
        usize::try_from(offset.unsigned_abs())
            .unwrap_or(usize::MAX)
            .min(limit)
    } else {
        usize::try_from(offset.max(1)).unwrap_or(usize::MAX)
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
        let read = read_bounded_line(
            &mut reader,
            &mut buf,
            raw_path,
            header_sample,
            file_size,
            line_no + 1,
            call_ctx,
        )?;
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
            return Err(binary_file_error(
                raw_path,
                header_sample,
                file_size,
                BinaryLocation::Line(line_no),
            ));
        }
        if tail {
            let line = decode_line(&buf, raw_path, line_no)?;
            let added = line.len() + usize::from(!page.is_empty());
            page.push_back(line);
            bytes = bytes.saturating_add(added);
            while page.len() > requested || (page.len() > 1 && bytes > MAX_READ_BYTES) {
                if let Some(evicted) = page.pop_front() {
                    let removed = evicted.len() + usize::from(!page.is_empty());
                    bytes = bytes.saturating_sub(removed);
                }
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
                tail_of_bytes: None,
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
                tail_of_bytes: None,
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
        tail_of_bytes: None,
    })
}

/// Read the tail of an arbitrarily large file by seeking backwards from EOF
/// (`[INV-READ-TAIL-01]`, ADR-0257).
///
/// Only enough chunks to satisfy `min(|offset|, limit)` lines or
/// [`MAX_READ_BYTES`] are read; an unbroken single-line file stops after
/// [`MAX_REVERSE_NO_NEWLINE_BYTES`] so peak memory stays tightly bounded.
/// When the scan stops before byte 0, absolute line numbers are unknown
/// (`start_line == 0`) and `tail_of_bytes` is set so the header honestly
/// reports `Last N lines of ~X MiB`.
#[allow(clippy::too_many_arguments)]
pub(super) fn tail_from_eof(
    file: &mut fs::File,
    file_len: u64,
    requested_lines: usize,
    raw_path: &str,
    header_sample: &[u8],
    call_ctx: &ToolContext,
    scan_budget: u64,
) -> Result<TextPage, String> {
    if file_len == 0 || requested_lines == 0 {
        return Ok(TextPage {
            text: String::new(),
            start_line: 1,
            total: Some(0),
            next: None,
            tail_of_bytes: None,
        });
    }

    let mut cursor = file_len;
    let mut scanned = 0u64;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut newlines_seen = 0usize;
    let mut completed_line_lower_bound = 0usize;
    let mut current_line_raw_len = 0usize;
    let mut seen_trailing_newline = false;

    while cursor > 0 && scanned < scan_budget {
        if call_ctx.is_cancelled() {
            return Err("tool `read` execution was cancelled".to_string());
        }
        let budget_left = (scan_budget - scanned) as usize;
        let to_read = (cursor.min(TAIL_CHUNK_BYTES as u64) as usize).min(budget_left);
        if to_read == 0 {
            break;
        }
        cursor -= to_read as u64;
        file.seek(SeekFrom::Start(cursor))
            .map_err(|err| format!("cannot seek `{raw_path}` to byte {cursor}: {err}"))?;
        let mut chunk = vec![0u8; to_read];
        file.read_exact(&mut chunk)
            .map_err(|err| format!("cannot read `{raw_path}`: {err}"))?;
        scanned = scanned.saturating_add(to_read as u64);

        if chunk.contains(&0) {
            return Err(binary_file_error(
                raw_path,
                header_sample,
                file_len,
                BinaryLocation::NearEof,
            ));
        }

        // Track newline count and a strict lower bound on rendered bytes of
        // already-complete lines from EOF so huge tail limits stop as soon as
        // MAX_READ_BYTES is guaranteed to be filled.
        for &byte in chunk.iter().rev() {
            if byte == b'\n' {
                newlines_seen += 1;
                if !seen_trailing_newline && scanned == to_read as u64 && current_line_raw_len == 0
                {
                    seen_trailing_newline = true;
                } else {
                    let rendered_min = current_line_raw_len
                        .saturating_sub(1)
                        .min(MAX_LINE_CHARS)
                        .saturating_add(1);
                    completed_line_lower_bound =
                        completed_line_lower_bound.saturating_add(rendered_min);
                }
                current_line_raw_len = 0;
            } else {
                current_line_raw_len = current_line_raw_len.saturating_add(1);
            }
        }
        chunks.push(chunk);

        let complete_lines_ready = if seen_trailing_newline {
            newlines_seen >= requested_lines + 2
        } else {
            newlines_seen > requested_lines
        };
        if complete_lines_ready
            || (newlines_seen >= 2 && completed_line_lower_bound > MAX_READ_BYTES)
            || (newlines_seen == 0 && scanned >= MAX_REVERSE_NO_NEWLINE_BYTES)
        {
            break;
        }
    }

    let total_buf_len: usize = chunks.iter().map(Vec::len).sum();
    let mut combined = Vec::with_capacity(total_buf_len);
    for chunk in chunks.into_iter().rev() {
        combined.extend_from_slice(&chunk);
    }

    let reached_start = cursor == 0;
    let mut clipped_oversized_tail = false;
    let slice: &[u8] = if reached_start {
        combined.as_slice()
    } else if let Some(first_nl) = combined.iter().position(|&b| b == b'\n') {
        &combined[first_nl + 1..]
    } else {
        // Single giant line with no newline inside the reverse scan window.
        clipped_oversized_tail = true;
        let mut start = combined.len().saturating_sub(MAX_LINE_BYTES);
        while start < combined.len() && (combined[start] & 0b1100_0000) == 0b1000_0000 {
            start += 1;
        }
        &combined[start..]
    };

    // Match `str::lines()` semantics: `""` yields 0 lines while `"\n"` yields 1 empty line.
    let raw_lines: Vec<&[u8]> = if slice.is_empty() {
        Vec::new()
    } else {
        let body = slice.strip_suffix(b"\n").unwrap_or(slice);
        body.split(|&b| b == b'\n').collect()
    };

    let mut page = Vec::new();
    let mut bytes = 0usize;
    for (rev_idx, raw_line) in raw_lines.iter().rev().enumerate() {
        if page.len() >= requested_lines {
            break;
        }
        let truncated_raw = clipped_oversized_tail || raw_line.len() > MAX_LINE_BYTES;
        let line_slice = if raw_line.len() > MAX_LINE_BYTES {
            let mut end = MAX_LINE_BYTES;
            while end > 0 && std::str::from_utf8(&raw_line[..end]).is_err() {
                end -= 1;
            }
            &raw_line[..end]
        } else {
            raw_line
        };
        let is_first_file_line = reached_start && rev_idx + 1 == raw_lines.len();
        let line_no_for_diag = if reached_start {
            raw_lines.len() - rev_idx
        } else {
            0
        };
        let line = decode_line_with_bom_flag(
            line_slice,
            raw_path,
            line_no_for_diag,
            is_first_file_line,
            truncated_raw,
        )?;
        let cost = line.len() + usize::from(!page.is_empty());
        if !page.is_empty() && bytes + cost > MAX_READ_BYTES {
            break;
        }
        bytes += cost;
        page.push(line);
    }
    page.reverse();

    if reached_start {
        let total = raw_lines.len();
        let start_line = total.saturating_sub(page.len()) + 1;
        Ok(TextPage {
            text: page.join("\n"),
            start_line,
            total: Some(total),
            next: None,
            tail_of_bytes: None,
        })
    } else {
        Ok(TextPage {
            text: page.join("\n"),
            start_line: 0,
            total: None,
            next: None,
            tail_of_bytes: Some(file_len),
        })
    }
}

pub(super) fn page_from_text(
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
        let requested = usize::try_from(offset.unsigned_abs())
            .unwrap_or(usize::MAX)
            .min(limit);
        let mut page = Vec::new();
        let mut bytes = 0usize;
        for line in lines.iter().rev() {
            if page.len() >= requested {
                break;
            }
            let clamped = clamp_line(line);
            let cost = clamped.len() + usize::from(!page.is_empty());
            if !page.is_empty() && bytes + cost > MAX_READ_BYTES {
                break;
            }
            bytes += cost;
            page.push(clamped);
        }
        page.reverse();
        let start = total.saturating_sub(page.len());
        return Ok(TextPage {
            text: page.join("\n"),
            start_line: start + 1,
            total: Some(total),
            next: None,
            tail_of_bytes: None,
        });
    }

    let start = (usize::try_from(offset.max(1)).unwrap_or(usize::MAX) - 1).min(total);
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
        tail_of_bytes: None,
    })
}

fn decode_line(buf: &[u8], raw_path: &str, line_no: usize) -> Result<String, String> {
    decode_line_with_bom_flag(buf, raw_path, line_no, line_no == 1, false)
}

fn decode_line_with_bom_flag(
    buf: &[u8],
    raw_path: &str,
    line_no: usize,
    strip_bom: bool,
    force_truncated: bool,
) -> Result<String, String> {
    let bytes = buf.strip_suffix(b"\n").unwrap_or(buf);
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|err| {
        if line_no > 0 {
            format!(
                "`{raw_path}` is not valid UTF-8 at line {line_no} \
                 (byte offset {} within the line): {err}",
                err.valid_up_to()
            )
        } else {
            format!(
                "`{raw_path}` is not valid UTF-8 (byte offset {} within tail line): {err}",
                err.valid_up_to()
            )
        }
    })?;
    let text = if strip_bom {
        text.strip_prefix('\u{FEFF}').unwrap_or(text)
    } else {
        text
    };
    let clamped = clamp_line(text);
    if force_truncated && !clamped.contains("line truncated to") {
        Ok(format!(
            "{clamped} … (line truncated to {MAX_LINE_CHARS} chars)"
        ))
    } else {
        Ok(clamped)
    }
}

pub(super) fn decode_utf8(
    bytes: &[u8],
    raw_path: &str,
    start_line: usize,
) -> Result<String, ToolError> {
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

pub(super) fn lang_for(raw_path: &str) -> Option<String> {
    Path::new(raw_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
}

/// Produce a diagnostic error for a non-text binary file using header magic
/// bytes and file size so the model does not waste a turn guessing the format.
pub(super) fn binary_file_error(
    raw_path: &str,
    header_sample: &[u8],
    file_size: u64,
    location: BinaryLocation,
) -> String {
    let size = format_bytes(file_size);
    if let Some((kind, hint)) = identify_binary(header_sample) {
        return format!(
            "`{raw_path}` is a binary file ({kind}, {size}); {hint}. \
             `read` supports UTF-8 text, images (PNG/JPEG/GIF/WebP), and text-layer PDFs."
        );
    }
    let where_found = match location {
        BinaryLocation::Prefix(len) => format!("NUL byte in the first {len} bytes"),
        BinaryLocation::Line(line) => format!("NUL byte on line {line}"),
        BinaryLocation::NearEof => "NUL byte near EOF".to_string(),
    };
    format!(
        "`{raw_path}` is a binary file ({size}, {where_found}); inspect with `file` or `xxd | head` \
         via `execute_command`. `read` supports UTF-8 text, images (PNG/JPEG/GIF/WebP), and text-layer PDFs."
    )
}

fn identify_binary(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\xFF\xFE") && !bytes.starts_with(b"\xFF\xFE\x00\x00") {
        return Some((
            "UTF-16LE text with BOM",
            "convert to UTF-8 with `iconv -f UTF-16LE -t UTF-8` via `execute_command`",
        ));
    }
    if bytes.starts_with(b"\xFE\xFF") {
        return Some((
            "UTF-16BE text with BOM",
            "convert to UTF-8 with `iconv -f UTF-16BE -t UTF-8` via `execute_command`",
        ));
    }
    if bytes.starts_with(b"SQLite format 3\0") {
        return Some((
            "SQLite database",
            "inspect tables with `sqlite3` via `execute_command`",
        ));
    }
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        return Some((
            "ZIP archive (or Office/JAR package)",
            "list entries with `unzip -l` via `execute_command`",
        ));
    }
    if bytes.starts_with(b"\x1F\x8B") {
        return Some((
            "gzip archive",
            "inspect with `gzip -dc | head` via `execute_command`",
        ));
    }
    if bytes.starts_with(b"\x28\xB5\x2F\xFD") {
        return Some((
            "zstd archive",
            "inspect with `zstdcat | head` via `execute_command`",
        ));
    }
    if bytes.starts_with(b"\xFD7zXZ\x00") {
        return Some((
            "xz archive",
            "inspect with `xzcat | head` via `execute_command`",
        ));
    }
    if bytes.starts_with(b"BZh") {
        return Some((
            "bzip2 archive",
            "inspect with `bzcat | head` via `execute_command`",
        ));
    }
    if bytes.len() > 262 && &bytes[257..262] == b"ustar" {
        return Some((
            "TAR archive",
            "list entries with `tar -tf` via `execute_command`",
        ));
    }
    if bytes.starts_with(b"\x7FELF") {
        return Some((
            "ELF executable/library",
            "inspect with `file` or `readelf -h` via `execute_command`",
        ));
    }
    if bytes.starts_with(b"\xFE\xED\xFA\xCE")
        || bytes.starts_with(b"\xCE\xFA\xED\xFE")
        || bytes.starts_with(b"\xFE\xED\xFA\xCF")
        || bytes.starts_with(b"\xCF\xFA\xED\xFE")
        || bytes.starts_with(b"\xCA\xFE\xBA\xBE")
    {
        return Some((
            "Mach-O executable/library",
            "inspect with `file` or `otool` via `execute_command`",
        ));
    }
    if bytes.starts_with(b"MZ") {
        return Some((
            "Windows PE executable/DLL",
            "inspect with `file` via `execute_command`",
        ));
    }
    if bytes.starts_with(b"\0asm") {
        return Some((
            "WebAssembly module",
            "inspect with `wasm-tools` or `file` via `execute_command`",
        ));
    }
    if bytes.starts_with(b"wOFF") || bytes.starts_with(b"wOF2") || bytes.starts_with(b"OTTO") {
        return Some((
            "font file",
            "inspect metadata with `file` via `execute_command`",
        ));
    }
    if bytes.starts_with(b"OggS")
        || bytes.starts_with(b"fLaC")
        || bytes.starts_with(b"ID3")
        || (bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE")
    {
        return Some((
            "audio file",
            "inspect metadata with `file` via `execute_command`",
        ));
    }
    None
}

pub(super) fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
pub(super) fn stream_lines_with_budget(
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
        &[],
        0,
        call_ctx,
        scan_budget,
        0,
        0,
        &mut checkpoints,
    )
}
