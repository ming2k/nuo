//! Native byte-level BPE tokenizer (tiktoken-compatible).
//!
//! Replaces the char-class estimator as the token *predictor* for context
//! pressure, compaction gates, and `/context`. The algorithm and the
//! `cl100k_base` vocabulary are OpenAI's tiktoken: text is split by the
//! cl100k pretokenizer regex, then each pretoken's bytes are merged greedily
//! by lowest rank (`byte_pair_merge`). Ranks come from a compact packed
//! table embedded at compile time (`vendor/cl100k_base.packed` — format and
//! generator in `vendor/README.md`), so encoding is total (every byte value
//! is a token) and dependency-free.
//!
//! See [ADR-0117](../../../docs/adr/0117-native-cl100k-bpe-tokenizer.md).

use std::collections::HashMap;
use std::sync::OnceLock;

/// Packed `cl100k_base` vocabulary, generated from OpenAI's published
/// `.tiktoken` file.
static CL100K_PACKED: &[u8] = include_bytes!("../../vendor/cl100k_base.packed");

/// Byte length of the longest token in the vocabulary (128 in cl100k_base).
/// Merge candidates longer than this cannot be tokens and are skipped.
const MAX_TOKEN_LEN: usize = 128;

/// Pretokens longer than this are merged in chunks. Natural language never
/// approaches the cap (the pretokenizer breaks on class boundaries); it
/// exists to bound the O(n²) merge scan for pathological single-class runs
/// (a 100 KB unbroken CJK or identifier stretch would otherwise take minutes).
const MAX_PIECE_LEN: usize = 2_048;

/// A parsed BPE vocabulary: token bytes → tiktoken rank.
struct Ranks {
    /// All token bytes concatenated in rank order (leaked once, process-wide).
    blob: &'static [u8],
    /// `starts[i]` / `lens[i]`: byte offset and length of the rank-`i` token.
    starts: Box<[u32]>,
    lens: Box<[u8]>,
    /// Rank of each single byte value (all 256 are present in cl100k_base).
    single_byte_ranks: [u32; 256],
    /// Lookup for tokens of ≥ 2 bytes, keyed by slices into `blob`.
    multi: HashMap<&'static [u8], u32>,
}

impl Ranks {
    /// Rank of an exact byte string, or `None` when it is not a token.
    fn get(&self, bytes: &[u8]) -> Option<u32> {
        match bytes.len() {
            1 => Some(self.single_byte_ranks[bytes[0] as usize]),
            2.. => self.multi.get(bytes).copied(),
            _ => None,
        }
    }

    /// Bytes of the rank-`r` token.
    fn token(&self, r: u32) -> &[u8] {
        let start = self.starts[r as usize] as usize;
        &self.blob[start..start + self.lens[r as usize] as usize]
    }
}

/// Decode the packed table. Layout (little-endian): `u32 version`,
/// `u32 token_count`, `u64 blob_len`, `token_count × u32` byte lengths, then
/// the blob — all token bytes concatenated in rank order, so a token's
/// position in the blob *is* its tiktoken rank.
fn parse_packed(packed: &[u8]) -> Ranks {
    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
    }
    fn u64_at(b: &[u8], i: usize) -> u64 {
        let mut w = [0u8; 8];
        w.copy_from_slice(&b[i..i + 8]);
        u64::from_le_bytes(w)
    }

    let version = u32_at(packed, 0);
    let token_count = u32_at(packed, 4) as usize;
    let blob_len = u64_at(packed, 16) as usize;
    let lengths_start = 24;
    let blob_start = lengths_start + token_count * 4;
    assert_eq!(version, 1, "unknown packed vocabulary version");
    assert_eq!(
        packed.len(),
        blob_start + blob_len,
        "packed vocabulary truncated"
    );

    let mut starts = Vec::with_capacity(token_count);
    let mut lens = Vec::with_capacity(token_count);
    let mut offset = 0u32;
    for i in 0..token_count {
        starts.push(offset);
        let len = u32_at(packed, lengths_start + i * 4);
        assert!(len <= u8::MAX as u32, "token longer than 255 bytes");
        lens.push(len as u8);
        offset += len;
    }
    assert_eq!(
        offset as usize, blob_len,
        "length table disagrees with blob"
    );

    // Leak the blob once: the vocabulary lives for the process lifetime, and
    // the `multi` map borrows slices out of it. `Box::leak` makes that
    // borrow 'static without `unsafe`.
    let mut blob_vec = Vec::with_capacity(blob_len);
    blob_vec.extend_from_slice(&packed[blob_start..blob_start + blob_len]);
    let blob: &'static [u8] = Box::leak(blob_vec.into_boxed_slice());

    let mut single_byte_ranks = [0u32; 256];
    let mut single_seen = [false; 256];
    let mut multi = HashMap::with_capacity(token_count);
    for rank in 0..token_count {
        let token = &blob[starts[rank] as usize..starts[rank] as usize + lens[rank] as usize];
        if token.len() == 1 {
            single_byte_ranks[token[0] as usize] = rank as u32;
            single_seen[token[0] as usize] = true;
        } else {
            multi.insert(token, rank as u32);
        }
    }
    assert!(
        single_seen.iter().all(|&seen| seen),
        "vocabulary must contain every single byte value (byte-level BPE)"
    );

    Ranks {
        blob,
        starts: starts.into_boxed_slice(),
        lens: lens.into_boxed_slice(),
        single_byte_ranks,
        multi,
    }
}

