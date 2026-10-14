//! Unified task list (todos) — the single source of truth for "what is the
//! agent working on and what is left to do."
//!
//! One list, one sticky panel, one persisted field.
//!
//! Domain types live here. The agent owns the live task-list cells, while the
//! concrete `todo` / `todo_update` implementations and their injected context
//! live in `nuo-agent::tools`. The harness mirrors their state back into the
//! session each turn and replays it on resume.
//!
//! ## Identity vs. display
//! [`TodoItem::id`] is a stable, monotonic identifier used for persistence
//! and reconciliation (so an item keeps its `created_at` when the model
//! re-sends the same content). It is *not* a display number: the model's
//! `todo` tool takes the whole desired list each call (robust against
//! identity-tracking failures), and `todo_update` refers to items by 1-based
//! position or by content substring — never by internal id — so the display
//! can reorder or shrink without breaking references.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Hard cap on the number of items in a [`TodoList`]. Matches the historical
/// `todo` tool contract so existing prompts and behavior stay valid.
pub const MAX_TODOS: usize = 50;

/// Stable, monotonic identifier for a single todo item. Opaque to callers —
/// display and references use position/content, not this value. Serialized
/// transparently so persisted lists stay compact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TodoId(pub u64);

/// Lifecycle of a single todo item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl TodoStatus {
    /// Single-character glyph used by the TUI panel.
    pub fn glyph(self) -> &'static str {
        match self {
            TodoStatus::Completed => "✓",
            TodoStatus::InProgress => "●",
            TodoStatus::Pending => "○",
            TodoStatus::Cancelled => "✕",
        }
    }

    /// Wire string used by the `todo` / `todo_update` tools' `status` field.
    pub fn as_str(self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
            TodoStatus::Cancelled => "cancelled",
        }
    }

    /// Bracket glyph used in plain-text rendering (tool results, `/todos`
    /// text output). Distinct from [`glyph`](Self::glyph) (panel style) to
    /// match the historical `todo` tool output the model is accustomed to.
    pub fn bracket(self) -> &'static str {
        match self {
            TodoStatus::Pending => "[ ]",
            TodoStatus::InProgress => "[~]",
            TodoStatus::Completed => "[x]",
            TodoStatus::Cancelled => "[-]",
        }
    }

    /// Parse a status from its wire string. Accepts the legacy plan-progress
    /// spellings (`done`, `skipped`) so a model trained on the old vocabulary
    /// still works, mapping them onto the unified statuses.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "pending" => Self::Pending,
            "in_progress" => Self::InProgress,
            "completed" | "done" => Self::Completed,
            "cancelled" | "skipped" => Self::Cancelled,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: TodoId,
    pub content: String,
    pub status: TodoStatus,
    /// Unix-epoch seconds when the item was first created. Preserved across
    /// reconciliations (the model re-sending the same content does not reset
    /// the clock).
    #[serde(default)]
    pub created_at: u64,
    /// Unix-epoch seconds of the last status change. Bumped only when the
    /// status actually differs, not on every no-op reconcile.
    #[serde(default)]
    pub updated_at: u64,
}

/// The whole task list. Display order is the [`Vec`] order; identity is
/// [`TodoItem::id`]. Invariants (unique ids, ≤ [`MAX_TODOS`] items, ≤ one
/// `InProgress`) are enforced by the constructors and mutators, never by
/// callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoList {
    #[serde(default)]
    pub items: Vec<TodoItem>,
    /// Next id to hand out. Monotonic for the life of the list so ids are
    /// never reused after a removal, even across save/resume.
    #[serde(default = "default_next_id")]
    pub next_id: u64,
    /// Harness round counter value the last time the list changed. The TUI
    /// compares this against the current round counter to flag a stale panel.
    #[serde(default, alias = "updated_at_turn")]
    pub updated_at_round: u64,
}

fn default_next_id() -> u64 {
    1
}

impl Default for TodoList {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            next_id: 1,
            updated_at_round: 0,
        }
    }
}

