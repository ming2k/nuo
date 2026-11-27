//! Attachment "chips" staged behind short placeholders inside the live input
//! box, mirroring the paste UX in codex / claude-code / opencode:
//!
//! - Pasting an image inserts `[Image #N (size)]` into the input text and
//!   stages the base64 payload in `pending_images`. The chip is the user's
//!   visible affordance that an attachment will ship with the next message.
//! - Pasting a large block of text inserts `[Pasted text #N +M lines (size)]`
//!   and stages the full text in `pending_text_pastes`, so the input box stays
//!   compact instead of being flooded by thousands of lines.
//! - A single `Backspace` against either chip removes the whole chip (plus
//!   any one trailing space inserted alongside it) in one keystroke, and the
//!   matching staged entry is dropped on the next reconcile pass.
//!
//! The chip label is the attachment's **identifier and carries real
//! information**: its `#N` keys into the staged payload vector, `+M lines`
//! and the byte-size badge report how much content is hidden behind it, and
//! the composer renders the two kinds in distinct colors (text block vs.
//! image block) so pasted material reads as a typed object, not prose.
//!
//! The chip text lives inline in `App::input` (codex / claude-code style
//! rather than codex's separate element list) so that ordinary cursor
//! movement, selection, and copy keep working without a custom text buffer.
//! A small scan pass (`reconcile`) runs after each input mutation to prune
//! orphaned staged entries and relabel surviving chips so their `#N` always
//! matches their 1-based position in the staged vectors — re-deriving the
//! size badge from the surviving payload each time so a relabeled chip never
//! reports a stale byte count.

use nuo_tool::ImagePart;

/// A pasted-text block becomes a chip when its size crosses this threshold,
/// mirroring codex's `LARGE_PASTE_CHAR_THRESHOLD` (1000) and claude-code's
/// `PASTE_THRESHOLD` (800). The chosen 1024 sits between them and matches
/// the user expectation that ordinary short snippets — single-line commands,
/// URLs, a paragraph of prose — keep flowing through the cursor as ordinary
/// text, while a truly large paste (a source file, a stack trace, a long
/// log dump) collapses into a one-line placeholder so the input box stays
/// compact.
pub const LARGE_PASTE_CHAR_THRESHOLD: usize = 1024;

/// Classification of a parsed chip. Mirrors the staged-attachment vectors on
/// `App`: image chips reference `pending_images`, paste chips
/// reference `pending_text_pastes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipKind {
    Image,
    Paste,
}

/// A chip scanned out of the live input text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipMatch {
    pub kind: ChipKind,
    /// The `N` in `#N`, 1-based.
    pub number: usize,
    /// For paste chips, the `M` in `+M lines` if present.
    pub line_count: Option<usize>,
    /// Inclusive start byte offset of the chip's opening `[`.
    pub start_byte: usize,
    /// Exclusive end byte offset just past the chip's closing `]`.
    pub end_byte: usize,
}