/// The process-wide parsed vocabulary.
fn ranks() -> &'static Ranks {
    static RANKS: OnceLock<Ranks> = OnceLock::new();
    RANKS.get_or_init(|| parse_packed(CL100K_PACKED))
}

// Pretokenizer — the cl100k_base pattern, hand-rolled (no regex dependency):
//
//   '(?:[sdmt]|ll|ve|re) | ?\p{L}+ | ?\p{N}+ | ?[^\s\p{L}\p{N}]+ |
//   \s+(?!\S) | \s+
//
// Semantics (matched against a tiktoken-faithful reference implementation;
// see the tests for the pinned cases):
//
// - Alternation is leftmost-first: at each position the contraction branch
//   is tried before the space-prefixed runs. A leading space never prefixes
//   a contraction (` 'll` → ` '`, `ll`), because the symbol-run branch wins
//   at the space's position first.
// - Classes per scalar: `\p{L}` = `is_alphabetic`, `\p{N}` = `is_numeric`
//   (tested before letters so letter-numerals like Ⅻ land in `\p{N}`), and
//   `\s` = `is_whitespace` (Perl \s includes NBSP, U+3000, U+2009, U+0085 —
//   verified against the reference).
// - Whitespace runs followed by a non-space satisfy `\s+(?!\S)` with the run
//   minus its last character; the last character then prefixes the next
//   pretoken when it is a space (`' '`), or forms its own one-character
//   pretoken otherwise. Runs at end of input match whole.

#[derive(Copy, Clone, PartialEq, Eq)]
enum CharKind {
    Letter,
    Number,
    Symbol,
    Whitespace,
}

fn classify(c: char) -> CharKind {
    if c == ' ' || c.is_whitespace() {
        CharKind::Whitespace
    } else if c.is_numeric() {
        CharKind::Number
    } else if c.is_alphabetic() {
        CharKind::Letter
    } else {
        CharKind::Symbol
    }
}

/// Byte range of one pretoken.
#[derive(Debug)]
struct Pretoken {
    start: usize,
    end: usize,
}

/// The char starting at byte offset `i`, if any.
fn char_at(text: &str, i: usize) -> Option<char> {
    text[i..].chars().next()
}

/// End offset (exclusive) of the contraction branch `'(?:[sdmt]|ll|ve|re)`
/// starting at `i` (where `text[i] == '\''`), or `None` when it does not
/// match. The branch has no leading-space form and no lookahead.
fn contraction_end(text: &str, i: usize) -> Option<usize> {
    let next = char_at(text, i + 1)?;
    match next {
        's' | 'd' | 'm' | 't' => Some(i + 2),
        'l' if char_at(text, i + 2) == Some('l') => Some(i + 3),
        'v' | 'r' if char_at(text, i + 2) == Some('e') => Some(i + 3),
        _ => None,
    }
}

/// Split `text` into pretokens as byte ranges.
fn pretokenize(text: &str) -> Vec<Pretoken> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let n = text.len();

    while i < n {
        let c = char_at(text, i).unwrap_or('\u{FFFD}');

        // Branch 1: contraction suffixes, only at a fresh position (a
        // pending leading space below means the symbol-run branch already
        // claimed this apostrophe).
        if c == '\''
            && let Some(end) = contraction_end(text, i)
        {
            out.push(Pretoken { start: i, end });
            i = end;
            continue;
        }

        // Branches 2-4: an optional single leading space, then a run of one
        // class. `c == ' '` with a non-space follower prefixes the run;
        // otherwise `c` itself starts it.
        let (start, body) = if c == ' ' {
            match char_at(text, i + 1) {
                Some(next) if !next.is_whitespace() => (i, i + 1),
                _ => {
                    i = whitespace_run(text, i, &mut out);
                    continue;
                }
            }
        } else if c.is_whitespace() {
            i = whitespace_run(text, i, &mut out);
            continue;
        } else {
            (i, i)
        };

        let kind = classify(char_at(text, body).unwrap_or('\u{FFFD}'));
        let mut j = body + char_at(text, body).map_or(1, char::len_utf8);
        while let Some(ch) = char_at(text, j) {
            if classify(ch) == kind {
                j += ch.len_utf8();
            } else {
                break;
            }
        }
        out.push(Pretoken { start, end: j });
        i = j;
    }
    out
}

