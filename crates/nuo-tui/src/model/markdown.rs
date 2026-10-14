//! Markdown block and inline parsing for transcript messages
//! (ADR-0001 structured output): blocks, tables, links, code ranges.
//! Pure text-in / structured-out; no rendering concerns here.

use super::document::{Block, CodeRange, Inline, InlineScan, LinkRange, TableAlignment};

type ParsedLink = ((usize, usize), (usize, usize), String);

pub fn parse_blocks(text: &str) -> Vec<Block> {
    parse_blocks_tracked(text).0
}

/// Resume metadata from a tracked parse (ADR-0184): where a later suffix
/// parse must resume, and how many trailing blocks belong to the still-open
/// final construct.
///
/// Markdown blocks are line-oriented: once a construct is terminated by an
/// explicit terminator (blank line, closing fence, non-continuation line),
/// it can never be modified by future input. Only the construct that was
/// ended by EOF (a streaming tail) is still open — appended deltas can
/// extend it, join further blocks to its run, or close it and open new
/// constructs. Re-parsing from `resume_offset` therefore reproduces exactly
/// the last `live_len` blocks plus everything after it, while every block
/// before that region is frozen.
pub(crate) struct ParseResume {
    /// Byte offset in the parsed text where the open final construct begins;
    /// `text.len()` when every construct is closed.
    pub resume_offset: usize,
    /// Number of trailing blocks produced by that open construct.
    pub live_len: usize,
}

pub(crate) fn parse_blocks_tracked(text: &str) -> (Vec<Block>, ParseResume) {
    parse_blocks_markdown_tracked(text)
}

/// Parse plain-text input (user messages) into blocks without any markdown
/// interpretation. The entire text becomes a single [`Block::Text`] so it
/// renders as one continuous verbatim panel; line breaks are preserved by the
/// renderer's wrapper rather than being collapsed by a markdown parser.
pub fn parse_blocks_plain(text: &str) -> Vec<Block> {
    if text.is_empty() {
        return Vec::new();
    }
    vec![Block::Text(Inline::plain(text.to_string()))]
}

