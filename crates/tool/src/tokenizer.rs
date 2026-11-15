//! Fast, zero-dependency, class-based token estimator for LLM context management.
//!
//! Replaces heavy vocabulary-based BPE (tiktoken `cl100k_base.packed`) with a
//! lean, single-pass $O(N)$ Unicode/class-based token estimator.
//!
//! Context pressure, compaction gates, and prompt budgeting in modern multi-model
//! environments (GPT-4o, Claude 3.5/3.7, DeepSeek, Qwen) do not share a single
//! vendor vocabulary. This estimator provides high statistical fidelity across
//! modern LLM architectures without external binary artifacts or runtime heap
//! table initialization.
//!
//! See [ADR-0056](../../../docs/adr/0056-zero-dependency-class-based-token-estimator.md).

/// Tokenizer handle matching the canonical interface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokenizer;

impl Tokenizer {
    /// Creates a new tokenizer instance.
    pub fn new() -> Self {
        Self
    }

    /// Estimate token count for `text`.
    pub fn count(&self, text: &str) -> usize {
        count_tokens(text)
    }
}

/// Character classification category for token piece formation.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum CharCategory {
    Whitespace,
    AsciiWord,
    Cjk,
    Punctuation,
    Other,
}

#[inline]
fn classify(c: char) -> CharCategory {
    if c.is_ascii_whitespace() {
        CharCategory::Whitespace
    } else if c.is_ascii_alphanumeric() || c == '_' {
        CharCategory::AsciiWord
    } else if is_cjk(c) {
        CharCategory::Cjk
    } else if c.is_ascii_punctuation() {
        CharCategory::Punctuation
    } else {
        CharCategory::Other
    }
}

#[inline]
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'     // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}'   // CJK Unified Ideographs Ext A
        | '\u{20000}'..='\u{2A6DF}' // CJK Ext B
        | '\u{F900}'..='\u{FAFF}'   // CJK Compatibility Ideographs
        | '\u{3040}'..='\u{309F}'   // Hiragana
        | '\u{30A0}'..='\u{30FF}'   // Katakana
        | '\u{AC00}'..='\u{D7AF}'   // Hangul Syllables
        | '\u{3000}'..='\u{303F}'   // CJK Symbols and Punctuation
        | '\u{FF00}'..='\u{FFEF}'   // Halfwidth and Fullwidth Forms
    )
}

/// Represents a scanned lexical token piece with byte offsets in the source text.
#[derive(Debug, Clone, Copy)]
struct Piece {
    start: usize,
    end: usize,
    tokens: usize,
}

/// Estimate the token cost of a contiguous run of characters within the same category.
fn piece_tokens(category: CharCategory, slice: &str) -> usize {
    match category {
        CharCategory::Whitespace => {
            // Count newlines vs spaces
            let mut newlines = 0usize;
            let mut spaces = 0usize;
            for b in slice.bytes() {
                if b == b'\n' {
                    newlines += 1;
                } else {
                    spaces += 1;
                }
            }
            let nl_tokens = newlines.div_ceil(2);
            let sp_tokens = spaces.div_ceil(4);
            (nl_tokens + sp_tokens).max(1)
        }
        CharCategory::AsciiWord => {
            // For words: short words (1..5 chars) are 1 token. Longer words break into subwords.
            let len = slice.len();
            if len <= 5 {
                1
            } else {
                1 + (len - 5).div_ceil(4)
            }
        }
        CharCategory::Cjk => {
            // Modern models (DeepSeek, Qwen, Claude, GPT-4o) encode CJK at ~1 token per character.
            slice.chars().count()
        }
        CharCategory::Punctuation => {
            // Runs of identical punctuation (e.g. `===`, `---`, `///`): (len + 2) / 3
            let len = slice.len();
            if len <= 1 { 1 } else { len.div_ceil(3) }
        }
        CharCategory::Other => {
            // Other unicode graphemes / symbols: 1 token each
            slice.chars().count().max(1)
        }
    }
}

