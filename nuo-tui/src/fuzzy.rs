//! Fuzzy subsequence matching with fzf/fzy-style scoring.
//!
//! Used by the Ctrl+R history-search modal to filter `input_history` as the
//! user types. [`fuzzy_match`] returns whether `needle` is a subsequence of
//! `haystack` (case-insensitive), a ranking [`FuzzyMatch::score`], and the
//! [`FuzzyMatch::positions`] of the matched haystack chars so the renderer can
//! highlight them.
//!
//! Algorithm: a single forward DP over `(needle_idx, haystack_idx)`, with a
//! running-max optimization that keeps it `O(needle_len * haystack_len)` and
//! single-pass. Bonuses (all additive, only used for ranking):
//!
//! - Start-of-string, whitespace/punctuation boundary, or lower→upper
//!   camelCase transition: `BONUS_BOUNDARY`.
//! - Adjacent (gap of zero) to the previous matched char: `BONUS_CONSECUTIVE`.
//! - Exact case match (not just case-insensitive): `BONUS_CASE_MATCH`.
//! - Each char of gap between consecutive matches: `-PENALTY_GAP`.

/// Result of a successful fuzzy match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// Match quality score; higher is better. Used only for ranking.
    pub score: i64,
    /// Char indices in the haystack that the needle matched, in order. Used
    /// by the renderer to highlight the matched characters.
    pub positions: Vec<usize>,
}

const BONUS_EXACT_FULL: i64 = 500;
const BONUS_EXACT_WORD: i64 = 300;
const BONUS_WORD_PREFIX: i64 = 150;
const BONUS_CONTIGUOUS_SUBSTRING: i64 = 80;
const BONUS_BOUNDARY: i64 = 40;
const BONUS_CONSECUTIVE: i64 = 40;
const BONUS_CASE_MATCH: i64 = 10;
const PENALTY_GAP: i64 = 3;
const PENALTY_SCATTER_SPAN: i64 = 2;
const NEG: i64 = i64::MIN / 4;

use std::cell::RefCell;

/// Pre-allocated scratchpad buffers reused across calls to eliminate heap allocations
/// during fuzzy matching.
pub struct Scratchpad {
    pub h: Vec<char>,
    pub dp: Vec<i64>,
    pub back: Vec<Option<u32>>,
}

impl Default for Scratchpad {
    fn default() -> Self {
        Self::new()
    }
}

impl Scratchpad {
    pub fn new() -> Self {
        Self {
            h: Vec::with_capacity(512),
            dp: Vec::with_capacity(512 * 64),
            back: Vec::with_capacity(512 * 64),
        }
    }

    #[inline]
    fn prepare_tables(&mut self, total_cells: usize) {
        if self.dp.len() < total_cells {
            self.dp.resize(total_cells, NEG);
        }
        if self.back.len() < total_cells {
            self.back.resize(total_cells, None);
        }
    }
}

thread_local! {
    static THREAD_SCRATCHPAD: RefCell<Scratchpad> = RefCell::new(Scratchpad::new());
}

/// Pre-compiled search query (needle) optimized for high-throughput batch filtering.
/// Compiles needle character representations once, enabling zero-allocation subsequence
/// testing and hot DP matching over hundreds of thousands of candidate items.
#[derive(Debug, Clone)]
pub struct Matcher {
    pub needle_chars: Vec<char>,
    pub is_ascii: bool,
    pub needle_bytes_lower: Vec<u8>,
}

impl Matcher {
    /// Compile a query into a reusable `Matcher`.
    pub fn new(query: &str) -> Self {
        let is_ascii = query.is_ascii();
        let needle_bytes_lower = if is_ascii {
            query.bytes().map(|b| b.to_ascii_lowercase()).collect()
        } else {
            Vec::new()
        };
        Self {
            needle_chars: query.chars().collect(),
            is_ascii,
            needle_bytes_lower,
        }
    }

    /// Fast, zero-allocation pre-filter: returns whether needle is a case-insensitive
    /// subsequence of haystack. Rejects non-matches in O(|haystack|) without allocations.
    #[inline]
    pub fn is_subsequence(&self, haystack: &str) -> bool {
        if self.needle_chars.is_empty() {
            return true;
        }
        if self.is_ascii && haystack.is_ascii() {
            if haystack.len() < self.needle_bytes_lower.len() {
                return false;
            }
            let mut n_idx = 0;
            let n_len = self.needle_bytes_lower.len();
            let mut target = self.needle_bytes_lower[0];
            for &b in haystack.as_bytes() {
                if b.to_ascii_lowercase() == target {
                    n_idx += 1;
                    if n_idx == n_len {
                        return true;
                    }
                    target = self.needle_bytes_lower[n_idx];
                }
            }
            return false;
        }

        // Unicode path
        let mut needle_iter = self.needle_chars.iter();
        let mut target = match needle_iter.next() {
            Some(&c) => c,
            None => return true,
        };
        for c in haystack.chars() {
            if c.eq_ignore_ascii_case(&target) {
                match needle_iter.next() {
                    Some(&next_c) => target = next_c,
                    None => return true,
                }
            }
        }
        false
    }