/// Human-readable byte size for chip badges: `900 B`, `1.2 KB`, `3.4 MB`.
/// Sub-1024 sizes stay exact byte counts; scaled values keep one decimal
/// until they reach 100 (`120 KB`) so the badge stays short on screen.
pub fn human_size(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Recover the original byte length of a base64-encoded payload (e.g. an
/// image's `ImagePart::data`), used to re-derive chip size badges after a
/// [`reconcile`] pass. Exact for standard base64 with or without `=` padding.
pub fn base64_byte_size(b64: &str) -> usize {
    let padding = b64.bytes().rev().take_while(|&b| b == b'=').count();
    b64.len().saturating_sub(padding) * 3 / 4
}

/// Build the `[Image #N (size)]` label staged behind a pasted image
/// attachment. `size_bytes` is the raw (unencoded) byte length of the image,
/// so the identifier reports the attachment's real weight; the composer
/// paints image chips in a distinct color (see `Theme::chip_image_fg`)
/// so they read as image blocks at a glance.
pub fn image_chip(number_one_based: usize, size_bytes: usize) -> String {
    format!("[Image #{number_one_based} ({})]", human_size(size_bytes))
}

/// Build the `[Pasted text #N +M lines (size)]` label staged behind a large
/// pasted text block. `line_count` is the number of logical lines
/// (`\n`-separated) and `size_bytes` the byte length of the original paste,
/// so the chip's identifier reports exactly how much text is hidden behind
/// it; the composer paints paste chips in a distinct color (see
/// `Theme::chip_paste_fg`) so they
/// read as text blocks.
pub fn paste_chip(number_one_based: usize, line_count: usize, size_bytes: usize) -> String {
    format!(
        "[Pasted text #{number_one_based} +{line_count} lines ({})]",
        human_size(size_bytes)
    )
}

/// Decide whether a pasted text block should be staged behind a chip rather
/// than inlined verbatim. Only size matters: a single threshold of
/// `LARGE_PASTE_CHAR_THRESHOLD` chars, so ordinary short pastes (a
/// command, a URL, a paragraph of prose, even a multi-line snippet that
/// stays under the limit) keep flowing through the cursor like an ordinary
/// editor paste. Beyond the threshold the paste collapses into a
/// `[Pasted text #N +M lines]` chip so the input box does not balloon to
/// dozens of wrapped rows.
pub fn should_chip_paste(text: &str) -> bool {
    text.chars().count() > LARGE_PASTE_CHAR_THRESHOLD
}

/// Count the logical lines in a pasted text block for the chip's `+M lines`
/// badge. Matches `str::lines` semantics for empty input and trailing
/// newlines: `"a\nb"` → 2, `"a\nb\n"` → 2, `""` → 0.
pub fn paste_line_count(text: &str) -> usize {
    text.lines().count()
}

/// Parse the inside of a `[...]` chip (without the surrounding brackets).
/// Returns `(kind, number, line_count_opt)` on a syntactic match. Both the
/// bare legacy forms (`[Image #1]`, `[Pasted text #1 +5 lines]`) and the
/// current size-badged forms (`[Image #1 (24.1 KB)]`,
/// `[Pasted text #1 +5 lines (24.1 KB)]`) and the former ` | size` forms are
/// recognized; the size badge itself is not parsed here —
/// [`reconcile`] recomputes sizes from the staged payloads, so a stale badge
/// is corrected rather than trusted.
fn parse_chip_body(body: &str) -> Option<(ChipKind, usize, Option<usize>)> {
    if let Some(rest) = body.strip_prefix("Image #") {
        // The number ends before either the current `(size)` suffix or the
        // legacy separator-prefixed size suffix. Rejecting neither keeps resumed drafts
        // editable while every newly reconciled chip uses the current form.
        let num_part = rest.split_once(' ').map_or(rest, |(number, _)| number);
        let n = num_part.parse::<usize>().ok()?;
        if n == 0 {
            return None;
        }
        return Some((ChipKind::Image, n, None));
    }
    if let Some(rest) = body.strip_prefix("Pasted text #") {
        // Optional ` +M lines[ (size)]` or legacy ` +M lines[ | size]`
        // suffix. The number itself is
        // everything up to the first space or end-of-string.
        let (num_part, tail) = match rest.find(' ') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, ""),
        };
        let n = num_part.parse::<usize>().ok()?;
        if n == 0 {
            return None;
        }
        let line_count = if tail.is_empty() {
            None
        } else if let Some(rest2) = tail.strip_prefix(" +") {
            let digits: String = rest2.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                None
            } else {
                digits.parse::<usize>().ok()
            }
        } else {
            None
        };
        return Some((ChipKind::Paste, n, line_count));
    }
    None
}