/// Handle a whitespace run starting at `i`: consume it, emit pretokens per
/// the `\s+(?!\S)` / `\s+` rules, and return the next scan offset.
fn whitespace_run(text: &str, i: usize, out: &mut Vec<Pretoken>) -> usize {
    let n = text.len();
    let mut j = i;
    while let Some(c) = char_at(text, j) {
        if c.is_whitespace() {
            j += c.len_utf8();
        } else {
            break;
        }
    }

    if j == n {
        // Run ends the input: `\s+(?!\S)` matches it whole.
        out.push(Pretoken { start: i, end: j });
        return j;
    }

    // Followed by a non-space: branch 5 keeps all but the last character.
    let last = text[..j].chars().next_back().unwrap_or('\u{FFFD}');
    let last_len = last.len_utf8();
    if last == ' ' {
        // The trailing space prefixes the next pretoken (branches 2-4), so
        // leave it unconsumed; emit the head if the run had more than it.
        if j - last_len > i {
            out.push(Pretoken {
                start: i,
                end: j - last_len,
            });
        }
        j - last_len
    } else {
        // Non-space whitespace cannot prefix anything; it becomes its own
        // one-character pretoken after the head.
        if j - last_len > i {
            out.push(Pretoken {
                start: i,
                end: j - last_len,
            });
        }
        out.push(Pretoken {
            start: j - last_len,
            end: j,
        });
        j
    }
}

// Byte-pair merge — tiktoken's `byte_pair_merge`.

/// Merge `piece` greedily: repeatedly merge the adjacent pair whose
/// concatenation has the lowest rank (ranks are unique, so no tie-break) until
/// no adjacent pair is a token. Appends each final part's rank to `out`.
///
/// Every part of ≥ 2 bytes exists only because it was formed by merging a
/// ranked pair, so all final parts are vocabulary tokens; single bytes all
/// are. Encoding is therefore total.
///
/// The lowest-rank scan reads a parallel `pair_rank` cache instead of
/// re-hashing every candidate each iteration: a merge changes only the two
/// pairs adjacent to the merged part, so hashing drops from O(p²) to O(p)
/// per piece — the scan itself stays a cheap integer pass.
fn byte_pair_merge(ranks: &Ranks, piece: &[u8], out: &mut Vec<u32>) {
    let pair_rank = |a: usize, b: usize| -> Option<u32> {
        let seg = &piece[a..b];
        (seg.len() <= MAX_TOKEN_LEN)
            .then(|| ranks.get(seg))
            .flatten()
    };

    let mut parts: Vec<(u32, u32)> = (0..piece.len() as u32).map(|i| (i, i + 1)).collect();
    if parts.len() < 2 {
        // Single byte (or empty, never passed): always a vocabulary token.
        if let Some(rank) = ranks.get(piece) {
            out.push(rank);
        } else if !piece.is_empty() {
            out.push(ranks.single_byte_ranks[piece[0] as usize]);
        }
        return;
    }
    let mut pair_ranks: Vec<Option<u32>> = (0..parts.len() - 1)
        .map(|i| pair_rank(parts[i].0 as usize, parts[i + 1].1 as usize))
        .collect();

    loop {
        let mut min_rank = u32::MAX;
        let mut min_idx = usize::MAX;
        for (i, rank) in pair_ranks.iter().enumerate() {
            if let Some(rank) = rank
                && *rank < min_rank
            {
                min_rank = *rank;
                min_idx = i;
            }
        }
        if min_idx == usize::MAX || parts.len() < 2 {
            break;
        }
        parts[min_idx].1 = parts[min_idx + 1].1;
        parts.remove(min_idx + 1);
        pair_ranks.remove(min_idx);
        // Only the pairs touching the merged part changed.
        if min_idx > 0 {
            pair_ranks[min_idx - 1] =
                pair_rank(parts[min_idx - 1].0 as usize, parts[min_idx].1 as usize);
        }
        if min_idx < pair_ranks.len() {
            pair_ranks[min_idx] =
                pair_rank(parts[min_idx].0 as usize, parts[min_idx + 1].1 as usize);
        }
        if parts.len() < 2 {
            break;
        }
    }
    for (s, e) in parts {
        let seg = &piece[s as usize..e as usize];
        match ranks.get(seg) {
            Some(rank) => out.push(rank),
            None => out.push(ranks.single_byte_ranks[seg[0] as usize]),
        }
    }
}