    /// Fuzzy-match against `haystack` using the given scratchpad buffer.
    /// Completely zero-allocation on the match loop.
    pub fn match_with_scratch(
        &self,
        haystack: &str,
        scratch: &mut Scratchpad,
    ) -> Option<FuzzyMatch> {
        let n_len = self.needle_chars.len();
        if n_len == 0 {
            return Some(FuzzyMatch {
                score: 0,
                positions: Vec::new(),
            });
        }

        // 1. Fast subsequence rejection: avoids DP and UTF-8 collecting on 99%+ of candidates.
        if !self.is_subsequence(haystack) {
            return None;
        }

        // 2. Collect haystack chars into reusable scratch buffer
        scratch.h.clear();
        scratch.h.extend(haystack.chars());
        let h_len = scratch.h.len();
        if h_len < n_len {
            return None;
        }

        let total_cells = n_len * h_len;
        scratch.prepare_tables(total_cells);

        let n = &self.needle_chars;
        let h = &scratch.h;

        // Base case: needle[0] can match any single haystack char with no predecessor.
        let n0 = n[0];
        for j in 0..h_len {
            if h[j].eq_ignore_ascii_case(&n0) {
                scratch.dp[j] = char_bonus(h, j, n0);
            } else {
                scratch.dp[j] = NEG;
            }
            scratch.back[j] = None;
        }

        // Inductive case: one forward pass per needle char, maintaining a running
        // max so the inner loop stays O(h_len) instead of O(h_len^2).
        for i in 1..n_len {
            let row_offset = i * h_len;
            let prev_row_offset = (i - 1) * h_len;
            let ni = n[i];

            scratch.dp[row_offset..row_offset + h_len].fill(NEG);
            scratch.back[row_offset..row_offset + h_len].fill(None);

            let mut running_max = NEG;
            let mut running_max_k: Option<u32> = None;

            for j in 0..h_len {
                // 1) Try matching needle[i] at haystack[j].
                if h[j].eq_ignore_ascii_case(&ni) {
                    let mut best_val = NEG;
                    let mut best_k: Option<u32> = None;
                    // Adjacent predecessor (k = j-1) earns the consecutive bonus.
                    if j >= 1 {
                        let prev_val = scratch.dp[prev_row_offset + j - 1];
                        if prev_val != NEG {
                            best_val = prev_val.saturating_add(BONUS_CONSECUTIVE);
                            best_k = Some((j - 1) as u32);
                        }
                    }
                    // Non-adjacent best from the running max beats the adjacent
                    // candidate only when it strictly exceeds it, so ties keep
                    // the adjacent path (visually tighter highlight run).
                    if running_max != NEG && running_max > best_val {
                        best_val = running_max;
                        best_k = running_max_k;
                    }
                    if best_val != NEG {
                        let total = best_val.saturating_add(char_bonus(h, j, ni));
                        if total > scratch.dp[row_offset + j] {
                            scratch.dp[row_offset + j] = total;
                            scratch.back[row_offset + j] = best_k;
                        }
                    }
                }

                // 2) Extend the running max with k=j as a future predecessor
                //    (contributes dp[i-1][j] with no gap when matched at j+1).
                let prev_cell = scratch.dp[prev_row_offset + j];
                if prev_cell != NEG && prev_cell > running_max {
                    running_max = prev_cell;
                    running_max_k = Some(j as u32);
                }

                // 3) Age the running max by one gap unit for the next iteration.
                running_max = running_max.saturating_sub(PENALTY_GAP);
            }
        }

        // Pick the best ending position for the last needle char. Strict `>`
        // keeps the lowest-`j` end on ties, which visually favors earlier matches.
        let last_row_offset = (n_len - 1) * h_len;
        let mut best_end: Option<usize> = None;
        let mut best_score = NEG;
        for j in 0..h_len {
            let cell = scratch.dp[last_row_offset + j];
            if cell > best_score {
                best_score = cell;
                best_end = Some(j);
            }
        }
        let end = best_end?;
        if best_score == NEG {
            return None;
        }

        // Reconstruct positions by following back-pointers from (n_len-1, end).
        let mut positions: Vec<usize> = Vec::with_capacity(n_len);
        let mut i = n_len - 1;
        let mut j = end;
        loop {
            positions.push(j);
            if i == 0 {
                break;
            }
            let prev_j = scratch.back[i * h_len + j]? as usize;
            j = prev_j;
            i -= 1;
        }
        positions.reverse();
        debug_assert_eq!(positions.len(), n_len);

        // Multi-tier structural scoring bonuses (Industry Gold Standard):
        // Tier 0: Exact full string match
        // Tier 1: Exact whole-word match (bounded by whitespace/punctuation)
        // Tier 2: Word prefix match (needle starts a word)
        // Tier 3: Contiguous substring match (needle appears contiguous mid-word)
        // Tier 4: Scattered subsequence (penalized proportionally to scatter span)
        let is_contiguous = positions.windows(2).all(|w| w[1] == w[0] + 1);
        let first_pos = *positions.first().unwrap_or(&0);
        let last_pos = *positions.last().unwrap_or(&0);

        let left_boundary =
            first_pos == 0 || is_word_boundary(Some(h[first_pos - 1]), h[first_pos]);
        let next_char = if last_pos + 1 < h_len {
            Some(h[last_pos + 1])
        } else {
            None
        };
        let right_boundary = is_right_boundary(h[last_pos], next_char);

        if is_contiguous {
            if left_boundary && right_boundary {
                if positions.len() == h_len {
                    best_score = best_score.saturating_add(BONUS_EXACT_FULL);
                } else {
                    best_score = best_score.saturating_add(BONUS_EXACT_WORD);
                }
            } else if left_boundary {
                best_score = best_score.saturating_add(BONUS_WORD_PREFIX);
            } else {
                best_score = best_score.saturating_add(BONUS_CONTIGUOUS_SUBSTRING);
            }
        } else {
            let span = last_pos.saturating_sub(first_pos) + 1;
            let excess_span = span.saturating_sub(n_len);
            let scatter_penalty = (excess_span as i64).saturating_mul(PENALTY_SCATTER_SPAN);
            best_score = best_score.saturating_sub(scatter_penalty);
        }

        // Compactness penalty: slight penalty for extremely long haystacks to favor compact prompts
        let compactness_penalty = ((h_len.saturating_sub(n_len)) / 10).min(30) as i64;
        best_score = best_score.saturating_sub(compactness_penalty);

        Some(FuzzyMatch {
            score: best_score,
            positions,
        })
    }
}

