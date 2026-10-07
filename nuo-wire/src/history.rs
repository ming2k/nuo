//! Input history — the cross-session record of prompts the user has sent.
//!
//! Each entry remembers not just its text but the **origin** that produced it:
//! the [`HistoryEntry::session_id`] it was typed into and the
//! [`HistoryEntry::workspace`] (project root) that session was running in.
//! This is what lets the Ctrl+R surface search the *whole* history
//! (independent of session or workspace) while the inline ↑/↓ recall walks
//! only the current session's entries.
//!
//! Entries are persisted to a single global file (`history.json`) and merged
//! across concurrent processes (ADR-0018's union-on-write strategy), so the
//! history is the union of every session the user has ever run.

use serde::{Deserialize, Serialize};

/// A single recorded user input, tagged with the session and workspace it
/// came from plus the wall-clock time it was sent.
///
/// The on-disk format is `Vec<HistoryEntry>`. `session_id` and `workspace`
/// are `Option` so that:
/// - entries recorded before origin-tracking existed (or by a process that
///   could not learn its session id) load back as "unknown origin" rather
///   than failing to parse, and
/// - the cross-process union merge can keep first-seen entries verbatim
///   without having to fabricate an origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// The user's literal prompt text, sent verbatim to the agent.
    pub text: String,
    /// The id of the session this entry was typed into, or `None` when the
    /// recording process could not attribute it (e.g. a legacy entry). The
    /// inline ↑/↓ recall filters the global history by this field against
    /// the live session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The workspace (project root) the session was running in when this
    /// entry was sent, or `None` when unknown. Surfaced in the history
    /// surface's selected-row origin line so the user can tell a prompt
    /// typed in another project apart from one typed here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// When the entry was sent, as milliseconds since the UNIX epoch. Drives
    /// the newest-first ordering of the history surface (most recent on top)
    /// and the selected-row time stamp. Entries without a known time load as
    /// `0` and sort oldest.
    #[serde(default)]
    pub created_at_ms: u64,
}

impl HistoryEntry {
    /// Cap on the number of history entries kept on disk.
    pub const CAP: usize = HISTORY_CAP;

    /// Build a new entry with the given origin, stamped "now".
    pub fn new(
        text: String,
        session_id: Option<String>,
        workspace: Option<String>,
        created_at_ms: u64,
    ) -> Self {
        Self {
            text,
            session_id,
            workspace,
            created_at_ms,
        }
    }