/// Scan `input` for every well-formed chip in byte order. Used by the
/// reconcile and submit-time expansion passes.
pub fn iter_chips(input: &str) -> Vec<ChipMatch> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            // Find the matching `]` on the same byte slice. Chips never
            // contain nested brackets, so the first `]` after `[` closes
            // the candidate.
            if let Some(rel) = input[i..].find(']') {
                let end = i + rel + 1;
                let body = &input[i + 1..i + rel];
                if let Some((kind, number, line_count)) = parse_chip_body(body) {
                    out.push(ChipMatch {
                        kind,
                        number,
                        line_count,
                        start_byte: i,
                        end_byte: end,
                    });
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Find a well-formed chip that ends exactly at `byte_cursor` (i.e. its
/// closing `]` is the byte just before the cursor). Used by the Backspace
/// handler to detect an atomic-chip delete target.
pub fn chip_range_ending_at(input: &str, byte_cursor: usize) -> Option<(usize, usize)> {
    let bytes = input.as_bytes();
    if byte_cursor == 0 || byte_cursor > bytes.len() {
        return None;
    }
    if bytes[byte_cursor - 1] != b']' {
        return None;
    }
    // Scan backward for the nearest `[`.
    let mut start = byte_cursor - 1;
    while start > 0 && bytes[start] != b'[' {
        start -= 1;
    }
    if bytes[start] != b'[' {
        return None;
    }
    let body = input.get(start + 1..byte_cursor - 1)?;
    if parse_chip_body(body).is_some() {
        Some((start, byte_cursor))
    } else {
        None
    }
}

/// Detect a chip targeted by a single `Backspace` at `byte_cursor`. Two
/// shapes are recognized so the keystroke fully undoes a paste:
///
/// 1. `<chip>` immediately before the cursor — chip + cursor (no trailing
///    space, e.g. the user moved the cursor left to land right after `]`).
/// 2. `<chip> ` immediately before the cursor — chip + one trailing space +
///    cursor (the common case, since paste inserts `chip + " "`).
///
/// Returns the `(start_byte, end_byte)` byte range to delete.
pub fn chip_range_for_backspace(input: &str, byte_cursor: usize) -> Option<(usize, usize)> {
    // Case 2: chip + trailing space + cursor.
    if byte_cursor >= 1 && input.as_bytes().get(byte_cursor - 1) == Some(&b' ') {
        // `byte_cursor - 1` lands on the space (ASCII, 1 byte). Check for a
        // chip ending just before the space.
        if let Some((s, _)) = chip_range_ending_at(input, byte_cursor - 1) {
            return Some((s, byte_cursor));
        }
    }
    // Case 1: chip + cursor.
    chip_range_ending_at(input, byte_cursor)
}

/// Detect a chip targeted by a single forward `Delete` at `byte_cursor` —
/// the chip that *starts* at the caret. Mirrors
/// [`chip_range_for_backspace`]: one keystroke removes the whole chip (plus
/// the one trailing space a paste inserts) so forward-deleting a paste
/// undoes it in full.
///
/// Returns the `(start_byte, end_byte)` byte range to delete.
pub fn chip_range_for_delete(input: &str, byte_cursor: usize) -> Option<(usize, usize)> {
    let bytes = input.as_bytes();
    if byte_cursor >= bytes.len() || bytes[byte_cursor] != b'[' {
        return None;
    }
    let rel = input[byte_cursor..].find(']')?;
    let end = byte_cursor + rel + 1;
    let body = &input[byte_cursor + 1..end - 1];
    parse_chip_body(body)?;
    // Include the one trailing space a paste appends, matching the
    // backspace shape (chip + ` ` + cursor read backwards).
    if bytes.get(end) == Some(&b' ') {
        Some((byte_cursor, end + 1))
    } else {
        Some((byte_cursor, end))
    }
}

/// Reconcile the staged attachment vectors against the chips that survive in
/// `input`. Returns the new input text with all chips relabeled so their
/// `#N` matches their new 1-based position in the truncated vectors.
///
/// Algorithm:
/// 1. Walk chips in input order. Each surviving image chip pulls its payload
///    from `pending_images[old_n - 1]`; each paste chip pulls from
///    `pending_text_pastes[old_n - 1]`. If the lookup is out of bounds or
///    the slot has already been consumed (the same `#N` appeared twice),
///    the chip is treated as orphan and left in place as plain text.
/// 2. Truncate the staged vectors to the surviving counts.
/// 3. Rewrite the chip text so `#N` matches the new index.
///
/// Trailing stale entries (e.g. chip #3 was deleted but `pending_images`
/// still holds three items) are dropped by the truncate.
pub fn reconcile(
    input: &str,
    pending_images: &mut Vec<ImagePart>,
    pending_text_pastes: &mut Vec<String>,
) -> String {
    let chips = iter_chips(input);
    if chips.is_empty() {
        let mut changed = false;
        if !pending_images.is_empty() {
            pending_images.clear();
            changed = true;
        }
        if !pending_text_pastes.is_empty() {
            pending_text_pastes.clear();
            changed = true;
        }
        // No chips in input; nothing to rewrite regardless.
        let _ = changed;
        return input.to_string();
    }

    // First pass: collect surviving payloads in chip order, tracking which
    // original indices have already been consumed so a repeated chip (e.g.
    // the user copy-pasted `[Image #1]` twice) does not double-spend the
    // same backing slot.
    let mut new_images: Vec<ImagePart> = Vec::new();
    let mut new_pastes: Vec<String> = Vec::new();
    let mut consumed_image_slots: Vec<bool> = vec![false; pending_images.len()];
    let mut consumed_paste_slots: Vec<bool> = vec![false; pending_text_pastes.len()];
    let mut chip_payload: Vec<Option<(ChipKind, usize)>> = Vec::with_capacity(chips.len());
    for chip in &chips {
        match chip.kind {
            ChipKind::Image => {
                let slot = chip.number.saturating_sub(1);
                let taken = consumed_image_slots.get(slot).copied().unwrap_or(true);
                if !taken && let Some(image) = pending_images.get(slot) {
                    new_images.push(image.clone());
                    consumed_image_slots[slot] = true;
                    chip_payload.push(Some((ChipKind::Image, new_images.len())));
                    continue;
                }
                chip_payload.push(None);
            }
            ChipKind::Paste => {
                let slot = chip.number.saturating_sub(1);
                let taken = consumed_paste_slots.get(slot).copied().unwrap_or(true);
                if !taken && let Some(text) = pending_text_pastes.get(slot) {
                    new_pastes.push(text.clone());
                    consumed_paste_slots[slot] = true;
                    chip_payload.push(Some((ChipKind::Paste, new_pastes.len())));
                    continue;
                }
                chip_payload.push(None);
            }
        }
    }

    // Second pass: rebuild the input, relabeling recognized chips and
    // leaving orphan chips as-is so the user sees what they typed. The size
    // badge on each relabeled chip is re-derived from the surviving payload,
    // so a chip that kept its label through editing never reports a stale
    // byte count.
    let mut out = String::with_capacity(input.len());
    let mut last_end = 0;
    for (chip, payload) in chips.iter().zip(chip_payload.iter()) {
        out.push_str(&input[last_end..chip.start_byte]);
        match payload {
            Some((ChipKind::Image, n)) => {
                let size = new_images
                    .get(*n - 1)
                    .map(|img| base64_byte_size(&img.data))
                    .unwrap_or(0);
                out.push_str(&image_chip(*n, size));
            }
            Some((ChipKind::Paste, n)) => {
                let size = new_pastes.get(*n - 1).map_or(0, |text| text.len());
                out.push_str(&paste_chip(*n, chip.line_count.unwrap_or(0), size));
            }
            None => out.push_str(&input[chip.start_byte..chip.end_byte]),
        }
        last_end = chip.end_byte;
    }
    out.push_str(&input[last_end..]);

    *pending_images = new_images;
    *pending_text_pastes = new_pastes;
    out
}

/// Replace every paste chip in `input` with its staged full text, leaving
/// image chips in place (their payload ships via `AgentRequest::Chat::images`
/// alongside the text, so the model needs the chip's positional label to
/// know where the image belongs in the message). Used at submit time so the
/// model receives the real text rather than the chip label.
///
/// A paste chip whose `#N` has no staged payload — e.g. recalled from a
/// history entry recorded before attachment staging, or hand-typed — is
/// **dropped** rather than shipped to the model as a literal placeholder:
/// a bare `[Pasted text #1 …]` label means nothing without its hidden text.
pub fn expand_paste_chips(input: &str, pending_text_pastes: &[String]) -> String {
    let chips = iter_chips(input);
    if chips.is_empty() {
        return input.to_string();
    }
    let mut out = String::with_capacity(
        input.len() + pending_text_pastes.iter().map(|s| s.len()).sum::<usize>(),
    );
    let mut last_end = 0;
    let mut dropped_orphan = false;
    for chip in &chips {
        if chip.kind != ChipKind::Paste {
            continue;
        }
        out.push_str(&input[last_end..chip.start_byte]);
        // The chip's `#N` matches `pending_text_pastes[N-1]` because
        // reconcile has already run by submit time.
        let slot = chip.number.saturating_sub(1);
        if let Some(text) = pending_text_pastes.get(slot) {
            out.push_str(text);
            last_end = chip.end_byte;
        } else {
            // Orphaned paste chip: drop the label plus one following space
            // (the paste convention is `chip + " "`). The space that
            // preceded the chip stays as the separator, so "pre [chip] mid"
            // collapses to "pre mid" instead of shipping a literal
            // placeholder to the model.
            dropped_orphan = true;
            let mut end = chip.end_byte;
            if input.as_bytes().get(end) == Some(&b' ') {
                end += 1;
            }
            last_end = end;
        }
    }
    out.push_str(&input[last_end..]);
    // An orphan dropped at the very end leaves the space that preceded it
    // dangling ("pre [chip]" → "pre "); trim it so the message ends cleanly.
    if dropped_orphan {
        let trimmed = out.trim_end().len();
        out.truncate(trimmed);
    }
    out
}

/// Remove every `[Image #N …]` chip from `input` whose payload is **not**
/// staged in `images` (i.e. `N > image_count`). Image chips are positional
/// labels the model uses to locate the attached pixels; an orphaned label —
/// recalled from a history entry recorded before attachment staging, or
/// hand-typed — has no payload behind it and must not ship to the model as
/// literal text (which reads as a fake "image" the model cannot see).
///
/// Chips with a backing payload are left untouched (the submit path pairs
/// them with `AgentRequest::Chat::images`). A dropped chip takes one
/// immediately-following space with it (the paste convention is `chip + " "`),
/// so consecutive orphans collapse to a single space.
pub fn strip_orphan_image_chips(input: &str, image_count: usize) -> String {
    let chips = iter_chips(input);
    if chips.is_empty() {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut last_end = 0;
    let mut dropped_orphan = false;
    for chip in &chips {
        if chip.kind != ChipKind::Image || chip.number <= image_count {
            continue;
        }
        out.push_str(&input[last_end..chip.start_byte]);
        let mut end = chip.end_byte;
        if input.as_bytes().get(end) == Some(&b' ') {
            end += 1;
        }
        last_end = end;
        dropped_orphan = true;
    }
    out.push_str(&input[last_end..]);
    // An orphan dropped at the very end leaves the space that preceded it
    // dangling; trim it so the message ends cleanly.
    if dropped_orphan {
        let trimmed = out.trim_end().len();
        out.truncate(trimmed);
    }
    out
}

#[cfg(test)]
mod tests;
