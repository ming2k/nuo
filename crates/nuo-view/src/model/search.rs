//! Transcript retrieval: the facet vocabulary, the query, and the match engine.
//!
//! # Why this is a pure function over `&[TranscriptMessage]`
//!
//! The obvious implementation keeps an index (`Vec<IndexedEntry>`) beside the
//! transcript and patches it incrementally. That index is a second copy of the
//! document, and every copy needs an invalidation story: streaming tails bump
//! `rev`, tool steps settle their output mid-list, compaction appends a card,
//! restore replaces the whole list, the subagent zoom swaps the slice. Each of
//! those is a chance for the index to be silently stale — and a stale search
//! result is worse than a slow one, because the user is *looking for something
//! they cannot see*.
//!
//! So there is no index. [`find`] walks the transcript and matches borrowed
//! `&str` segments, exactly as declared by
//! [`TranscriptMessage::visit_search_segments`]. The cost is one linear scan
//! per refine — a keystroke or a transcript mutation, never a frame — and in
//! exchange the results can never disagree with what is on screen.
//!
//! If a day comes when that scan is measurably too slow, the seam to add
//! caching at is this module's boundary (a `&mut SearchCache` threaded through
//! [`find`]), not a parallel document inside `App`.
//!
//! # Facets
//!
//! A [`Facet`] answers "which entries do I want to see". The vocabulary is
//! deliberately *open at the end*: [`Facet::ToolNamed`] is minted from the tool
//! names actually present in the transcript ([`choices`]). A new tool needs no
//! registry entry, no config key, and no enum arm — the day it runs, it becomes
//! filterable.
//!
//! # Ordering
//!
//! Hits are **document order, always**. This is a locator, not a ranker: `n`/`N`
//! must walk the conversation the way it happened, so that "the third match" is
//! a stable place in history rather than an artifact of a scoring function.

use std::ops::Range;

use crate::model::document::TranscriptMessage;

/// Which transcript entries a query selects.
///
/// The declaration order is the order the choices are offered to the user
/// ([`choices`]) and therefore the cycle order of the facet axis.
#[derive(Clone, Default, PartialEq, Eq, Debug, Hash)]
pub enum Facet {
    /// Every entry.
    #[default]
    Any,
    /// What the human said.
    User,
    /// Assistant prose.
    Text,
    /// Reasoning traces ("thought").
    Thought,
    /// Any tool step (including subagent tasks).
    Tool,
    /// One specific tool, by its wire name.
    ToolNamed(String),
    /// Harness notices.
    Notice,
    /// Entries that failed.
    Error,
    /// Control-plane command rows.
    Command,
}

impl Facet {
    /// The chip label shown in the find bar.
    pub fn label(&self) -> &str {
        match self {
            Facet::Any => "all",
            Facet::User => "you",
            Facet::Text => "reply",
            Facet::Thought => "thought",
            Facet::Tool => "tool",
            Facet::ToolNamed(name) => name,
            Facet::Notice => "notice",
            Facet::Error => "error",
            Facet::Command => "cmd",
        }
    }

    /// Whether an entry with `facets` (and, for tool steps, `tool`) is selected.
    ///
    /// `Any` is the only facet that does not constrain. Every other facet is a
    /// *superset* test: `Tool` selects a subagent task as well as a shell call,
    /// `Error` selects a failed tool step as well as an error notice.
    pub fn selects(&self, facets: Facets, tool: Option<&str>) -> bool {
        match self {
            Facet::Any => true,
            Facet::User => facets.contains(Facets::USER),
            Facet::Text => facets.contains(Facets::TEXT),
            Facet::Thought => facets.contains(Facets::THOUGHT),
            Facet::Tool => facets.contains(Facets::TOOL),
            Facet::ToolNamed(name) => facets.contains(Facets::TOOL) && tool == Some(name.as_str()),
            Facet::Notice => facets.contains(Facets::NOTICE),
            Facet::Error => facets.contains(Facets::ERROR),
            Facet::Command => facets.contains(Facets::COMMAND),
        }
    }
}