    /// Convenience accessor for the text as `&str`.
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl AsRef<str> for HistoryEntry {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

/// Cap on the number of history entries kept on disk. Matches the previous
/// `Vec<String>` constant so the on-disk footprint stays bounded.
pub const HISTORY_CAP: usize = 100_000;

/// Merge `incoming` into `existing`, taking the union and keeping the
/// **newest** `created_at_ms` for each survivor, then sorting newest-first and
/// capping to [`HISTORY_CAP`].
///
/// The merge identity is chosen by `dedup`:
/// - `dedup == true` keys on the **prompt text alone**, so the same prompt
///   sent twice — even in different sessions or workspaces — collapses into a
///   single entry. This is the default (`[input_history] dedup = true`):
///   users usually want the picker to show one row per prompt, not one per
///   repetition.
/// - `dedup == false` keys on `(text, session_id)`, so the same words typed
///   in two sessions stay as two entries, each with its own origin and
///   timestamp.
///
/// This is the cross-process merge used by `Config::save_history`: every
/// live process appends only its own new entries, and the write takes a file
/// lock, reloads the on-disk list, and unions the two so a concurrent
/// process's recent commands survive (ADR-0018).
pub fn merge_history(
    existing: &[HistoryEntry],
    incoming: &[HistoryEntry],
    dedup: bool,
) -> Vec<HistoryEntry> {
    // Preserve first-seen order from `existing`, then append `incoming`
    // entries whose identity is not already present, updating the timestamp
    // (and, under text-only identity, the origin) to the newer of the two
    // when an identity collides. O(N) index-backed lookup keeps this fast
    // even at HISTORY_CAP (100,000 entries).
    let mut merged: Vec<HistoryEntry> = existing.to_vec();

    if dedup {
        let mut index: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::with_capacity(existing.len() + incoming.len());
        for (i, entry) in existing.iter().enumerate() {
            index.entry(entry.text.as_str()).or_insert(i);
        }
        for entry in incoming {
            if let Some(&idx) = index.get(entry.text.as_str()) {
                let slot = &mut merged[idx];
                if entry.created_at_ms > slot.created_at_ms {
                    slot.created_at_ms = entry.created_at_ms;
                    // Under text-only identity the newer entry's origin is the one
                    // the recall surfaces under — refresh it so ↑/↓ in the session
                    // that last sent the prompt still finds it. Only overwrite with
                    // a known origin; a legacy `None` never wipes a real one.
                    if entry.session_id.is_some() {
                        slot.session_id = entry.session_id.clone();
                    }
                    if entry.workspace.is_some() {
                        slot.workspace = entry.workspace.clone();
                    }
                }
            } else {
                let new_idx = merged.len();
                index.insert(entry.text.as_str(), new_idx);
                merged.push(entry.clone());
            }
        }
    } else {
        let mut index: std::collections::HashMap<(&str, Option<&str>), usize> =
            std::collections::HashMap::with_capacity(existing.len() + incoming.len());
        for (i, entry) in existing.iter().enumerate() {
            index
                .entry((entry.text.as_str(), entry.session_id.as_deref()))
                .or_insert(i);
        }
        for entry in incoming {
            let key = (entry.text.as_str(), entry.session_id.as_deref());
            if let Some(&idx) = index.get(&key) {
                let slot = &mut merged[idx];
                if entry.created_at_ms > slot.created_at_ms {
                    slot.created_at_ms = entry.created_at_ms;
                }
            } else {
                let new_idx = merged.len();
                index.insert(key, new_idx);
                merged.push(entry.clone());
            }
        }
    }

    // Newest-first: stable sort keeps first-seen (disk) order among ties.
    merged.sort_by_key(|e| std::cmp::Reverse(e.created_at_ms));
    if merged.len() > HISTORY_CAP {
        let drop = merged.len() - HISTORY_CAP;
        merged.truncate(merged.len() - drop);
    }
    merged
}

/// One full-text search hit across persisted session transcripts (ADR-0208).
///
/// Produced by the server's FTS5 query over `fts_entries` — the wire-facing
/// mirror of the persistence layer's raw result, enriched with the session
/// title so a hit is presentable without a second round-trip. The snippet
/// carries FTS `<b>`/`</b>` highlight markers; renderers that cannot show
/// emphasis strip them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistorySearchHit {
    /// The transcript entry that matched (stable entry identity).
    pub entry_id: String,
    /// The session the entry belongs to — the dashboard's jump/attach anchor.
    pub session_id: String,
    /// Project root of the owning session (addressing metadata, ADR-0093).
    pub workspace: String,
    /// Stored AI/manual title of the owning session, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    /// Role of the matched entry (`user` / `assistant` / `system` / `tool`).
    pub role: String,
    /// Highlighted content excerpt around the match (`<b>`/`</b>` markers).
    pub snippet: String,
    /// BM25 relevance score (lower is better, FTS5 convention). Purely
    /// ordering metadata — not a stable quantity across builds.
    pub score: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str, session: &str, ws: &str, ts: u64) -> HistoryEntry {
        HistoryEntry::new(
            text.to_string(),
            Some(session.to_string()),
            Some(ws.to_string()),
            ts,
        )
    }