pub(crate) fn parse_blocks_markdown_tracked(text: &str) -> (Vec<Block>, ParseResume) {
    let mut blocks = Vec::new();
    // Each line with its byte offset in `text`, so the live construct's
    // resume offset can be recorded when it starts.
    let mut lines: Vec<(&str, usize)> = Vec::with_capacity(text.len() / 8 + 1);
    let mut line_offset = 0usize;
    for line in text.split('\n') {
        lines.push((line, line_offset));
        line_offset += line.len() + 1;
    }
    let mut i = 0;

    // Whether every line is terminated by a newline (the final split segment
    // is the empty trailing line). A construct whose last line lacks the
    // trailing newline is EOF-ended: the line can still extend, so the
    // construct stays open to future deltas no matter how "complete" its
    // syntax looks right now.
    let ends_with_newline = text.is_empty() || text.ends_with('\n');
    // The trailing empty split segment (present iff `ends_with_newline`) is
    // the line terminator of the final line, not a real blank line — it does
    // not terminate a construct. A run that "breaks" on it is EOF-ended and
    // stays live (a later delta may continue the run).
    let trailing_blank =
        |idx: usize, lines: &[(&str, usize)]| ends_with_newline && idx + 1 == lines.len();

    // The construct that consumed the most recent line, as (start byte
    // offset, first block index at construct start). `Some` means the
    // construct was ended by EOF (still open to future deltas); explicit
    // terminators clear it. Paragraphs set it on their first prose line.
    let mut live: Option<(usize, usize)> = None;

    // Accumulator for a paragraph: the prose lines (already stripped of their
    // block-prefix), joined with soft-break→space / hard-break→`\n` rules.
    // Once a paragraph is flushed we scan the resulting string for inline
    // `code` / `**bold**` runs and record their byte ranges.
    let mut para: Vec<String> = Vec::new();
    let mut para_hard: Vec<bool> = Vec::new(); // hard-break before this line?

    // (List items are pushed directly during the list run — adjacent items
    // share no Break thanks to push_block's ListItem-pair rule.)

    let flush_para =
        |para: &mut Vec<String>, para_hard: &mut Vec<bool>, blocks: &mut Vec<Block>| {
            if para.is_empty() {
                return;
            }
            // Join lines: a soft break inserts a space; a hard break (the *previous*
            // line ended with a two-space marker) inserts a literal "\n".
            let mut content = String::new();
            for (idx, line) in para.iter().enumerate() {
                if idx > 0 {
                    content.push(if para_hard[idx - 1] { '\n' } else { ' ' });
                }
                content.push_str(line);
            }
            push_block(blocks, Block::Text(Inline::scanned(&content)));
            para.clear();
            para_hard.clear();
        };

    while i < lines.len() {
        let (line, line_off) = lines[i];
        let trimmed = line.trim_start();

        // Fenced code block
        if let Some(rest) = trimmed.strip_prefix("```") {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            live = Some((line_off, blocks.len()));
            let lang = rest.trim().to_string();
            let language = if lang.is_empty() { None } else { Some(lang) };
            let mut content = String::new();
            i += 1;
            while i < lines.len() && !lines[i].0.trim_start().starts_with("```") {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(lines[i].0);
                i += 1;
            }
            // skip closing fence (if present). The close is only frozen when
            // its line is newline-terminated: an EOF-ended ```` ``` ```` line
            // can still extend into ```` ```x ```` (a fresh fence opener whose
            // suffix the full parser would swallow), so the fence stays live
            // with its resume at the fence start.
            if i < lines.len() {
                i += 1;
                if i < lines.len() || ends_with_newline {
                    live = None; // explicitly closed — frozen
                }
            }
            push_block(&mut blocks, Block::Code { language, content });
            continue;
        }

        // Display math block
        if trimmed == "$$" || trimmed.starts_with("$$") || trimmed == "\\[" {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            live = Some((line_off, blocks.len()));
            let closing = if trimmed.starts_with("$$") {
                "$$"
            } else {
                "\\]"
            };
            let mut content = String::new();
            if let Some(rest) = trimmed.strip_prefix("$$") {
                if let Some(end) = rest.find("$$") {
                    content.push_str(rest[..end].trim());
                    i += 1;
                    push_block(&mut blocks, Block::Math { content });
                    // An EOF-ended opening line can still swallow more of the
                    // line into the discarded post-``$$`` tail, so it stays
                    // live unless newline-terminated.
                    if i < lines.len() || ends_with_newline {
                        live = None; // closed on the opening line
                    }
                    continue;
                }
                let rest = rest.trim();
                if !rest.is_empty() {
                    content.push_str(rest);
                }
            }
            i += 1;
            while i < lines.len() {
                let candidate = lines[i].0.trim();
                if candidate == closing {
                    i += 1;
                    if i < lines.len() || ends_with_newline {
                        live = None; // explicitly closed — frozen
                    }
                    break;
                }
                if closing == "$$"
                    && let Some(end) = candidate.find("$$")
                {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(candidate[..end].trim_end());
                    i += 1;
                    if i < lines.len() || ends_with_newline {
                        live = None;
                    }
                    break;
                }
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(lines[i].0.trim_end());
                i += 1;
            }
            push_block(&mut blocks, Block::Math { content });
            continue;
        }

        // Horizontal rule
        if is_rule(trimmed) {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            live = Some((line_off, blocks.len()));
            push_block(&mut blocks, Block::Rule);
            // Closed only when the line is terminated: at EOF the final line
            // can still extend (`--` → `---`), so the construct stays open.
            i += 1;
            if i < lines.len() || ends_with_newline {
                live = None;
            }
            continue;
        }

        // Heading
        if let Some((level, content_line)) = parse_heading(trimmed) {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            live = Some((line_off, blocks.len()));
            push_block(
                &mut blocks,
                Block::Heading {
                    level,
                    inline: Inline::scanned(content_line),
                },
            );
            // Same EOF rule as the horizontal rule: `# Hea` is a live
            // heading until a newline (or a later delta) settles the line.
            i += 1;
            if i < lines.len() || ends_with_newline {
                live = None;
            }
            continue;
        }

        // Blockquote
        if let Some(content_line) = parse_quote(trimmed) {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            live = Some((line_off, blocks.len()));
            // Collect consecutive quote lines.
            let mut q_lines: Vec<String> = Vec::new();
            let mut q_hard: Vec<bool> = Vec::new();
            q_lines.push(content_line.to_string());
            q_hard.push(false);
            i += 1;
            while i < lines.len() {
                let t = lines[i].0.trim_start();
                if let Some(c) = parse_quote(t) {
                    let hard = q_lines.last().is_some_and(|line| line_ends_hard(line));
                    q_hard.push(hard);
                    q_lines.push(c.to_string());
                    i += 1;
                } else {
                    if !trailing_blank(i, &lines) {
                        live = None; // terminated by a real non-quote line
                    }
                    break;
                }
            }
            let mut content = String::new();
            for (idx, l) in q_lines.iter().enumerate() {
                if idx > 0 {
                    content.push(if q_hard[idx] { '\n' } else { ' ' });
                }
                // Strip the trailing two-space/tab hard-break marker from the
                // stored text exactly as the paragraph path does; `q_hard`
                // already recorded that this line ended in a hard break, so the
                // marker itself must not survive into the content (it used to
                // leak two trailing spaces into copy and rendering).
                content.push_str(l.trim_end_matches([' ', '\t']));
            }
            push_block(&mut blocks, Block::Quote(Inline::scanned(&content)));
            continue;
        }

        // List item
        if parse_list_item(trimmed).is_some() {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            live = Some((line_off, blocks.len()));
            // Collect consecutive list items as a group; push_block's
            // ListItem↔ListItem rule keeps them tight (no Break between).
            while i < lines.len() {
                let t = lines[i].0.trim_start();
                if let Some((m, c, ch)) = parse_list_item(t) {
                    push_block(
                        &mut blocks,
                        Block::ListItem {
                            inline: Inline::scanned(c),
                            ordered: m,
                            depth: 0,
                            checked: ch,
                        },
                    );
                    i += 1;
                } else {
                    if !trailing_blank(i, &lines) {
                        live = None; // terminated by a real non-item line
                    }
                    break;
                }
            }
            continue;
        }

        // Table (GFM: | ... | lines with a separator row)
        if trimmed.starts_with('|')
            && i + 1 < lines.len()
            && is_table_separator(lines[i + 1].0.trim())
        {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            live = Some((line_off, blocks.len()));
            let mut table = TableAccumulator::default();
            // Header row
            let header_cells = split_table_row(trimmed);
            table.header = header_cells.clone();
            // Alignment from separator
            table.aligns = parse_table_aligns(lines[i + 1].0.trim());
            i += 2;
            // Body rows
            while i < lines.len() {
                let t = lines[i].0.trim();
                if t.starts_with('|') && !is_table_separator(t) {
                    let cells = split_table_row(t);
                    table.rows.push(cells);
                    i += 1;
                } else {
                    if !trailing_blank(i, &lines) {
                        live = None; // terminated by a real non-row line
                    }
                    break;
                }
            }
            // GFM tables define the column count from the header: a body row
            // with fewer cells is padded with empty cells, and a row with more
            // is truncated. Normalizing here establishes the invariant that
            // every row in `Block::Table` has exactly `headers.len()` cells, so
            // every consumer (live renderer, selection copy, hit-testing) can
            // index a row by column without per-access bounds checks. Without
            // this, a ragged body row panicked the adaptive renderer (index out
            // of bounds in `build_table_render`).
            normalize_table_rows(&table.header, &mut table.rows);
            let rendered = table.render();
            if !rendered.is_empty() {
                push_block(
                    &mut blocks,
                    Block::Table {
                        headers: table.header,
                        rows: table.rows,
                        aligns: table.aligns,
                        rendered,
                    },
                );
            }
            continue;
        }

        // Blank line: paragraph break
        if trimmed.is_empty() {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            if !trailing_blank(i, &lines) {
                live = None; // the paragraph is terminated — frozen
            }
            i += 1;
            continue;
        }

        // Ordinary prose line
        // A trailing two-space (or tab) marker is a hard line break. Strip it
        // from the stored text; the `para_hard` flag records that this line
        // ends in a hard break so the join inserts a literal "\n" before the
        // *next* line.
        if para.is_empty() {
            live = Some((line_off, blocks.len()));
        }
        let hard = line_ends_hard(line);
        let stored = trimmed.trim_end_matches([' ', '\t']);
        para.push(stored.to_string());
        para_hard.push(hard);
        i += 1;
    }

    flush_para(&mut para, &mut para_hard, &mut blocks);

    // Strip trailing Breaks (a trailing blank line should not produce one).
    while matches!(blocks.last(), Some(Block::Break)) {
        blocks.pop();
    }
    let resume = match live {
        Some((offset, first_block)) => ParseResume {
            resume_offset: offset,
            live_len: blocks.len().saturating_sub(first_block),
        },
        None => ParseResume {
            resume_offset: text.len(),
            live_len: 0,
        },
    };
    (blocks, resume)
}