/// The set of roles one entry plays, as a bit set.
///
/// An entry is rarely one thing: a failed `execute_command` is a tool step
/// *and* an error; a subagent task is a tool step carrying other entries. A set
/// (rather than an enum) is what lets `Error` and `Tool` both select it without
/// inventing a lattice of composite variants.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Facets(u16);

impl Facets {
    /// Nothing (should never describe a rendered entry).
    pub const NONE: Self = Self(0);
    /// A human message.
    pub const USER: Self = Self(1 << 0);
    /// Assistant prose.
    pub const TEXT: Self = Self(1 << 1);
    /// A reasoning trace.
    pub const THOUGHT: Self = Self(1 << 2);
    /// A tool step.
    pub const TOOL: Self = Self(1 << 3);
    /// A harness notice (or a live provider-retry row, which renders as one).
    pub const NOTICE: Self = Self(1 << 4);
    /// Something that failed.
    pub const ERROR: Self = Self(1 << 5);
    /// A control-plane command row.
    pub const COMMAND: Self = Self(1 << 6);

    /// Add `other` to this set.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every bit of `other` is present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no bit is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// One searchable text region of an entry.
///
/// The order of the variants is the match priority: within one entry the
/// earliest *field* that matches wins, so a needle that appears both in a tool's
/// arguments and in its output always reports the arguments. Determinism here is
/// what makes `n`/`N` stable across refines.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    /// Assistant or human prose, and compaction summaries.
    Prose,
    /// A reasoning trace.
    Thought,
    /// The tool's name.
    ToolName,
    /// The tool call's arguments.
    ToolArgs,
    /// The tool call's output.
    ToolOutput,
    /// A notice's topic/title/detail.
    Notice,
    /// A control-plane command's invocation and reply.
    Command,
}

impl Field {
    /// The short label shown beside a hit in the find bar.
    pub fn label(self) -> &'static str {
        match self {
            Field::Prose => "text",
            Field::Thought => "thought",
            Field::ToolName => "tool",
            Field::ToolArgs => "args",
            Field::ToolOutput => "output",
            Field::Notice => "notice",
            Field::Command => "cmd",
        }
    }
}

/// One user query: a facet selection plus a text needle.
///
/// Both halves are optional in effect — an empty needle keeps every entry the
/// facet selects (so `thought` alone lists all reasoning), and [`Facet::Any`]
/// keeps every entry the needle matches.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Filter {
    /// Which entries to consider.
    pub facet: Facet,
    /// Case-insensitive substring. Empty matches every entry the facet selects.
    pub needle: String,
}

impl Filter {
    /// Whether this filter would select `message` on facet grounds alone.
    pub fn selects(&self, message: &TranscriptMessage) -> bool {
        self.facet.selects(message.facets(), message.tool_label())
    }
}

/// How much context the snippet keeps on each side of the match.
const SNIPPET_CONTEXT_CHARS: usize = 28;

/// A short, single-line window around a match, for the find bar.
///
/// `matched` is kept separate from its neighbours so the bar can emphasise it
/// without re-running the matcher (and therefore without ever disagreeing with
/// the row it describes).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Snippet {
    /// Text between the start of the line (clamped, ellipsised) and the match.
    pub lead: String,
    /// The matched text, exactly as it appears in the source.
    pub matched: String,
    /// Text between the match and the end of the line (clamped, ellipsised).
    pub tail: String,
}

impl Snippet {
    /// Whether the snippet carries any text.
    pub fn is_empty(&self) -> bool {
        self.lead.is_empty() && self.matched.is_empty() && self.tail.is_empty()
    }
}

/// One located entry.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hit {
    /// Index into the slice the search ran over.
    pub msg_idx: usize,
    /// Which region of the entry matched.
    pub field: Field,
    /// The matched byte range within that region.
    pub range: Range<usize>,
    /// 1-based line of the match within that region.
    pub line: usize,
    /// Display window around the match.
    pub snippet: Snippet,
}