    #[test]
    fn merge_unions_by_text_and_session_keeping_newest_time() {
        let existing = vec![
            entry("hello", "s1", "ws-a", 100),
            entry("deploy", "s1", "ws-a", 200),
        ];
        let incoming = vec![
            // same text+session as existing[0] but newer time → updates ts.
            entry("hello", "s1", "ws-b", 300),
            // brand new → appended.
            entry("rollback", "s2", "ws-c", 250),
        ];

        let merged = merge_history(&existing, &incoming, false);

        // Newest-first: hello(300), rollback(250), deploy(200).
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].text, "hello");
        assert_eq!(merged[0].created_at_ms, 300);
        assert_eq!(merged[1].text, "rollback");
        assert_eq!(merged[2].text, "deploy");

        // The colliding entry's origin is not overwritten when it already
        // had one (ws-a stays, ws-b ignored) in the non-dedup identity.
        assert_eq!(merged[0].workspace.as_deref(), Some("ws-a"));
    }

    #[test]
    fn same_text_in_two_sessions_is_two_entries_without_dedup() {
        let existing = vec![entry("hello", "s1", "ws-a", 100)];
        let incoming = vec![entry("hello", "s2", "ws-b", 200)];

        let merged = merge_history(&existing, &incoming, false);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].session_id.as_deref(), Some("s2")); // newer first
        assert_eq!(merged[1].session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn dedup_collapses_same_text_across_sessions() {
        // With dedup on, identity is the prompt text alone: the same words in
        // two sessions collapse to one entry, keeping the newest timestamp and
        // adopting the newest known origin.
        let existing = vec![entry("hello", "s1", "ws-a", 100)];
        let incoming = vec![entry("hello", "s2", "ws-b", 200)];

        let merged = merge_history(&existing, &incoming, true);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "hello");
        assert_eq!(merged[0].created_at_ms, 200);
        assert_eq!(merged[0].session_id.as_deref(), Some("s2"));
        assert_eq!(merged[0].workspace.as_deref(), Some("ws-b"));
    }

    #[test]
    fn dedup_reorders_the_collapsed_entry_newest_first() {
        let existing = vec![
            entry("hello", "s1", "ws-a", 100),
            entry("deploy", "s1", "ws-a", 90),
        ];
        let incoming = vec![entry("deploy", "s1", "ws-a", 300)];

        let merged = merge_history(&existing, &incoming, true);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "deploy"); // bumped to the top by its new ts
        assert_eq!(merged[0].created_at_ms, 300);
        assert_eq!(merged[1].text, "hello");
    }

    #[test]
    fn dedup_keeps_existing_origin_when_incoming_is_unknown() {
        // A legacy `None`-origin incoming must not wipe a known origin, even
        // though it is newer — unknown provenance is never more informative.
        let existing = vec![HistoryEntry::new(
            "hello".to_string(),
            Some("s1".to_string()),
            Some("ws-a".to_string()),
            100,
        )];
        let incoming = vec![HistoryEntry::new("hello".to_string(), None, None, 300)];

        let merged = merge_history(&existing, &incoming, true);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].created_at_ms, 300);
        assert_eq!(merged[0].session_id.as_deref(), Some("s1"));
        assert_eq!(merged[0].workspace.as_deref(), Some("ws-a"));
    }

    #[test]
    fn merge_treats_unknown_origin_as_distinct_identity() {
        // Identity is `(text, session_id)` when dedup is off. A legacy entry
        // with no known session id does NOT collide with a real entry that
        // names a session — the two are genuinely different provenances, so
        // both survive the union. This matches the migration stance: the
        // global history file is reset on upgrade, so legacy `None`-origin
        // entries never coexist with attributed ones in practice.
        let existing = vec![HistoryEntry::new("hello".to_string(), None, None, 100)];
        let incoming = vec![entry("hello", "s1", "ws-a", 100)];

        let merged = merge_history(&existing, &incoming, false);
        assert_eq!(merged.len(), 2);
        assert!(
            merged
                .iter()
                .any(|e| e.session_id == Some("s1".to_string()))
        );
        assert!(merged.iter().any(|e| e.session_id.is_none()));
    }

    #[test]
    fn merge_caps_to_history_cap() {
        let big: Vec<HistoryEntry> = (0..(HISTORY_CAP + 50))
            .map(|i| entry(&format!("e{i}"), "s", "ws", i as u64))
            .collect();
        let merged = merge_history(&[], &big, true);
        assert_eq!(merged.len(), HISTORY_CAP);
        // The newest HISTORY_CAP survive (oldest 50 dropped).
        assert_eq!(
            merged.first().unwrap().text,
            format!("e{}", HISTORY_CAP + 49)
        );
        assert_eq!(merged.last().unwrap().text, format!("e{}", 50));
    }

    #[test]
    fn legacy_entry_without_origin_round_trips() {
        // An entry recorded before origin tracking must still parse and
        // survive a merge unchanged.
        let json = r#"[{"text":"old prompt","created_at_ms":42}]"#;
        let parsed: Vec<HistoryEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].text, "old prompt");
        assert_eq!(parsed[0].session_id, None);
        assert_eq!(parsed[0].workspace, None);
        assert_eq!(parsed[0].created_at_ms, 42);

        // Re-serialize and it omits the absent origin fields.
        let reser = serde_json::to_string(&parsed).unwrap();
        assert!(!reser.contains("session_id"));
        assert!(!reser.contains("workspace"));
    }
}