/// Whether a line is a thematic break (`---`, `***`, `___` with ≥3 same chars).
fn is_rule(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 3 {
        return false;
    }
    let Some(c) = s.chars().next() else {
        return false;
    };
    if c != '-' && c != '*' && c != '_' {
        return false;
    }
    s.chars().all(|ch| ch == c) && s.chars().count() >= 3
}

/// Parse a heading line `# title` … `###### title`. Returns `(level, content)`
/// where `content` still carries any inline formatting markers.
fn parse_heading(s: &str) -> Option<(u8, &str)> {
    let hashes = s.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &s[hashes..];
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    if rest.is_empty() && !s[..hashes].chars().all(|c| c == '#') {
        return None;
    }
    Some((hashes as u8, rest))
}

/// Parse a blockquote line `> text`. Supports `> text` and `>text`.
fn parse_quote(s: &str) -> Option<&str> {
    s.strip_prefix('>')
        .map(|rest| rest.strip_prefix(' ').unwrap_or(rest))
}

/// Parse a list-item line. Returns `(ordered_marker, content, checked)`.
/// `ordered_marker` is `Some(n)` for `N. `, `None` for bullet (`-`/`*`/`+ `).
/// `checked` is `Some(bool)` for task-list items `- [x]`/`- [ ]`.
fn parse_list_item(s: &str) -> Option<(Option<u64>, &str, Option<bool>)> {
    // Task list: - [x] / - [ ] / * [x] / + [ ]
    if let Some(after_bullet) = strip_bullet(s) {
        let after = after_bullet.trim_start_matches(' ');
        if let Some(rest) = after.strip_prefix("[") {
            let rest_first = rest.chars().next();
            let checked = match rest_first {
                Some('x') | Some('X') => Some(true),
                Some(' ') => Some(false),
                _ => None,
            };
            if checked.is_some()
                && let Some(content) = rest[1..].strip_prefix("]")
            {
                return Some((None, content.trim_start(), checked));
            }
        }
        return Some((None, after, None));
    }
    // Ordered list: 1. / 2. …
    if let Some((num, rest)) = parse_ordered(s) {
        let rest = rest.trim_start_matches(' ');
        // Ordered task list: 1. [x] (rare, but handle it)
        if let Some(r) = rest.strip_prefix("[") {
            let checked = match r.chars().next() {
                Some('x') | Some('X') => Some(true),
                Some(' ') => Some(false),
                _ => None,
            };
            if checked.is_some()
                && let Some(content) = r[1..].strip_prefix("]")
            {
                return Some((Some(num), content.trim_start(), checked));
            }
        }
        return Some((Some(num), rest, None));
    }
    None
}