/// Find every entry the filter selects, in document order.
///
/// One hit per entry: the user navigates *entries*, and an entry with four
/// matches is still one place in the conversation. The reported match is the
/// first one in field-priority order.
pub fn find(messages: &[TranscriptMessage], filter: &Filter) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (msg_idx, message) in messages.iter().enumerate() {
        if !filter.selects(message) {
            continue;
        }
        if filter.needle.is_empty() {
            hits.push(Hit {
                msg_idx,
                field: Field::Prose,
                range: 0..0,
                line: 1,
                snippet: Snippet::default(),
            });
            continue;
        }
        let mut found = None;
        message.visit_search_segments(&mut |field, text| match find_ci(text, &filter.needle) {
            Some(range) => {
                // Everything that escapes the closure is owned: line number and
                // display snippet are extracted here, while the borrowed
                // segment is still alive.
                found = Some(Hit {
                    msg_idx,
                    field,
                    line: 1 + text[..range.start].matches('\n').count(),
                    snippet: snippet_of(text, &range),
                    range,
                });
                true
            }
            None => false,
        });
        if let Some(hit) = found {
            hits.push(hit);
        }
    }
    hits
}

/// The facets offered for a transcript: the fixed vocabulary, then one entry per
/// tool name that actually appears in it.
///
/// Discovered, never configured — a tool the agent starts using becomes
/// filterable the moment it runs, and the list stays free of vocabulary the
/// thread never used.
pub fn choices(messages: &[TranscriptMessage]) -> Vec<Facet> {
    let mut choices = vec![
        Facet::Any,
        Facet::User,
        Facet::Text,
        Facet::Thought,
        Facet::Tool,
        Facet::Notice,
        Facet::Error,
        Facet::Command,
    ];
    for message in messages {
        let Some(name) = message.tool_label() else {
            continue;
        };
        if choices
            .iter()
            .any(|facet| matches!(facet, Facet::ToolNamed(known) if known == name))
        {
            continue;
        }
        choices.push(Facet::ToolNamed(name.to_string()));
    }
    choices
}

/// Case-insensitive substring search returning the match in `haystack` byte
/// offsets.
///
/// Two paths, both exact:
///
/// - **ASCII fast path.** Byte-wise `eq_ignore_ascii_case` over a sliding
///   window. No allocation, no char decoding, and the byte offsets are
///   trivially the match offsets.
/// - **Unicode path.** Walks char boundaries and compares per-char
///   `to_lowercase()` iterators. This is the only correct way to do it:
///   lowercasing a *copy* is wrong because case folding can change a string's
///   byte length (`İ` folds to two chars), which would make every reported
///   offset — and therefore every highlight and selection range — drift.
///   Folding is therefore **per char (1:1, simple case folding)**: a needle
///   never matches across a one-to-many fold (`ß`→`ss`, `İ`→`i̇`). That is the
///   same contract terminal pagers offer, and it keeps every returned range a
///   slice boundary of `haystack`.
pub fn find_ci(haystack: &str, needle: &str) -> Option<Range<usize>> {
    if needle.is_empty() {
        return Some(0..0);
    }
    if haystack.is_ascii() && needle.is_ascii() {
        let hay = haystack.as_bytes();
        let needle = needle.as_bytes();
        if needle.len() > hay.len() {
            return None;
        }
        return (0..=hay.len() - needle.len())
            .find(|&start| hay[start..start + needle.len()].eq_ignore_ascii_case(needle))
            .map(|start| start..start + needle.len());
    }
    find_ci_unicode(haystack, needle)
}

fn find_ci_unicode(haystack: &str, needle: &str) -> Option<Range<usize>> {
    let needle: Vec<char> = needle.chars().collect();
    let hay: Vec<(usize, char)> = haystack.char_indices().collect();
    if needle.len() > hay.len() {
        return None;
    }
    for start in 0..=hay.len() - needle.len() {
        let matched = needle.iter().enumerate().all(|(offset, &want)| {
            let (_, have) = hay[start + offset];
            have == want || have.to_lowercase().eq(want.to_lowercase())
        });
        if matched {
            let lo = hay[start].0;
            let hi = hay
                .get(start + needle.len())
                .map(|&(byte, _)| byte)
                .unwrap_or(haystack.len());
            return Some(lo..hi);
        }
    }
    None
}