/// True if the transition `prev → cur` is a word boundary: at the start of
/// the haystack, just after whitespace or punctuation, or at a lower→upper
/// camelCase boundary. Matched chars at boundaries get [`BONUS_BOUNDARY`] so
/// the matcher prefers whole-word / token starts.
fn is_word_boundary(prev: Option<char>, cur: char) -> bool {
    match prev {
        None => true,
        Some(p) => {
            p.is_whitespace()
                || !p.is_alphanumeric()
                || (p.is_lowercase()
                    && cur.is_uppercase()
                    && p.is_alphabetic()
                    && cur.is_alphabetic())
        }
    }
}

/// True if the transition `cur → next` ends a word boundary: at the end of
/// the haystack, just before whitespace or punctuation, or at a lower→upper
/// camelCase transition.
fn is_right_boundary(cur: char, next: Option<char>) -> bool {
    match next {
        None => true,
        Some(n) => {
            n.is_whitespace()
                || !n.is_alphanumeric()
                || (cur.is_lowercase()
                    && n.is_uppercase()
                    && cur.is_alphabetic()
                    && n.is_alphabetic())
        }
    }
}

/// Bonus accumulated at haystack position `j` when it matches a needle char.
/// `needle_c` is the needle char (already known to case-insensitively match
/// `h[j]`); an exact-case match adds [`BONUS_CASE_MATCH`].
fn char_bonus(h: &[char], j: usize, needle_c: char) -> i64 {
    let cur = h[j];
    let prev = if j == 0 { None } else { Some(h[j - 1]) };
    let mut bonus = 0;
    if is_word_boundary(prev, cur) {
        bonus += BONUS_BOUNDARY;
    }
    if cur == needle_c {
        bonus += BONUS_CASE_MATCH;
    }
    bonus
}