// Public API

/// The `cl100k_base` tokenizer (GPT-3.5/GPT-4 family and most
/// OpenAI-compatible relays). Zero-sized: the vocabulary is process-global
/// and parsed on first use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokenizer {
    _private: (),
}

impl Tokenizer {
    pub const fn new() -> Self {
        Self { _private: () }
    }

    /// Token count of `text` under `cl100k_base`. Exact for that encoding;
    /// a close approximation for sibling encodings (o200k_base).
    pub fn count(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        self.encode(text).len()
    }

    /// Encode `text` into `cl100k_base` token ranks, in order.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        if text.is_empty() {
            return Vec::new();
        }
        let ranks = ranks();
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        for pretoken in pretokenize(text) {
            let piece = &bytes[pretoken.start..pretoken.end];
            if piece.len() == 1 {
                out.push(ranks.single_byte_ranks[piece[0] as usize]);
                continue;
            }
            if piece.len() <= MAX_PIECE_LEN {
                byte_pair_merge(ranks, piece, &mut out);
            } else {
                for chunk in piece.chunks(MAX_PIECE_LEN) {
                    byte_pair_merge(ranks, chunk, &mut out);
                }
            }
        }
        out
    }

    /// Decode ranks back to bytes (lossy for invalid UTF-8 concatenations).
    pub fn decode_bytes(&self, tokens: &[u32]) -> Vec<u8> {
        let ranks = ranks();
        let mut bytes = Vec::with_capacity(tokens.len() * 4);
        for &rank in tokens {
            bytes.extend_from_slice(ranks.token(rank));
        }
        bytes
    }
}

// Incremental counting for streamed text

/// Exact incremental `cl100k_base` token counter for streamed text.
///
/// BPE is **not** additive across arbitrary chunk boundaries: a merge can
/// span a delta boundary (`"he"` + `"llo"` is one token `"hello"`, but the
/// two halves tokenize as 1+2), so summing per-delta counts overestimates —
/// measured +2…+100% depending on chunk size and script. BPE merges never
/// cross a *pretoken* boundary, though, so the only state an exact streaming
/// count must carry is the current unfinished pretoken: feed each delta,
/// keep the tail that may still grow into (or complete) a pretoken, and
/// tokenize only finalized pretokens plus the tail at the end.
///
/// UTF-8 safety: deltas are `&str` (provider deltas arrive as complete
/// scalars), and the carried tail is always a whole-scalar boundary because
/// pretokens never split a scalar — class runs extend scalar-by-scalar.
///
/// ```
/// use nuo_contracts::tokenizer::StreamingCounter;
///
/// let mut counter = StreamingCounter::new();
/// for delta in ["hello ", "wor", "ld!"] {
///     counter.push(delta);
/// }
/// assert_eq!(counter.finish(), 3); // == Tokenizer::new().count("hello world!")
/// ```
#[derive(Debug, Clone, Default)]
pub struct StreamingCounter {
    /// The unfinished trailing pretoken (empty when the stream sits at a
    /// pretoken boundary).
    tail: String,
    /// Tokens counted from finalized pretokens so far.
    tokens: usize,
}

impl StreamingCounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one stream delta and return the running exact count so far
    /// (finalized pretokens only — the carried tail is not yet counted).
    pub fn push(&mut self, delta: &str) -> usize {
        if delta.is_empty() {
            return self.tokens;
        }
        let ranks = ranks();
        // `carry` = tail + delta; tokenize whole, then keep the final
        // pretoken (it may extend with the next delta).
        let mut carry = std::mem::take(&mut self.tail);
        carry.push_str(delta);
        let pretokens = pretokenize(&carry);
        let bytes = carry.as_bytes();
        // Commit every pretoken that no future scalar can extend or re-split;
        // carry the rest. All four class runs are extendable at stream end
        // (letters/numbers/symbols by more of their class, whitespace by more
        // whitespace), and a symbol run ending in `'` can additionally be
        // re-split into a contraction by a following letter — so the
        // conservative rule is: commit all but the final pretoken, and also
        // hold back any *symbol-run* pretoken that directly precedes it when
        // that run ends with an apostrophe (the contraction re-split case).
        let mut commit_through = pretokens.len().saturating_sub(1);
        while commit_through > 0 {
            let piece =
                &bytes[pretokens[commit_through - 1].start..pretokens[commit_through - 1].end];
            if piece.ends_with(b"'") && classify_last(piece) == CharKind::Symbol {
                commit_through -= 1; // could become a contraction; keep carrying
            } else {
                break;
            }
        }
        for pretoken in pretokens.iter().take(commit_through) {
            let piece = &bytes[pretoken.start..pretoken.end];
            self.tokens += tokenize_piece(ranks, piece);
        }
        if commit_through < pretokens.len() {
            self.tail = carry[pretokens[commit_through].start..].to_string();
        }
        self.tokens
    }

    /// Close the stream: tokenize the carried tail and return the final
    /// exact total (equal to `Tokenizer::count` over the concatenated
    /// deltas).
    pub fn finish(&mut self) -> usize {
        let ranks = ranks();
        let tail = std::mem::take(&mut self.tail);
        if tail.is_empty() {
            return self.tokens;
        }
        let extra = tokenize_piece(ranks, tail.as_bytes());
        self.tokens += extra;
        self.tokens
    }

    /// Running count without closing the stream.
    pub fn tokens(&self) -> usize {
        self.tokens
    }
}