/// Build the display window for a match: the containing line, clamped to
/// [`SNIPPET_CONTEXT_CHARS`] characters on each side.
fn snippet_of(text: &str, range: &Range<usize>) -> Snippet {
    let line_start = text[..range.start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[range.end..]
        .find('\n')
        .map_or(text.len(), |i| range.end + i);

    let before = &text[line_start..range.start];
    let after = &text[range.end..line_end];

    let mut lead = clamp_tail(before, SNIPPET_CONTEXT_CHARS);
    if lead.len() < before.len() {
        lead.insert(0, '…');
    }
    let mut tail = clamp_head(after, SNIPPET_CONTEXT_CHARS);
    if tail.len() < after.len() {
        tail.push('…');
    }
    Snippet {
        lead,
        matched: text[range.clone()].to_string(),
        tail,
    }
}

/// Keep at most `chars` characters from the end of `text`.
fn clamp_tail(text: &str, chars: usize) -> String {
    let count = text.chars().count();
    if count <= chars {
        return text.to_string();
    }
    text.chars().skip(count - chars).collect()
}

/// Keep at most `chars` characters from the start of `text`.
fn clamp_head(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

/// Whether the find bar is only *locating* matches or *reducing* the transcript
/// to them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Posture {
    /// The transcript is untouched; the bar walks matches within it.
    #[default]
    Locate,
    /// The transcript renders only the selected entries.
    Filter,
}

impl Posture {
    /// A stable label for the posture badge.
    pub fn label(self) -> &'static str {
        match self {
            Posture::Locate => "locate",
            Posture::Filter => "only",
        }
    }
}

/// The find bar's state machine.
///
/// Owns the query, the resulting hits, and the cursor into them. Everything that
/// changes the result set funnels through [`Finder::sync`], so the hit list and
/// the visible-index list (the filter posture's input to the layout) are always
/// produced together and cannot drift.
#[derive(Clone, Debug)]
pub struct Finder {
    facet: Facet,
    needle: String,
    posture: Posture,
    hits: Vec<Hit>,
    /// Hit indices, kept separately so the layout can borrow `&[usize]` without
    /// the shell re-deriving (or re-allocating) it every frame — a changed
    /// allocation is a changed virtual-index identity.
    visible: Vec<usize>,
    choices: Vec<Facet>,
    cursor: usize,
    stamp: Stamp,
    synced: bool,
}

/// What the current hit list was computed from.
///
/// Cheap to compute (one add/xor per entry) and compared before every refine, so
/// an unchanged transcript never re-scans. It covers appends, in-place
/// streaming, settled tool output, restore, and view switches — the whole
/// mutation surface — without any producer having to remember to invalidate
/// anything.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Stamp {
    count: usize,
    fingerprint: u64,
    facet_rev: u64,
    needle_len: usize,
    view_gen: u32,
}

impl Default for Finder {
    fn default() -> Self {
        Self {
            facet: Facet::Any,
            needle: String::new(),
            posture: Posture::Locate,
            hits: Vec::new(),
            visible: Vec::new(),
            choices: vec![Facet::Any],
            cursor: 0,
            stamp: Stamp::default(),
            synced: false,
        }
    }
}

impl Finder {
    /// A new, empty finder.
    pub fn new() -> Self {
        Self::default()
    }

    /// The active facet.
    pub fn facet(&self) -> &Facet {
        &self.facet
    }

    /// The active needle.
    pub fn needle(&self) -> &str {
        &self.needle
    }

    /// The active posture.
    pub fn posture(&self) -> Posture {
        self.posture
    }

    /// Every hit, in document order.
    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    /// The cursor position, 0-based.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The hit under the cursor.
    pub fn current(&self) -> Option<&Hit> {
        self.hits.get(self.cursor)
    }

    /// `(current, total)` in 1-based form for a `3/17` counter; `(0, 0)` when
    /// there is nothing to show.
    pub fn counter(&self) -> (usize, usize) {
        if self.hits.is_empty() {
            (0, 0)
        } else {
            (self.cursor + 1, self.hits.len())
        }
    }

    /// The facets offered right now (fixed vocabulary + discovered tool names).
    pub fn choices(&self) -> &[Facet] {
        &self.choices
    }