/// Strip a bullet prefix (`-`/`*`/`+`), returning the remainder.
fn strip_bullet(s: &str) -> Option<&str> {
    if let Some(rest) = s.strip_prefix("- ") {
        Some(rest)
    } else if let Some(rest) = s.strip_prefix("* ") {
        Some(rest)
    } else if let Some(rest) = s.strip_prefix("+ ") {
        Some(rest)
    } else if let Some(rest) = s.strip_prefix("-\t") {
        Some(rest)
    } else {
        None
    }
}

/// Parse an ordered-list marker `N. ` or `N) `, returning `(N, remainder)`.
fn parse_ordered(s: &str) -> Option<(u64, &str)> {
    let digits_end = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits_end == 0 {
        return None;
    }
    let rest = &s[digits_end..];
    if let Some(after) = rest.strip_prefix(". ") {
        let n: u64 = s[..digits_end].parse().ok()?;
        return Some((n, after));
    }
    if let Some(after) = rest.strip_prefix(") ") {
        let n: u64 = s[..digits_end].parse().ok()?;
        return Some((n, after));
    }
    None
}

/// Whether a line ends with a hard break (≥2 trailing spaces). The two-space
/// marker is stripped from the content before this is called on the stored
/// string, so we check the *original* line; callers pass the raw line.
fn line_ends_hard(line: &str) -> bool {
    line.ends_with("  ") || line.ends_with("\t")
}