impl TodoList {
    /// Maximum capacity of a todo list.
    pub const MAX_CAPACITY: usize = MAX_TODOS;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Number of items in a given status (used for the panel summary
    /// `done_count/total`).
    pub fn count(&self, status: TodoStatus) -> usize {
        self.items.iter().filter(|i| i.status == status).count()
    }

    /// Whether every item is terminal (`Completed` or `Cancelled`) — i.e. no
    /// `Pending` / `InProgress` work remains. True for an empty list too.
    /// Used by the harness to auto-clear the list once everything is wrapped
    /// up, so a finished task list does not linger in the panel forever.
    pub fn is_all_done(&self) -> bool {
        self.items
            .iter()
            .all(|i| matches!(i.status, TodoStatus::Completed | TodoStatus::Cancelled))
    }

    /// Replace the whole list with `desired`, preserving identity for items
    /// whose content is unchanged (first unused match wins). New content gets
    /// a fresh id; dropped items vanish from the live list (their history is
    /// retained in the session event log). Returns `true` if anything
    /// changed.
    ///
    /// `desired` is assumed already validated (non-empty content, ≤
    /// [`MAX_TODOS`], ≤ one `InProgress`) — validation lives in the callers
    /// so the user-facing `/todos` command and the model-facing `todo` tool
    /// can surface violations with their own appropriate error shape.
    pub fn reconcile(&mut self, desired: &[(String, TodoStatus)], now: u64, round: u64) -> bool {
        // Snapshot current items so id allocation (which mutates self) does
        // not fight the immutable lookups used to match identity.
        let current: Vec<TodoItem> = self.items.clone();
        let mut used: HashSet<u64> = HashSet::new();
        let mut next_id = self.next_id;

        let new_items: Vec<TodoItem> = desired
            .iter()
            .map(|(content, status)| {
                if let Some(mut item) = current
                    .iter()
                    .find(|it| it.content == *content && !used.contains(&it.id.0))
                    .map(|m| {
                        used.insert(m.id.0);
                        m.clone()
                    })
                {
                    if item.status != *status {
                        item.status = *status;
                        item.updated_at = now;
                    }
                    item
                } else {
                    let id = TodoId(next_id);
                    next_id += 1;
                    TodoItem {
                        id,
                        content: content.clone(),
                        status: *status,
                        created_at: now,
                        updated_at: now,
                    }
                }
            })
            .collect();

        let changed = new_items != self.items;
        if changed {
            self.items = new_items;
            self.next_id = next_id;
            self.updated_at_round = round;
        }
        changed
    }

    /// Update the status of items matched by `key`. `key` is either a 1-based
    /// display position (`"1"`, `"3"`) or, when not a valid position, a
    /// case-insensitive substring of the content (all matches updated).
    /// Returns the number of items changed. Stamps `updated_at_round` only
    /// when at least one item moved.
    pub fn update(&mut self, key: &str, status: TodoStatus, now: u64, round: u64) -> usize {
        let mut changed = 0;
        let trimmed = key.trim();
        // Position match takes priority when it is a valid 1-based index.
        let pos = trimmed
            .trim_start_matches('#')
            .parse::<usize>()
            .ok()
            .filter(|p| *p >= 1 && *p <= self.items.len());
        if let Some(p) = pos {
            let item = &mut self.items[p - 1];
            if item.status != status {
                item.status = status;
                item.updated_at = now;
                changed += 1;
            }
            if changed > 0 {
                self.updated_at_round = round;
            }
            return changed;
        }
        // Fall back to case-insensitive content substring.
        let needle = trimmed.to_lowercase();
        if !needle.is_empty() {
            for item in &mut self.items {
                if item.content.to_lowercase().contains(&needle) && item.status != status {
                    item.status = status;
                    item.updated_at = now;
                    changed += 1;
                }
            }
        }
        if changed > 0 {
            self.updated_at_round = round;
        }
        changed
    }

    /// Plain-text rendering with 1-based positions and bracket glyphs, the
    /// format the model is trained on for `todo` tool results.
    pub fn render(&self) -> String {
        if self.items.is_empty() {
            return "(empty)".to_string();
        }
        self.items
            .iter()
            .enumerate()
            .map(|(idx, item)| format!("{}. {} {}", idx + 1, item.status.bracket(), item.content))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Current wall-clock time in Unix-epoch seconds. Used to stamp `created_at` /
/// `updated_at` when allocating todo items.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desired(items: &[(&str, TodoStatus)]) -> Vec<(String, TodoStatus)> {
        items.iter().map(|(c, s)| (c.to_string(), *s)).collect()
    }

    #[test]
    fn reconcile_preserves_identity_for_unchanged_content() {
        let mut list = TodoList::new();
        list.reconcile(&desired(&[("design", TodoStatus::Pending)]), 100, 1);
        let first_id = list.items[0].id;

        // Same content, new status: id and created_at preserved, status/updated move.
        list.reconcile(&desired(&[("design", TodoStatus::Completed)]), 200, 2);
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].id, first_id);
        assert_eq!(list.items[0].created_at, 100);
        assert_eq!(list.items[0].status, TodoStatus::Completed);
        assert_eq!(list.items[0].updated_at, 200);
        assert_eq!(list.updated_at_round, 2);
    }

    #[test]
    fn round_stamp_writes_canonical_key_and_reads_legacy_key() {
        let legacy = r#"{"items":[],"next_id":1,"updated_at_turn":7}"#;
        let list: TodoList = serde_json::from_str(legacy).unwrap();
        assert_eq!(list.updated_at_round, 7);

        let serialized = serde_json::to_string(&list).unwrap();
        assert!(serialized.contains("\"updated_at_round\":7"));
        assert!(!serialized.contains("\"updated_at_turn\""));
    }

    #[test]
    fn reconcile_assigns_fresh_ids_to_new_content() {
        let mut list = TodoList::new();
        list.reconcile(&desired(&[("a", TodoStatus::Pending)]), 100, 1);
        list.reconcile(
            &desired(&[("a", TodoStatus::Pending), ("b", TodoStatus::Pending)]),
            100,
            1,
        );
        assert_eq!(list.items.len(), 2);
        assert_ne!(list.items[0].id, list.items[1].id);
    }

    #[test]
    fn reconcile_drops_absent_items() {
        let mut list = TodoList::new();
        list.reconcile(
            &desired(&[("a", TodoStatus::Pending), ("b", TodoStatus::Pending)]),
            100,
            1,
        );
        list.reconcile(&desired(&[("a", TodoStatus::Completed)]), 200, 2);
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].content, "a");
    }

    #[test]
    fn reconcile_is_noop_when_unchanged() {
        let mut list = TodoList::new();
        list.reconcile(&desired(&[("a", TodoStatus::Pending)]), 100, 1);
        assert!(!list.reconcile(&desired(&[("a", TodoStatus::Pending)]), 100, 1));
    }

    #[test]
    fn update_by_position_changes_one_item() {
        let mut list = TodoList::new();
        list.reconcile(
            &desired(&[("a", TodoStatus::Pending), ("b", TodoStatus::Pending)]),
            100,
            1,
        );
        assert_eq!(list.update("1", TodoStatus::InProgress, 200, 3), 1);
        assert_eq!(list.items[0].status, TodoStatus::InProgress);
        assert_eq!(list.items[1].status, TodoStatus::Pending);
        assert_eq!(list.updated_at_round, 3);
    }

    #[test]
    fn update_by_content_substring_matches_case_insensitive() {
        let mut list = TodoList::new();
        list.reconcile(
            &desired(&[
                ("Design UI", TodoStatus::Pending),
                ("design db", TodoStatus::Pending),
            ]),
            100,
            1,
        );
        // Substring "design" matches both.
        assert_eq!(list.update("design", TodoStatus::Completed, 200, 3), 2);
    }

    #[test]
    fn update_position_takes_priority_over_content() {
        let mut list = TodoList::new();
        list.reconcile(
            &desired(&[
                ("1 thing", TodoStatus::Pending),
                ("other", TodoStatus::Pending),
            ]),
            100,
            1,
        );
        // "1" is a valid position → only the first item.
        assert_eq!(list.update("1", TodoStatus::Completed, 200, 3), 1);
    }

    #[test]
    fn status_parse_accepts_legacy_plan_spellings() {
        assert_eq!(TodoStatus::parse("done"), Some(TodoStatus::Completed));
        assert_eq!(TodoStatus::parse("skipped"), Some(TodoStatus::Cancelled));
        assert_eq!(
            TodoStatus::parse("in_progress"),
            Some(TodoStatus::InProgress)
        );
        assert_eq!(TodoStatus::parse("nope"), None);
    }

    #[test]
    fn render_uses_familiar_bracket_format() {
        let mut list = TodoList::new();
        list.reconcile(
            &desired(&[
                ("design", TodoStatus::Completed),
                ("implement", TodoStatus::InProgress),
                ("verify", TodoStatus::Pending),
                ("docs", TodoStatus::Cancelled),
            ]),
            100,
            1,
        );
        let out = list.render();
        assert!(out.contains("1. [x] design"));
        assert!(out.contains("2. [~] implement"));
        assert!(out.contains("3. [ ] verify"));
        assert!(out.contains("4. [-] docs"));
    }

    #[test]
    fn is_all_done_detects_terminal_list() {
        let mut list = TodoList::new();
        // Empty list is trivially "all done".
        assert!(list.is_all_done());

        list.reconcile(
            &desired(&[
                ("design", TodoStatus::Completed),
                ("docs", TodoStatus::Cancelled),
            ]),
            100,
            1,
        );
        assert!(list.is_all_done());

        // Any non-terminal item means not all done.
        list.reconcile(
            &desired(&[
                ("design", TodoStatus::Completed),
                ("implement", TodoStatus::InProgress),
            ]),
            100,
            1,
        );
        assert!(!list.is_all_done());

        list.reconcile(&desired(&[("verify", TodoStatus::Pending)]), 100, 1);
        assert!(!list.is_all_done());
    }
}