    /// The message indices the transcript should render, or `None` when the
    /// finder is only locating.
    pub fn visible(&self) -> Option<&[usize]> {
        match self.posture {
            Posture::Locate => None,
            Posture::Filter => Some(&self.visible),
        }
    }

    /// Replace the needle. Returns whether it changed.
    pub fn set_needle(&mut self, needle: impl Into<String>) -> bool {
        let needle = needle.into();
        if self.needle == needle {
            return false;
        }
        self.needle = needle;
        self.cursor = 0;
        true
    }

    /// Move the facet cursor by `delta` over the current choices, wrapping.
    ///
    /// Entering the cycle from a facet that is no longer offered (a tool that
    /// disappeared after a restore) snaps to the nearest offered facet rather
    /// than getting stuck on an invisible selection.
    pub fn cycle_facet(&mut self, delta: i8) {
        if self.choices.is_empty() {
            return;
        }
        let len = self.choices.len() as i32;
        let current = self
            .choices
            .iter()
            .position(|facet| *facet == self.facet)
            .map(|index| index as i32)
            .unwrap_or(0);
        let next = (current + i32::from(delta)).rem_euclid(len) as usize;
        self.facet = self.choices[next].clone();
        self.cursor = 0;
    }

    /// Switch between locating and filtering, returning the new posture.
    pub fn toggle_posture(&mut self) -> Posture {
        self.posture = match self.posture {
            Posture::Locate => Posture::Filter,
            Posture::Filter => Posture::Locate,
        };
        self.posture
    }

    /// Move the cursor by `delta` over the hits, wrapping. Returns the newly
    /// current hit.
    pub fn step(&mut self, delta: i8) -> Option<&Hit> {
        if self.hits.is_empty() {
            return None;
        }
        let len = self.hits.len() as i32;
        let next = (self.cursor as i32 + i32::from(delta)).rem_euclid(len);
        self.cursor = next as usize;
        self.current()
    }

    /// Recompute hits and choices if anything they depend on moved.
    ///
    /// `view_gen` distinguishes focus-stack views (the root thread vs a zoomed
    /// subagent) whose slices are not comparable by content. Returns whether the
    /// hit list changed.
    pub fn sync(&mut self, messages: &[TranscriptMessage], view_gen: u32) -> bool {
        let stamp = Stamp {
            count: messages.len(),
            fingerprint: fingerprint(messages),
            facet_rev: facet_rev(&self.facet),
            needle_len: self.needle.len(),
            view_gen,
        };
        if self.synced && stamp == self.stamp {
            return false;
        }
        self.synced = true;
        self.stamp = stamp;
        self.choices = choices(messages);
        let filter = Filter {
            facet: self.facet.clone(),
            needle: self.needle.clone(),
        };
        self.hits = find(messages, &filter);
        self.visible.clear();
        self.visible.extend(self.hits.iter().map(|hit| hit.msg_idx));
        if self.cursor >= self.hits.len() {
            self.cursor = self.hits.len().saturating_sub(1);
        }
        true
    }
}

/// A content-derived fingerprint of the transcript.
///
/// `id ^ rev` folding: ids are process-unique and revs bump on content
/// mutation, so any append, stream tick, or settled result changes the value.
/// Order-sensitive through the rotation, so a reordering cannot collide.
fn fingerprint(messages: &[TranscriptMessage]) -> u64 {
    messages
        .iter()
        .enumerate()
        .fold(0u64, |acc, (index, message)| {
            let term = message
                .id
                .rotate_left((index % 63) as u32 + 1)
                .wrapping_add(message.rev.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            acc.wrapping_add(term)
        })
}

/// A cheap identity for a facet, so a facet change is visible to [`Stamp`].
fn facet_rev(facet: &Facet) -> u64 {
    match facet {
        Facet::Any => 0,
        Facet::User => 1,
        Facet::Text => 2,
        Facet::Thought => 3,
        Facet::Tool => 4,
        Facet::ToolNamed(name) => {
            // FNV-1a over the tool name, offset out of the fixed range.
            0x10_0000
                + name.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                    (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
                })
        }
        Facet::Notice => 5,
        Facet::Error => 6,
        Facet::Command => 7,
    }
}

#[cfg(test)]
mod tests;