/// Is this line a GFM table separator (`| --- | :--: | ---: |`)?
fn is_table_separator(s: &str) -> bool {
    if !s.contains('-') {
        return false;
    }
    let stripped = s.trim_matches('|').trim();
    if stripped.is_empty() {
        return false;
    }
    // Each cell must contain at least one `-`, only `-`,`:`,and spaces.
    stripped.split('|').all(|cell| {
        let c = cell.trim();
        !c.is_empty() && c.contains('-') && c.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ')
    })
}

/// Parse alignment markers from a separator row into `TableAlignment`s.
fn parse_table_aligns(sep: &str) -> Vec<TableAlignment> {
    sep.trim_matches('|')
        .split('|')
        .map(|cell| {
            let c = cell.trim();
            let left = c.starts_with(':');
            let right = c.ends_with(':');
            match (left, right) {
                (true, true) => TableAlignment::Center,
                (true, false) => TableAlignment::Left,
                (false, true) => TableAlignment::Right,
                (false, false) => TableAlignment::None,
            }
        })
        .collect()
}

/// Split a `| a | b | c |` row into trimmed cell strings.
fn split_table_row(line: &str) -> Vec<String> {
    let line = line.trim();
    // Strip leading/trailing `|`.
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').unwrap_or(line);
    line.split('|').map(|c| c.trim().to_string()).collect()
}