/// Scans `text` into non-overlapping `Piece`s.
fn scan_pieces(text: &str) -> Vec<Piece> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut pieces = Vec::new();
    let mut chars = text.char_indices().peekable();

    while let Some((start, ch)) = chars.next() {
        let cat = classify(ch);
        let mut end = start + ch.len_utf8();

        match cat {
            CharCategory::Cjk => {
                // Each CJK char is an independent semantic piece
                pieces.push(Piece {
                    start,
                    end,
                    tokens: 1,
                });
            }
            CharCategory::Punctuation => {
                // Group runs of identical punctuation (e.g. `---`, `===`)
                while let Some(&(_, next_ch)) = chars.peek() {
                    if next_ch == ch {
                        chars.next();
                        end += next_ch.len_utf8();
                    } else {
                        break;
                    }
                }
                let tokens = piece_tokens(cat, &text[start..end]);
                pieces.push(Piece { start, end, tokens });
            }
            CharCategory::AsciiWord | CharCategory::Whitespace | CharCategory::Other => {
                // Group contiguous characters of the same category
                while let Some(&(_, next_ch)) = chars.peek() {
                    if classify(next_ch) == cat {
                        chars.next();
                        end += next_ch.len_utf8();
                    } else {
                        break;
                    }
                }
                let tokens = piece_tokens(cat, &text[start..end]);
                pieces.push(Piece { start, end, tokens });
            }
        }
    }

    pieces
}

/// Token count via the fast class-based estimator — the predictor behind
/// context pressure, compaction gates, and prompt budget calculations.
pub fn count_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let mut total = 0usize;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        let cat = classify(ch);
        let mut run_len = 1usize;
        let mut run_bytes = ch.len_utf8();

        match cat {
            CharCategory::Cjk => {
                total += 1;
            }
            CharCategory::Punctuation => {
                while let Some(&next_ch) = chars.peek() {
                    if next_ch == ch {
                        chars.next();
                        run_len += 1;
                        run_bytes += next_ch.len_utf8();
                    } else {
                        break;
                    }
                }
                total += if run_len <= 1 { 1 } else { run_len.div_ceil(3) };
            }
            CharCategory::AsciiWord => {
                while let Some(&next_ch) = chars.peek() {
                    if classify(next_ch) == CharCategory::AsciiWord {
                        chars.next();
                        run_bytes += next_ch.len_utf8();
                    } else {
                        break;
                    }
                }
                total += if run_bytes <= 5 {
                    1
                } else {
                    1 + (run_bytes - 5).div_ceil(4)
                };
            }
            CharCategory::Whitespace => {
                let mut newlines: usize = if ch == '\n' { 1 } else { 0 };
                let mut spaces: usize = if ch != '\n' { 1 } else { 0 };
                while let Some(&next_ch) = chars.peek() {
                    if classify(next_ch) == CharCategory::Whitespace {
                        chars.next();
                        if next_ch == '\n' {
                            newlines += 1;
                        } else {
                            spaces += 1;
                        }
                    } else {
                        break;
                    }
                }
                let nl_tokens = newlines.div_ceil(2);
                let sp_tokens = spaces.div_ceil(4);
                total += (nl_tokens + sp_tokens).max(1);
            }
            CharCategory::Other => {
                while let Some(&next_ch) = chars.peek() {
                    if classify(next_ch) == CharCategory::Other {
                        chars.next();
                        run_len += 1;
                    } else {
                        break;
                    }
                }
                total += run_len.max(1);
            }
        }
    }

    total
}

/// Largest prefix of `text` that tokenizes to **at most** `max_tokens`,
/// plus its token count.
///
/// Guaranteed properties:
/// 1. Returned string slice is always sliced on valid UTF-8 character boundaries.
/// 2. `tokens <= max_tokens`.
/// 3. Monotonic with respect to increasing `max_tokens`.
pub fn truncate_to_tokens(text: &str, max_tokens: usize) -> (&str, usize) {
    if max_tokens == 0 || text.is_empty() {
        return ("", 0);
    }

    let pieces = scan_pieces(text);
    let mut total_tokens = 0usize;
    let mut consumed_bytes = 0usize;

    for piece in pieces {
        if total_tokens >= max_tokens {
            break;
        }

        if total_tokens + piece.tokens <= max_tokens {
            total_tokens += piece.tokens;
            consumed_bytes = piece.end;
        } else {
            // Piece does not fit whole. If it has multiple tokens (e.g. a long word or multi-char piece),
            // take a prefix of characters that fits within remaining tokens.
            let remaining = max_tokens - total_tokens;
            if remaining == 0 {
                break;
            }
            let slice = &text[piece.start..piece.end];
            let mut piece_bytes = 0usize;
            let mut piece_tokens_accum = 0usize;

            for ch in slice.chars() {
                let next_bytes = piece_bytes + ch.len_utf8();
                let sub_cost = count_tokens(&slice[..next_bytes]);
                if sub_cost <= remaining {
                    piece_bytes = next_bytes;
                    piece_tokens_accum = sub_cost;
                } else {
                    break;
                }
            }

            if piece_bytes > 0 {
                consumed_bytes = piece.start + piece_bytes;
                total_tokens += piece_tokens_accum;
            }
            break;
        }
    }

    (&text[..consumed_bytes], total_tokens)
}

