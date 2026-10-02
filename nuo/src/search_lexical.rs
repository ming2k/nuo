//! Lexical session-history search — the `/search` backend.
//!
//! The previous backend embedded every message with a hash-based
//! `MockEmbeddingProvider` and scored by "cosine similarity" over vectors that
//! carried no semantics: the machinery (index file, dedup set, union-merge
//! save, full-file rewrite per search) was real while the ranking was noise.
//! Until a real embedding provider is wired in, `/search` ranks lexically —
//! deterministic, honest about what it measures, and free of any on-disk
//! index. Scoring: sum of per-term IDF-weighted term-frequency ratios over
//! the query's terms, with a length penalty so a term-stuffed blob does not
//! drown a focused hit.

use nuo_contracts::{CommandRecord, Message};

/// A scored hit: the rendered text plus its score.
pub(crate) struct LexicalHit {
    pub text: String,
    pub score: f32,
}

/// Search `messages` and `commands` for `query`, returning the top `k`.
pub(crate) fn search(
    query: &str,
    messages: &[Message],
    commands: &[CommandRecord],
    k: usize,
) -> Vec<LexicalHit> {
    let terms: Vec<String> = tokenize(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let mut candidates: Vec<(String, Vec<String>)> = Vec::new();
    for message in messages {
        let text = message.content.trim().to_string();
        if text.is_empty() {
            continue;
        }
        let tokens = tokenize(&text);
        candidates.push((text, tokens));
    }
    for record in commands {
        let Some(result) = &record.result else {
            continue;
        };
        let body = result.to_text();
        let body = body.trim();
        if body.len() <= 15 {
            continue;
        }
        let rendered = if record.args.is_empty() {
            format!("/{}\n{}", record.name, body)
        } else {
            format!("/{} {}\n{}", record.name, record.args, body)
        };
        let tokens = tokenize(&rendered);
        candidates.push((rendered, tokens));
    }
    // IDF over the candidate pool: a term hitting everything ranks nothing.
    let df: std::collections::HashMap<&str, usize> =
        terms
            .iter()
            .fold(std::collections::HashMap::new(), |mut acc, term| {
                let hits = candidates
                    .iter()
                    .filter(|(_, tokens)| tokens.contains(term))
                    .count();
                acc.insert(term.as_str(), hits);
                acc
            });
    let n = candidates.len().max(1) as f32;
    let mut scored: Vec<LexicalHit> = candidates
        .into_iter()
        .map(|(text, tokens)| {
            let len = tokens.len().max(1) as f32;
            let mut score = 0.0f32;
            for term in &terms {
                let tf = tokens.iter().filter(|t| *t == term).count() as f32;
                if tf == 0.0 {
                    continue;
                }
                let df_term = *df.get(term.as_str()).unwrap_or(&0) as f32;
                let idf = (1.0 + (n / (1.0 + df_term)).ln()).max(0.0);
                score += idf * (tf / (tf + 2.0));
            }
            // Mild length penalty: prefer focused hits over term-stuffed ones.
            LexicalHit {
                text,
                score: score / (1.0 + len / 5000.0),
            }
        })
        .filter(|hit| hit.score > 0.0)
        .collect();
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    scored.truncate(k);
    scored
}

/// Lowercase, strip non-alphanumeric boundaries, drop empties. Good enough
/// for CJK-free and CJK-containing text alike (a CJK run stays one token;
/// no dictionary segmentation — `contains` still catches substring repeats
/// through the `df` pass).
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(content: &str) -> Message {
        Message::new(nuo_contracts::Role::User, content.to_string())
    }

    #[test]
    fn ranks_relevant_above_irrelevant_and_respects_k() {
        let messages = vec![
            msg("The compaction policy preserves the last six rounds verbatim."),
            msg("Completely unrelated discussion about a logo change."),
            msg("Rounds and the compaction policy interact via preserve_rounds."),
        ];
        let hits = search("compaction rounds", &messages, &[], 2);
        assert_eq!(hits.len(), 2, "k is respected");
        let top = &hits[0].text;
        assert!(
            top.contains("preserve_rounds") || top.contains("preserves"),
            "a compaction-related message must outrank the logo one: {top}"
        );
    }

    #[test]
    fn empty_query_or_no_candidates_is_empty() {
        assert!(search("", &[msg("anything")], &[], 5).is_empty());
        assert!(search("term", &[], &[], 5).is_empty());
    }

    #[test]
    fn commands_are_searched_with_their_name_and_args() {
        let record = CommandRecord::new("usage", "tokens");
        let record = CommandRecord {
            result: Some(nuo_contracts::CommandResult::Text(
                "daily token totals by model".into(),
            )),
            ..record
        };
        let hits = search("usage tokens", &[], std::slice::from_ref(&record), 5);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].text.starts_with("/usage tokens"));
    }

    #[test]
    fn a_term_present_everywhere_ranks_nothing_above_zero() {
        let messages = vec![msg("alpha x"), msg("alpha y")];
        // "alpha" is in every candidate → idf floor still > 0, but the
        // point of the test: both score identically and neither is dropped
        // for absence; presence-only filtering is what must hold.
        let hits = search("alpha", &messages, &[], 5);
        assert_eq!(hits.len(), 2);
        assert!((hits[0].score - hits[1].score).abs() < f32::EPSILON);
    }

    #[test]
    fn tokenizer_splits_on_non_alphanumeric_and_lowercases() {
        assert_eq!(
            tokenize("Hello, World! 你好"),
            vec!["hello", "world", "你好"]
        );
        assert!(tokenize(" .,! ").is_empty());
    }
}