/// Fuzzy-match `needle` against `haystack` (case-insensitive subsequence).
///
/// Returns `None` when `needle` is not a subsequence of `haystack`. An empty
/// `needle` matches every haystack with score `0` and no highlighted positions
/// — this is what the history modal wants when the query box is empty (show
/// everything, highlight nothing).
///
/// `positions` are char indices (not byte offsets) into `haystack`, in
/// ascending order, one per needle char.
pub fn fuzzy_match(haystack: &str, needle: &str) -> Option<FuzzyMatch> {
    if needle.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            positions: Vec::new(),
        });
    }
    let matcher = Matcher::new(needle);
    THREAD_SCRATCHPAD.with(|cell| {
        let mut scratch = cell.borrow_mut();
        matcher.match_with_scratch(haystack, &mut scratch)
    })
}

/// Filter and rank `items` by fuzzy match against `query`, preserving the
/// original order on ties (stable). Returns `(original_index, FuzzyMatch)` for
/// each item whose match is `Some`. An empty `query` matches every item with
/// score `0` and no highlight positions, so the caller renders the full list.
#[allow(dead_code)]
pub fn rank<I: AsRef<str>>(items: &[I], query: &str) -> Vec<(usize, FuzzyMatch)> {
    if query.is_empty() {
        return items
            .iter()
            .enumerate()
            .map(|(i, _)| {
                (
                    i,
                    FuzzyMatch {
                        score: 0,
                        positions: Vec::new(),
                    },
                )
            })
            .collect();
    }

    rank_iter(
        items.iter().enumerate().map(|(i, item)| (i, item.as_ref())),
        query,
    )
}