/// Byte length of the largest ≤ `max_tokens` prefix.
pub fn truncate_str_to_tokens(text: &str, max_tokens: usize) -> &str {
    truncate_to_tokens(text, max_tokens).0
}

/// Streaming token counter that accumulates deltas and produces an exact
/// total matching `count_tokens` on completion.
#[derive(Debug, Clone, Default)]
pub struct StreamingCounter {
    /// Incomplete trailing buffer that may extend with future deltas.
    tail: String,
    /// Finalized token count so far.
    tokens: usize,
}

impl StreamingCounter {
    /// Creates a new streaming counter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one stream delta and returns the running total count.
    pub fn push(&mut self, delta: &str) -> usize {
        if delta.is_empty() {
            return self.tokens;
        }

        self.tail.push_str(delta);
        let pieces = scan_pieces(&self.tail);

        if pieces.len() <= 1 {
            // Only one piece in tail — keep it uncommitted as it may extend
            return self.tokens;
        }

        // Commit all pieces except the last one (which may extend)
        let commit_through = pieces.len() - 1;
        let mut committed_bytes = 0usize;
        for piece in &pieces[..commit_through] {
            self.tokens += piece.tokens;
            committed_bytes = piece.end;
        }

        self.tail.drain(..committed_bytes);
        self.tokens
    }

    /// Closes the stream and returns the final total count.
    pub fn finish(&mut self) -> usize {
        if !self.tail.is_empty() {
            self.tokens += count_tokens(&self.tail);
            self.tail.clear();
        }
        self.tokens
    }

    /// Running count without closing the stream.
    pub fn tokens(&self) -> usize {
        self.tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every chunking of a text must stream-count to the exact whole count.
    fn assert_streaming_exact(text: &str, sizes: &[usize]) {
        let whole = Tokenizer::new().count(text);
        for &size in sizes {
            let mut counter = StreamingCounter::new();
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
    fn empty_is_zero() {
        assert_eq!(count_tokens(""), 0);
        assert_eq!(Tokenizer::new().count(""), 0);
    }

    #[test]
    fn counts_are_positive_and_monotonic() {
        let base = "hello";
        let extended = "hello world";
        assert!(count_tokens(base) > 0);
        assert!(count_tokens(extended) >= count_tokens(base));
    }

    #[test]
    fn cjk_counts_match_character_densities() {
        let text = "你好，世界！这是一个测试。";
        let count = count_tokens(text);
        // 13 characters -> approximately 13 tokens in modern tokenizers
        assert!((10..=15).contains(&count), "got {count}");
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
    fn streaming_counter_handles_empty_and_repeated_finish() {
        let mut counter = StreamingCounter::new();
        assert_eq!(counter.push(""), 0);
        assert_eq!(counter.finish(), 0);
        counter.push("hello");
        let first = counter.finish();
        assert!(first > 0);
        assert_eq!(counter.finish(), first);
    }

    #[test]
    fn truncate_to_tokens_never_exceeds_budget_and_is_exact_prefix() {
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(20);
        for budget in [0, 1, 5, 10, 25, 50, 100, 500] {
            let (prefix, tokens) = truncate_to_tokens(&text, budget);
            assert!(tokens <= budget, "tokens {tokens} exceeded budget {budget}");
            assert!(
                text.starts_with(prefix),
                "prefix {prefix:?} is not a prefix of text"
            );
            assert_eq!(
                tokens,
                count_tokens(prefix),
                "reported tokens {tokens} must match count_tokens({prefix:?})"
            );
        }
    }

    #[test]
    fn truncate_is_monotone() {
        let text = "fn main() { println!(\"hello, world!\"); } let a = 1; let b = 2;";
        let mut prev_len = 0;
        let mut prev_tokens = 0;
        for budget in 0..=30 {
            let (prefix, tokens) = truncate_to_tokens(text, budget);
            assert!(
                prefix.len() >= prev_len,
                "prefix len shrank from {prev_len} to {} at budget {budget}",
                prefix.len()
            );
            assert!(
                tokens >= prev_tokens,
                "tokens shrank from {prev_tokens} to {tokens} at budget {budget}"
            );
            prev_len = prefix.len();
            prev_tokens = tokens;
        }
    }
}