/// Class of a pretoken's final scalar (for the contraction hold-back check).
fn classify_last(piece: &[u8]) -> CharKind {
    // Pretokens are whole scalars; find the last one without re-decoding the
    // whole piece (walk back to a lead byte).
    let mut i = piece.len() - 1;
    while i > 0 && (piece[i] & 0xC0) == 0x80 {
        i -= 1;
    }
    let s = std::str::from_utf8(&piece[i..]).unwrap_or("\u{FFFD}");
    classify(s.chars().next().unwrap_or('\u{FFFD}'))
}

/// Tokenize one whole pretoken (never split) to a count.
fn tokenize_piece(ranks: &Ranks, piece: &[u8]) -> usize {
    if piece.len() == 1 {
        return 1;
    }
    if piece.len() <= MAX_PIECE_LEN {
        let mut out = Vec::with_capacity(piece.len());
        byte_pair_merge(ranks, piece, &mut out);
        out.len()
    } else {
        let mut count = 0;
        for chunk in piece.chunks(MAX_PIECE_LEN) {
            let mut out = Vec::with_capacity(chunk.len());
            byte_pair_merge(ranks, chunk, &mut out);
            count += out.len();
        }
        count
    }
}

/// Token count via the shared [`Tokenizer`] — the predictor behind context
/// pressure, compaction gates, and `/context`.
pub fn count_tokens(text: &str) -> usize {
    Tokenizer::new().count(text)
}

/// Largest prefix of `text` that tokenizes to **at most** `max_tokens`
/// tokens, plus its exact token count. The cut lands on a token boundary by
/// construction (never mid-token, never mid-UTF-8): encoding is incremental,
/// so the prefix's tokenization is exactly the whole text's first N tokens.
///
/// This is the primitive every text-budget cut should use — a char budget
/// approximates the token count, this *is* the token count.
pub fn truncate_to_tokens(text: &str, max_tokens: usize) -> (&str, usize) {
    if max_tokens == 0 {
        return ("", 0);
    }
    let ranks = ranks();
    let bytes = text.as_bytes();
    let mut consumed = 0usize; // byte offset after all committed tokens
    let mut tokens = 0usize;
    for pretoken in pretokenize(text) {
        if tokens >= max_tokens {
            break;
        }
        let piece = &bytes[pretoken.start..pretoken.end];
        let piece_tokens = tokenize_piece(ranks, piece);
        // Whole pretoken fits: commit it (pieces never split — BPE merges
        // stay inside pretokens).
        if tokens + piece_tokens <= max_tokens {
            tokens += piece_tokens;
            consumed = pretoken.end;
            continue;
        }
        // Pretoken doesn't fit whole. Inside one pretoken the merge tree is
        // only defined for the whole piece, so an exact prefix cut is not
        // available; approximate by halving the piece (on scalar
        // boundaries — CJK pretokens are multi-byte) until it fits. This
        // branch exists for pathological single-class runs (thousands of
        // unbroken bytes or glyphs); normal text never reaches it because
        // words, punctuation, and mixed script split constantly.
        let remaining = max_tokens - tokens;
        let mut scalars = 0usize;
        for ch in text[pretoken.start..pretoken.end].chars() {
            let _ = ch;
            scalars += 1;
        }
        let mut cut_scalars = scalars;
        while cut_scalars > 0 {
            let half = cut_scalars / 2;
            if half == 0 {
                break;
            }
            cut_scalars = half;
            let cut_bytes: usize = text[pretoken.start..pretoken.end]
                .chars()
                .take(cut_scalars)
                .map(char::len_utf8)
                .sum();
            let count = tokenizer_prefix_count(ranks, &piece[..cut_bytes]);
            if count <= remaining {
                tokens += count;
                consumed = pretoken.start + cut_bytes;
                break;
            }
        }
        break;
    }
    (&text[..consumed], tokens)
}