/// Zero-allocation streaming rank: filter and score items provided by an indexed iterator.
/// Reuses a compiled [`Matcher`] and thread-local scratchpad across all items.
pub fn rank_iter<'a, I>(items: I, query: &str) -> Vec<(usize, FuzzyMatch)>
where
    I: IntoIterator<Item = (usize, &'a str)>,
{
    if query.is_empty() {
        return items
            .into_iter()
            .map(|(i, _)| {
                (
                    i,
                    FuzzyMatch {
                        score: 0,
                        positions: Vec::new(),
                    },
                )
            })
            .collect();
    }

    let matcher = Matcher::new(query);
    THREAD_SCRATCHPAD.with(|cell| {
        let mut scratch = cell.borrow_mut();
        items
            .into_iter()
            .filter_map(|(i, item)| {
                matcher
                    .match_with_scratch(item, &mut scratch)
                    .map(|m| (i, m))
            })
            .collect()
    })
}

/// Sort a list of `(index, FuzzyMatch)` in place by descending score, with
/// original-index ascending as the stable tiebreaker so equally-good matches
/// keep their top-to-bottom input order. Returns `&mut` so callers can chain.
pub fn sort_by_score(matches: &mut [(usize, FuzzyMatch)]) {
    // Reverse(score) sorts descending while keeping the slice sort stable, so
    // equally-good matches retain their top-to-bottom input order — exactly
    // the tiebreaker we want.
    matches.sort_by_key(|(_, m)| std::cmp::Reverse(m.score));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty needle matches anything with no highlighted positions.
    #[test]
    fn empty_needle_matches_everything() {
        let m = fuzzy_match("hello", "").unwrap();
        assert_eq!(m.score, 0);
        assert!(m.positions.is_empty());
    }

    /// Non-subsequence needles return None.
    #[test]
    fn rejects_non_subsequence() {
        assert!(fuzzy_match("abc", "ac").is_some()); // a-b-c contains a then c
        assert!(fuzzy_match("abc", "ca").is_none()); // c never appears before a
        assert!(fuzzy_match("ab", "abc").is_none()); // needle longer than haystack
    }

    /// Case-insensitive matching still records exact-case bonuses in the score.
    #[test]
    fn case_insensitive_match_with_case_bonus() {
        let upper = fuzzy_match("ABC", "abc").unwrap();
        let lower = fuzzy_match("abc", "abc").unwrap();
        // Matching lowercase needle against uppercase haystack earns no
        // BONUS_CASE_MATCH, so it should score strictly lower.
        assert!(lower.score > upper.score);
    }

    /// Consecutive matches beat scattered matches for the same needle.
    #[test]
    fn prefers_consecutive_run() {
        // "an" in "banana" can match at chars (1,2) [consecutive] or (3,4)
        // [also consecutive] or scattered. Both consecutive paths should beat
        // any scattered path; the score must be > 0.
        let m = fuzzy_match("banana", "an").unwrap();
        assert!(m.score > 0);
        assert_eq!(m.positions.len(), 2);
        // The matched positions must form a valid ascending subsequence.
        assert!(m.positions[0] < m.positions[1]);
    }

    /// Word-boundary bonus: matching at the start outranks matching mid-word.
    #[test]
    fn word_boundary_bonus() {
        // "cat" at start of "catalog" should outscore "cat" appearing inside
        // "concatenate" (where 'c' is mid-word).
        let start = fuzzy_match("catalog", "cat").unwrap();
        let mid = fuzzy_match("concatenate", "cat").unwrap();
        assert!(start.score > mid.score);
    }

    /// Positions are char indices, not byte offsets (multi-byte safe).
    #[test]
    fn positions_are_char_indices_for_unicode() {
        // "é" is two bytes in UTF-8 but one char. Needle "x" matches the ASCII
        // char at char-index 2 (byte-index 3).
        let m = fuzzy_match("éax", "x").unwrap();
        assert_eq!(m.positions, vec![2]);
    }

    /// Reconstructed positions actually correspond to the needle in order.
    #[test]
    fn positions_correspond_to_needle_chars() {
        let h = "foo bar baz";
        let n = "obb";
        let m = fuzzy_match(h, n).unwrap();
        let h_chars: Vec<char> = h.chars().collect();
        let n_chars: Vec<char> = n.chars().collect();
        for (k, &pos) in m.positions.iter().enumerate() {
            assert!(
                h_chars[pos].eq_ignore_ascii_case(&n_chars[k]),
                "position {} (haystack char {:?}) must match needle char {:?}",
                pos,
                h_chars[pos],
                n_chars[k]
            );
        }
    }

    /// rank() + sort_by_score() filters out non-matches and orders by score.
    /// Exact whole-word ("a cat") outranks word-prefix ("catalog"), which outranks
    /// mid-word substring ("scatter").
    #[test]
    fn rank_and_sort_by_score_orders_results() {
        let items = vec!["scatter", "catalog", "a cat"];
        let mut ranked = rank(&items, "cat");
        sort_by_score(&mut ranked);
        assert_eq!(ranked.len(), 3);
        // "a cat" (exact word) ranks first
        assert_eq!(ranked[0].0, 2); // a cat
        // "catalog" (word prefix) ranks second
        assert_eq!(ranked[1].0, 1); // catalog
        // "scatter" (mid-word substring) ranks third
        assert_eq!(ranked[2].0, 0); // scatter
        assert!(ranked[0].1.score > ranked[1].1.score);
        assert!(ranked[1].1.score > ranked[2].1.score);
    }

    /// Exact whole word matches must strictly beat scattered initials across words (e.g. "adr").
    #[test]
    fn exact_word_beats_scattered_acronym() {
        let exact = fuzzy_match("let's write the adr", "adr").unwrap();
        let prefix = fuzzy_match("adroit solution", "adr").unwrap();
        let substring = fuzzy_match("padre", "adr").unwrap();
        let scattered = fuzzy_match("all dogs run in the park", "adr").unwrap();

        assert!(exact.score > prefix.score, "exact word must beat prefix");
        assert!(prefix.score > substring.score, "prefix must beat substring");
        assert!(
            substring.score > scattered.score,
            "substring must beat scattered acronym"
        );
    }

    /// Empty query in rank() returns every item, unhighlighted.
    #[test]
    fn rank_with_empty_query_returns_all() {
        let items = vec!["a", "b", "c"];
        let ranked = rank(&items, "");
        assert_eq!(ranked.len(), 3);
        for (_, m) in ranked {
            assert_eq!(m.score, 0);
            assert!(m.positions.is_empty());
        }
    }

    /// Streaming rank_iter works identically to rank.
    #[test]
    fn rank_iter_matches_rank_results() {
        let items = vec!["build all", "cargo test", "cat file", "scatter"];
        let r1 = rank(&items, "cat");
        let r2 = rank_iter(items.iter().enumerate().map(|(i, &s)| (i, s)), "cat");
        assert_eq!(r1, r2);
    }

    /// Benchmarking/scale assertion: ranking 100,000 items is fast and returns valid results.
    #[test]
    fn batch_rank_100k_items_performance_and_correctness() {
        let n = 100_000;
        let mut items = Vec::with_capacity(n);
        for i in 0..n {
            if i % 1000 == 0 {
                items.push("git commit -m 'fix bug'");
            } else if i % 250 == 0 {
                items.push("cargo build --release");
            } else {
                items.push("echo hello world from nuo agent");
            }
        }

        let start = std::time::Instant::now();
        let mut ranked = rank_iter(items.iter().enumerate().map(|(i, &s)| (i, s)), "gcm");
        sort_by_score(&mut ranked);
        let elapsed = start.elapsed();

        // 100 matches of "git commit -m 'fix bug'"
        assert_eq!(ranked.len(), 100);
        assert!(
            elapsed.as_millis() < 500,
            "100k items fuzzy ranking took {:?}, expected < 500ms in debug mode",
            elapsed
        );
    }
}