/// Scan a prose string for inline code, bold, math, and links. Delimiters are
/// kept in `content`; renderers decide which marker bytes are visually elided.
pub fn scan_inline(content: &str) -> InlineScan {
    let bytes = content.as_bytes();
    let mut out = InlineScan::default();
    let mut i = 0usize;

    while i < bytes.len() {
        // Inline code: a run of backticks, closed by the same number. Nothing
        // inside code is scanned for math/links.
        if bytes[i] == b'`' {
            let tick_count = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            let close_start = i + tick_count;
            if let Some(rel) = find_backtick_run(&content[close_start..], tick_count) {
                let end = close_start + rel + tick_count;
                out.code_ranges.push((i, end));
                i = end;
                continue;
            }
        }

        if let Some((range, label_range, url)) = parse_markdown_link(content, i) {
            out.link_ranges.push(LinkRange {
                range,
                label_range,
                url,
            });
            i = range.1;
            continue;
        }
        if let Some((range, label_range, url)) = parse_tex_link(content, i) {
            out.link_ranges.push(LinkRange {
                range,
                label_range,
                url,
            });
            i = range.1;
            continue;
        }
        if let Some((start, end, url)) = parse_bare_url(content, i) {
            out.link_ranges.push(LinkRange {
                range: (start, end),
                label_range: (start, end),
                url,
            });
            i = end;
            continue;
        }

        // Inline math: `$…$` or `\(…\)`. Keep this after links so URLs with `$`
        // query fragments are not split before link detection gets a chance.
        if bytes[i] == b'$'
            && !starts_with_at(content, i, "$$")
            && let Some(rel) = content[i + 1..].find('$')
        {
            let end = i + 1 + rel + 1;
            if end > i + 2 {
                out.math_ranges.push((i, end));
                i = end;
                continue;
            }
        }
        if starts_with_at(content, i, "\\(")
            && let Some(rel) = content[i + 2..].find("\\)")
        {
            let end = i + 2 + rel + 2;
            if end > i + 4 {
                out.math_ranges.push((i, end));
                i = end;
                continue;
            }
        }

        // Bold: `**…**`.
        if bytes[i] == b'*'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'*'
            && let Some(rel) = content[i + 2..].find("**")
        {
            let end = i + 2 + rel + 2;
            out.bold_ranges.push((i, end));
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

fn starts_with_at(s: &str, i: usize, needle: &str) -> bool {
    s.as_bytes().get(i..i + needle.len()) == Some(needle.as_bytes())
}

fn parse_markdown_link(content: &str, i: usize) -> Option<ParsedLink> {
    if content.as_bytes().get(i) != Some(&b'[') {
        return None;
    }
    let label_end = i + 1 + content[i + 1..].find(']')?;
    let url_start = label_end + 1;
    if content.as_bytes().get(url_start) != Some(&b'(') {
        return None;
    }
    let url_end = url_start + 1 + content[url_start + 1..].find(')')?;
    let raw_url = content[url_start + 1..url_end].trim();
    let url = normalize_http_url(raw_url)?;
    Some(((i, url_end + 1), (i + 1, label_end), url))
}

fn parse_tex_link(content: &str, i: usize) -> Option<ParsedLink> {
    if starts_with_at(content, i, "\\url{") {
        let url_start = i + "\\url{".len();
        let url_end = url_start + content[url_start..].find('}')?;
        let url = normalize_http_url(content[url_start..url_end].trim())?;
        return Some(((i, url_end + 1), (url_start, url_end), url));
    }
    if starts_with_at(content, i, "\\href{") {
        let url_start = i + "\\href{".len();
        let url_end = url_start + content[url_start..].find('}')?;
        let after_url = url_end + 1;
        if content.as_bytes().get(after_url) != Some(&b'{') {
            return None;
        }
        let label_start = after_url + 1;
        let label_end = label_start + content[label_start..].find('}')?;
        let url = normalize_http_url(content[url_start..url_end].trim())?;
        return Some(((i, label_end + 1), (label_start, label_end), url));
    }
    None
}

fn parse_bare_url(content: &str, i: usize) -> Option<(usize, usize, String)> {
    if !(starts_with_at(content, i, "https://") || starts_with_at(content, i, "http://")) {
        return None;
    }
    let mut end = i;
    for (offset, ch) in content[i..].char_indices() {
        if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'') {
            break;
        }
        end = i + offset + ch.len_utf8();
    }
    while end > i
        && matches!(
            content.as_bytes()[end - 1],
            b'.' | b',' | b';' | b':' | b'!' | b'?' | b')' | b']'
        )
    {
        end -= 1;
    }
    let url = normalize_http_url(&content[i..end])?;
    Some((i, end, url))
}

fn normalize_http_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.starts_with("https://") || raw.starts_with("http://") {
        Some(raw.to_string())
    } else {
        None
    }
}

/// Find the byte offset of a run of exactly `n` backticks within `s`.
fn find_backtick_run(s: &str, n: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + n <= bytes.len() {
        if bytes[i..i + n].iter().all(|&b| b == b'`') {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Enforce the GFM table column-count invariant: the number of columns is
/// fixed by the header row, so every body row is normalized to exactly that
/// width — short rows are padded with empty cells, over-wide rows truncated.
/// Establishing this once at parse time lets every consumer index rows by
/// column without per-access bounds checks.
fn normalize_table_rows(header: &[String], rows: &mut [Vec<String>]) {
    let ncols = header.len();
    if ncols == 0 {
        // Degenerate: no columns to normalize against. Such a table yields an
        // empty render and is dropped by the caller, so the rows are unused.
        return;
    }
    for row in rows {
        if row.len() > ncols {
            row.truncate(ncols);
        } else if row.len() < ncols {
            row.resize(ncols, String::new());
        }
    }
}

#[derive(Default)]
struct TableAccumulator {
    aligns: Vec<TableAlignment>,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl TableAccumulator {
    /// Render the table as a GFM-style aligned grid using box-drawing borders.
    ///
    /// Columns are sized to their widest cell (intrinsic width) so vertical
    /// separators line up across all rows. The header is followed by a
    /// separator rule. Wide tables that exceed the viewport are handed to the
    /// renderer's normal line wrapping rather than being truncated.
    fn render(&self) -> String {
        if self.header.is_empty() {
            return String::new();
        }
        let ncols = self.header.len();
        let width = |cell: &str| display_width(cell);

        // Per-column intrinsic width: max of header and every body cell.
        // Rows are pre-normalized to `ncols` cells by `normalize_table_rows`,
        // so iterating in full here touches exactly one cell per column.
        let mut widths = vec![0usize; ncols];
        for (i, h) in self.header.iter().enumerate() {
            widths[i] = widths[i].max(width(h));
        }
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(width(cell));
            }
        }

        let join_borders = |sep: &str| -> String {
            widths
                .iter()
                .map(|w| "─".repeat(w + 2))
                .collect::<Vec<_>>()
                .join(sep)
        };

        let mut out = String::new();
        out.push_str(&format!("┌{}┐\n", join_borders("┬")));
        out.push_str(&format_row(&self.header, &widths, &self.aligns));
        out.push('\n');
        out.push_str(&format!("├{}┤\n", join_borders("┼")));
        for row in &self.rows {
            out.push_str(&format_row(row, &widths, &self.aligns));
            out.push('\n');
        }
        out.push_str(&format!("└{}┘", join_borders("┴")));
        out
    }
}

/// Format one table row as `│ cell │ cell │`, honoring per-column alignment.
fn format_row(cells: &[String], widths: &[usize], aligns: &[TableAlignment]) -> String {
    let ncols = widths.len();
    let parts: Vec<String> = (0..ncols)
        .map(|i| {
            let cell = cells.get(i).map(String::as_str).unwrap_or("");
            let align = aligns.get(i).copied().unwrap_or(TableAlignment::None);
            pad_cell(cell, widths[i], align)
        })
        .collect();
    format!("│ {} │", parts.join(" │ "))
}

fn pad_cell(cell: &str, width: usize, align: TableAlignment) -> String {
    let cell_w = display_width(cell);
    let pad = width.saturating_sub(cell_w);
    match align {
        TableAlignment::Right => format!("{}{}", " ".repeat(pad), cell),
        TableAlignment::Center => {
            let left = pad / 2;
            let right = pad - left;
            format!("{}{}{}", " ".repeat(left), cell, " ".repeat(right))
        }
        TableAlignment::None | TableAlignment::Left => format!("{}{}", cell, " ".repeat(pad)),
    }
}

fn display_width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// Drop ranges that fall entirely past `len` and clamp the end of any range
/// that straddles it (trim_end can only shrink trailing whitespace, so in
/// practice this is a no-op for interior code runs, but it keeps the invariant
/// `end <= content.len()` airtight).
pub(crate) fn clamp_ranges(ranges: &[CodeRange], len: usize) -> Vec<CodeRange> {
    ranges
        .iter()
        .map(|&(s, e)| (s.min(len), e.min(len)))
        .filter(|&(s, e)| s < e)
        .collect()
}

pub(crate) fn clamp_link_ranges(ranges: &[LinkRange], len: usize) -> Vec<LinkRange> {
    ranges
        .iter()
        .filter_map(|link| {
            let range = (link.range.0.min(len), link.range.1.min(len));
            let label_range = (link.label_range.0.min(len), link.label_range.1.min(len));
            (range.0 < range.1 && label_range.0 < label_range.1).then(|| LinkRange {
                range,
                label_range,
                url: link.url.clone(),
            })
        })
        .collect()
}

fn push_block(blocks: &mut Vec<Block>, block: Block) {
    if block.is_empty() && !matches!(block, Block::Rule | Block::Break) {
        return;
    }
    let needs_gap = blocks.last().is_some_and(|previous| {
        !matches!(
            (previous, &block),
            (Block::Break, _) | (Block::ListItem { .. }, Block::ListItem { .. })
        )
    });
    if needs_gap {
        blocks.push(Block::Break);
    }
    blocks.push(block);
}