/// Token count of a byte prefix that may split scalars/merges — used only by
/// the pathological-run halving above; the count is exact for the bytes
/// taken (each byte contributes its own token when no merge applies).
fn tokenizer_prefix_count(ranks: &Ranks, bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    if bytes.len() == 1 {
        return 1;
    }
    let mut out = Vec::with_capacity(bytes.len());
    byte_pair_merge(ranks, bytes, &mut out);
    out.len()
}

/// Byte length of the largest ≤ `max_tokens` prefix — convenience for
/// callers that need a `&str` slice directly.
pub fn truncate_str_to_tokens(text: &str, max_tokens: usize) -> &str {
    truncate_to_tokens(text, max_tokens).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pretokens(s: &str) -> Vec<&str> {
        pretokenize(s).iter().map(|p| &s[p.start..p.end]).collect()
    }

    // streaming counter

    /// Every chunking of a text must stream-count to the exact whole count.
    fn assert_streaming_exact(text: &str, sizes: &[usize]) {
        let whole = Tokenizer::new().count(text);
        for &size in sizes {
            let mut counter = StreamingCounter::new();
            // chunk on scalar boundaries at ~`size` scalars per delta
            let scalars: Vec<char> = text.chars().collect();
            let chunks: Vec<String> = scalars
                .chunks(size.max(1))
                .map(|c| c.iter().collect())
                .collect();
            for chunk in &chunks {
                counter.push(chunk);
            }
            let streamed = counter.finish();
            assert_eq!(
                streamed, whole,
                "chunk size {size}: {streamed} vs {whole} on {text:?}"
            );
        }
    }
    #[test]
    fn streaming_matches_whole_count_on_all_chunkings() {
        let texts = [
            "hello world!",
            "The quick brown fox jumps over the lazy dog. And keeps going.",
            "针对当前的token预测，我们应该改变目前这种粗暴的方案，采用BPE字节对编码算法在rust内原生实现。",
            "fn main() { let x: Vec<u32> = (0..100).map(|i| i * i).collect(); }",
            "we'll see it's fine that they've said you're right",
            "a\u{a0}\u{a0}b  c\n\n\nd\t\te",
            "1234567890 3.14159 🔥🦀✨ →←↑↓",
            "",
            " ",
        ];
        for text in texts {
            assert_streaming_exact(text, &[1, 2, 3, 5, 8, 13, 40]);
        }
    }

    #[test]
    fn streaming_delta_sum_would_overcount_but_counter_does_not() {
        // The motivating regression: per-delta counts sum high because merges
        // span delta boundaries; the StreamingCounter stays exact.
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(10);
        let whole = Tokenizer::new().count(&text);
        let mut counter = StreamingCounter::new();
        let mut naive_sum = 0;
        for chunk in text.as_bytes().chunks(3) {
            // byte chunks can split a scalar; use a lossy str for the naive
            // baseline only (the counter itself is fed valid str deltas).
            let s = String::from_utf8_lossy(chunk);
            naive_sum += Tokenizer::new().count(s.as_ref());
        }
        for chunk in text.chars().collect::<Vec<_>>().chunks(3) {
            let s: String = chunk.iter().collect();
            counter.push(&s);
        }
        assert_eq!(counter.finish(), whole);
        assert!(
            naive_sum > whole,
            "naive delta sum {naive_sum} should exceed whole {whole} here"
        );
    }

    #[test]
    fn streaming_counter_handles_empty_and_repeated_finish() {
        let mut counter = StreamingCounter::new();
        assert_eq!(counter.push(""), 0);
        assert_eq!(counter.finish(), 0);
        // finish is idempotent (tail taken, not cleared-in-place)
        counter.push("hello");
        let first = counter.finish();
        assert_eq!(first, 1);
        assert_eq!(counter.finish(), first);
    }

    /// `push` returns the counter's **running total**, not a per-delta
    /// increment. Callers must read `tokens()` (or `finish()`) for the current
    /// count — summing `push`'s returns re-counts every early token once per
    /// later delta and grows quadratically (a real 4 000-delta interrupted
    /// stream was booked as 14.7M tokens / 130 050 tok/s by exactly that
    /// mistake in the agent's request accounting).
    #[test]
    fn push_returns_running_total_not_per_delta_increment() {
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(50);
        let whole = Tokenizer::new().count(&text);
        let mut counter = StreamingCounter::new();
        let mut bogus_sum = 0i64;
        let scalars: Vec<char> = text.chars().collect();
        for chunk in scalars.chunks(8) {
            let delta: String = chunk.iter().collect();
            bogus_sum += counter.push(&delta) as i64;
        }
        // The counter itself is exact…
        assert_eq!(counter.finish(), whole);
        // …while summing the return values explodes quadratically.
        assert!(
            bogus_sum > (whole as i64) * 10,
            "cumulative sum {bogus_sum} must dwarf the exact count {whole} \
             for this many deltas"
        );
    }

    #[test]
    fn vocab_loads_and_covers_all_single_bytes() {
        let r = ranks();
        assert_eq!(r.starts.len(), 100_256);
        assert_eq!(r.blob.len(), 643_830);
        for b in 0u8..=255 {
            let rank = r.single_byte_ranks[b as usize];
            assert_eq!(r.token(rank), &[b], "byte {b:#x}");
        }
        // A known entry: "hello" is rank 15339 in cl100k_base.
        assert_eq!(r.get(b"hello"), Some(15_339));
        assert_eq!(r.get("中国".as_bytes()), Some(59_795));
    }

    /// Pretokenizer ground truth, pinned against a pure-python
    /// reimplementation of tiktoken's split + merge (itself cross-checked
    /// against published tiktoken outputs).
    #[test]
    fn pretokens_match_reference() {
        let cases: &[(&str, &[&str])] = &[
            ("hello world", &["hello", " world"]),
            ("I'm", &["I", "'m"]),
            ("we'll", &["we", "'ll"]),
            ("they've", &["they", "'ve"]),
            ("you're", &["you", "'re"]),
            ("it's", &["it", "'s"]),
            ("'sup", &["'s", "up"]),
            (" 'll", &[" '", "ll"]),
            ("a''ll", &["a", "''", "ll"]),
            ("isn't it's", &["isn", "'t", " it", "'s"]),
            ("don'ts don'", &["don", "'t", "s", " don", "'"]),
            ("你好", &["你好"]),
            ("你好，世界！", &["你好", "，", "世界", "！"]),
            ("hello   ", &["hello", "   "]),
            ("a  b", &["a", " ", " b"]),
            ("a\n\n\nb", &["a", "\n\n", "\n", "b"]),
            ("a \n b", &["a", " \n", " b"]),
            ("a\r\nb", &["a", "\r", "\n", "b"]),
            ("a\r\r b", &["a", "\r\r", " b"]),
            ("a  \n  b", &["a", "  \n ", " b"]),
            ("a\u{a0}b", &["a", "\u{a0}", "b"]),
            ("a\u{a0}\u{a0}b", &["a", "\u{a0}", "\u{a0}", "b"]),
            ("a \u{a0} b", &["a", " \u{a0}", " b"]),
            ("你好　世界", &["你好", "　", "世界"]),
            ("!!!", &["!!!"]),
            ("a\t\tb", &["a", "\t", "\t", "b"]),
            ("a \t b", &["a", " \t", " b"]),
            (
                "fn main() { let x = 42; }",
                &[
                    "fn", " main", "()", " {", " let", " x", " =", " 42", ";", " }",
                ],
            ),
            (
                "he said \"hi\" loudly",
                &["he", " said", " \"", "hi", "\"", " loudly"],
            ),
            (
                "crates/nuo_contracts/src/pressure.rs",
                &[
                    "crates",
                    "/",
                    "nuo",
                    "_",
                    "contracts",
                    "/",
                    "src",
                    "/",
                    "pressure",
                    ".",
                    "rs",
                ],
            ),
            ("\n\n\n", &["\n\n\n"]),
            (" x", &[" x"]),
            ("\nx", &["\n", "x"]),
            ("\n\n\nx", &["\n\n", "\n", "x"]),
            ("x  ", &["x", "  "]),
            ("x \n", &["x", " \n"]),
        ];
        for (input, expected) in cases {
            assert_eq!(&pretokens(input), expected, "input {input:?}");
        }
    }

    /// Token counts pinned against the same reference.
    #[test]
    fn counts_match_reference() {
        let cases: &[(&str, usize)] = &[
            ("hello world", 2),
            ("I'm", 2),
            ("we'll", 2),
            ("they've", 2),
            ("you're", 2),
            ("it's", 2),
            ("'sup", 2),
            (" 'll", 2),
            ("a''ll", 3),
            ("isn't it's", 5),
            ("don'ts don'", 5),
            ("你好", 2),
            ("中国", 1),
            ("你好，世界！", 7),
            ("你好世界", 5),
            ("antidisestablishmentarianism", 6),
            ("hello   ", 2),
            ("a  b", 3),
            ("a\n\n\nb", 4),
            ("a \n b", 3),
            ("a\r\nb", 4),
            ("!!!", 1),
            ("12345678901234567890", 8),
            ("a\t\tb", 4),
            ("a\u{a0}b", 3),
            ("你好　世界", 6),
            ("こんにちは", 1),
            ("안녕하세요", 5),
            ("Привет", 3),
            ("fn main() { let x = 42; }", 11),
            ("he said \"hi\" loudly", 6),
            ("crates/nuo_contracts/src/pressure.rs", 13),
            (
                "https://openaipublic.blob.core.windows.net/encodings/cl100k_base.tiktoken",
                27,
            ),
            ("针对当前的token预测，我们应该改变目前这种粗暴的方案。", 27),
            (
                "fn main() { let x: Vec<u32> = (0..100).map(|i| i * i).collect(); println!(\"{x:?}\"); }",
                35,
            ),
        ];
        let t = Tokenizer::new();
        for (text, expected) in cases {
            assert_eq!(&t.count(text), expected, "text: {text:?}");
        }
    }

    /// Reference counts for static corpus fixture files.
    #[test]
    fn corpus_file_counts_within_tolerance() {
        let cases: &[(&str, usize)] = &[
            ("tests/fixtures/corpus/chinese_doc.md", 538),
            ("tests/fixtures/corpus/english_doc.md", 262),
            ("tests/fixtures/corpus/code_sample.rs", 316),
            ("tests/fixtures/corpus/json_spec.json", 247),
        ];
        let t = Tokenizer::new();
        for (path, expected) in cases {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("corpus file {path} unreadable: {e}"));
            let text = text.replace("\r\n", "\n");
            let got = t.count(&text);
            assert_eq!(&got, expected, "{path}: {got} vs {expected}");
        }
    }

    #[test]
    fn round_trips() {
        let t = Tokenizer::new();
        for s in [
            "hello world",
            "fn main() { let x = 42; }",
            "你好，世界！",
            "a\u{a0}\u{a0}b",
            "!!!",
            "I'm sure we'll see it's fine",
            "crates/nuo_contracts/src/pressure.rs",
        ] {
            let enc = t.encode(s);
            assert_eq!(t.decode_bytes(&enc), s.as_bytes(), "round trip {s:?}");
        }
    }

    #[test]
    fn pathological_single_class_run_is_bounded_and_total() {
        let t = Tokenizer::new();
        // 100 KB of unbroken 'a's: chunked merge keeps this fast; the count
        // only needs to be stable and the round trip lossless.
        let s = "a".repeat(100_000);
        let n = t.count(&s);
        assert!(n > 10_000, "got {n}");
        // Deep CJK run without punctuation.
        let cjk = "人工智能".repeat(10_000);
        assert!(t.count(&cjk) > 10_000);
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(Tokenizer::new().count(""), 0);
        assert!(Tokenizer::new().encode("").is_empty());
    }

    // token-bounded truncation

    #[test]
    fn truncate_to_tokens_never_exceeds_budget_and_is_exact_prefix() {
        let t = Tokenizer::new();
        let texts: Vec<String> = vec![
            "The quick brown fox jumps over the lazy dog. ".repeat(20),
            "针对当前的token预测，我们应该改变目前这种粗暴的方案，采用BPE字节对编码算法在rust内原生实现。".repeat(10),
            "fn main() { let x: Vec<u32> = (0..100).map(|i| i * i).collect(); }".repeat(10),
            "short".to_string(),
            String::new(),
        ];
        for text in texts {
            let whole = t.count(&text);
            for budget in [0, 1, 2, 3, 7, 15, 50, 200, whole] {
                let (prefix, counted) = truncate_to_tokens(&text, budget);
                assert!(counted <= budget, "count {counted} > budget {budget}");
                assert!(prefix.len() <= text.len());
                assert!(text.starts_with(prefix));
                // The prefix's own count equals the reported count.
                assert_eq!(t.count(prefix), counted, "prefix {prefix:?}");
                // A budget ≥ whole keeps everything.
                if budget >= whole {
                    assert_eq!(prefix.len(), text.len());
                    assert_eq!(counted, whole);
                }
            }
        }
    }

    #[test]
    fn truncate_is_monotone() {
        let t = Tokenizer::new();
        let text = "mixed 中文 and code fn main() {} with punctuation !!! repeated ".repeat(10);
        let mut prev_len = 0;
        let mut prev_tokens = 0;
        for budget in 1..t.count(&text) {
            let (prefix, counted) = truncate_to_tokens(&text, budget);
            assert!(prefix.len() >= prev_len);
            assert!(counted >= prev_tokens);
            prev_len = prefix.len();
            prev_tokens = counted;
        }
    }
}
