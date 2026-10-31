use crate::message::{Message, Role};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Unique identifier for a node in the thread DAG tree.
pub type ThreadEntryId = String;

/// The content and semantics of a single node in the thread DAG.
// `Message` is by far the dominant payload of the tree and both variants are
// short-lived around serialization; keeping it inline avoids a heap hop on
// every append and keeps the on-disk serde shape flat.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThreadEntryKind {
    /// A standard dialogue message (User, Assistant, Tool, System).
    Message { message: Message },
    /// A compaction summary replacing older turns up to `first_kept_entry_id`.
    Compaction {
        summary: String,
        first_kept_entry_id: String,
        tokens_before: usize,
        #[serde(default)]
        read_files: Vec<String>,
        #[serde(default)]
        modified_files: Vec<String>,
    },
    /// A structured exploration summary injected when transitioning from an abandoned branch.
    BranchSummary {
        summary: String,
        from_id: String,
        #[serde(default)]
        read_files: Vec<String>,
        #[serde(default)]
        modified_files: Vec<String>,
    },
    /// A user-defined custom metadata or event node.
    Custom {
        custom_type: String,
        content: String,
        #[serde(default)]
        display: Option<String>,
    },
}

/// A node in the thread DAG tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadEntry {
    pub id: ThreadEntryId,
    pub parent_id: Option<ThreadEntryId>,
    pub timestamp: u64,
    #[serde(flatten)]
    pub kind: ThreadEntryKind,
}

/// Content payload of a compaction entry node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionPayload {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: usize,
    #[serde(default)]
    pub read_files: Vec<String>,
    #[serde(default)]
    pub modified_files: Vec<String>,
}

impl ThreadEntry {
    pub fn new_message(
        id: impl Into<String>,
        parent_id: Option<String>,
        timestamp: u64,
        message: Message,
    ) -> Self {
        Self {
            id: id.into(),
            parent_id,
            timestamp,
            kind: ThreadEntryKind::Message { message },
        }
    }

    pub fn new_compaction(
        id: impl Into<String>,
        parent_id: Option<String>,
        timestamp: u64,
        payload: CompactionPayload,
    ) -> Self {
        Self {
            id: id.into(),
            parent_id,
            timestamp,
            kind: ThreadEntryKind::Compaction {
                summary: payload.summary,
                first_kept_entry_id: payload.first_kept_entry_id,
                tokens_before: payload.tokens_before,
                read_files: payload.read_files,
                modified_files: payload.modified_files,
            },
        }
    }

    pub fn new_branch_summary(
        id: impl Into<String>,
        parent_id: Option<String>,
        timestamp: u64,
        summary: String,
        from_id: String,
        read_files: Vec<String>,
        modified_files: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            parent_id,
            timestamp,
            kind: ThreadEntryKind::BranchSummary {
                summary,
                from_id,
                read_files,
                modified_files,
            },
        }
    }

    /// Extract context-visible message from this entry.
    pub fn to_context_message(&self) -> Option<Message> {
        match &self.kind {
            ThreadEntryKind::Message { message } => Some(message.clone()),
            ThreadEntryKind::Compaction { summary, .. } => Some(Message::new(
                Role::System,
                format!("[Context Compaction Summary]\n{}", summary),
            )),
            ThreadEntryKind::BranchSummary { summary, .. } => Some(Message::new(
                Role::System,
                format!("[Branch Exploration Summary]\n{}", summary),
            )),
            ThreadEntryKind::Custom { content, .. } => {
                Some(Message::new(Role::System, content.clone()))
            }
        }
    }
}

/// A tree-structured DAG representing a branching conversation thread.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadTree {
    /// All entries in the tree keyed by ID.
    pub entries: HashMap<ThreadEntryId, ThreadEntry>,
    /// Root node of the thread tree (first prompt).
    pub root_id: Option<ThreadEntryId>,
    /// The currently active leaf ID in the tree.
    pub active_leaf_id: Option<ThreadEntryId>,
    /// Named branches mapping branch name -> leaf entry ID.
    #[serde(default)]
    pub named_branches: HashMap<String, ThreadEntryId>,
}

impl ThreadTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a new message entry to the tree. If `parent_id` is None, uses `active_leaf_id`.
    /// Updates `active_leaf_id` to the newly inserted entry.
    pub fn append_message(
        &mut self,
        message: Message,
        timestamp: u64,
        custom_parent_id: Option<String>,
    ) -> ThreadEntryId {
        let parent = custom_parent_id.or_else(|| self.active_leaf_id.clone());
        let id = uuid::Uuid::new_v4().to_string();
        let entry = ThreadEntry::new_message(id.clone(), parent, timestamp, message);
        self.insert_entry(entry);
        id
    }

    /// Insert an entry into the tree and update active leaf.
    pub fn insert_entry(&mut self, entry: ThreadEntry) {
        let id = entry.id.clone();
        if self.root_id.is_none() && entry.parent_id.is_none() {
            self.root_id = Some(id.clone());
        }
        self.active_leaf_id = Some(id.clone());
        self.entries.insert(id, entry);
    }

    /// Get an entry by ID.
    pub fn get_entry(&self, id: &str) -> Option<&ThreadEntry> {
        self.entries.get(id)
    }

    /// Return the active leaf ID if set.
    pub fn active_leaf(&self) -> Option<ThreadEntryId> {
        self.active_leaf_id.clone()
    }

    /// Get all entries forming the linear branch from the root down to `leaf_id`.
    /// Returned entries are in chronological root-to-leaf order.
    pub fn get_branch(&self, leaf_id: &str) -> Vec<ThreadEntry> {
        let mut path = Vec::new();
        let mut curr = Some(leaf_id.to_string());
        let mut visited = HashSet::new();

        while let Some(id) = curr {
            if !visited.insert(id.clone()) {
                break; // guard against potential cyclic malformed data
            }
            if let Some(entry) = self.entries.get(&id) {
                path.push(entry.clone());
                curr = entry.parent_id.clone();
            } else {
                break;
            }
        }

        path.reverse();
        path
    }

    /// Extract the active messages for LLM context corresponding to `leaf_id`.
    pub fn get_context_messages(&self, leaf_id: &str) -> Vec<Message> {
        let branch = self.get_branch(leaf_id);
        let mut messages = Vec::new();

        // Check if there is a compaction entry in the branch
        let mut start_idx = 0;
        for entry in &branch {
            if let ThreadEntryKind::Compaction {
                ref first_kept_entry_id,
                ..
            } = entry.kind
            {
                // Find the first kept entry index in the branch
                if let Some(kept_pos) = branch.iter().position(|e| e.id == *first_kept_entry_id) {
                    start_idx = kept_pos;
                }
                if let Some(summary_msg) = entry.to_context_message() {
                    messages.push(summary_msg);
                }
                break;
            }
        }

        for entry in &branch[start_idx..] {
            if let ThreadEntryKind::Compaction { .. } = entry.kind {
                continue; // already handled
            }
            if let Some(msg) = entry.to_context_message() {
                messages.push(msg);
            }
        }

        messages
    }

    /// Find the Lowest Common Ancestor (LCA) between two nodes in the tree.
    pub fn lowest_common_ancestor(&self, a_id: &str, b_id: &str) -> Option<String> {
        let a_ancestors: HashSet<String> =
            self.get_branch(a_id).into_iter().map(|e| e.id).collect();
        let b_branch = self.get_branch(b_id);

        // Iterate backwards from b's leaf to root to find the deepest node in a's path
        for entry in b_branch.iter().rev() {
            if a_ancestors.contains(&entry.id) {
                return Some(entry.id.clone());
            }
        }

        None
    }

    /// Collect entries that were added in `old_leaf`'s branch after diverging from `target_id`.
    /// Returns (abandoned_entries, common_ancestor_id).
    pub fn collect_abandoned_entries(
        &self,
        old_leaf: &str,
        target_id: &str,
    ) -> (Vec<ThreadEntry>, Option<String>) {
        let lca = self.lowest_common_ancestor(old_leaf, target_id);
        let mut abandoned = Vec::new();
        let mut curr = Some(old_leaf.to_string());

        while let Some(id) = curr {
            if Some(&id) == lca.as_ref() {
                break;
            }
            if let Some(entry) = self.entries.get(&id) {
                abandoned.push(entry.clone());
                curr = entry.parent_id.clone();
            } else {
                break;
            }
        }

        abandoned.reverse();
        (abandoned, lca)
    }

    /// Find all leaf entry IDs in the tree (nodes that have no children).
    pub fn leaves(&self) -> Vec<ThreadEntryId> {
        let parents: HashSet<&str> = self
            .entries
            .values()
            .filter_map(|e| e.parent_id.as_deref())
            .collect();

        self.entries
            .keys()
            .filter(|id| !parents.contains(id.as_str()))
            .cloned()
            .collect()
    }

    /// Return all immediate child entries of a given node.
    pub fn children_of(&self, parent_id: &str) -> Vec<&ThreadEntry> {
        self.entries
            .values()
            .filter(|e| e.parent_id.as_deref() == Some(parent_id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_append_and_branch_linear() {
        let mut tree = ThreadTree::new();
        let id1 = tree.append_message(Message::new(Role::User, "hello"), 100, None);
        let id2 = tree.append_message(Message::new(Role::Assistant, "hi"), 101, None);
        let id3 = tree.append_message(Message::new(Role::User, "what is rust?"), 102, None);

        let branch = tree.get_branch(&id3);
        assert_eq!(branch.len(), 3);
        assert_eq!(branch[0].id, id1);
        assert_eq!(branch[1].id, id2);
        assert_eq!(branch[2].id, id3);
        assert_eq!(tree.active_leaf_id, Some(id3));
    }

    #[test]
    fn tree_fork_and_lca() {
        let mut tree = ThreadTree::new();
        let r = tree.append_message(Message::new(Role::User, "prompt 1"), 100, None);
        let a1 = tree.append_message(
            Message::new(Role::Assistant, "plan A"),
            101,
            Some(r.clone()),
        );
        let a2 = tree.append_message(
            Message::new(Role::User, "implement A"),
            102,
            Some(a1.clone()),
        );

        // Fork from r to branch B
        let b1 = tree.append_message(
            Message::new(Role::Assistant, "plan B"),
            103,
            Some(r.clone()),
        );
        let b2 = tree.append_message(
            Message::new(Role::User, "implement B"),
            104,
            Some(b1.clone()),
        );

        assert_eq!(tree.lowest_common_ancestor(&a2, &b2), Some(r.clone()));

        let (abandoned, lca) = tree.collect_abandoned_entries(&a2, &b2);
        assert_eq!(lca, Some(r));
        assert_eq!(abandoned.len(), 2);
        assert_eq!(abandoned[0].id, a1);
        assert_eq!(abandoned[1].id, a2);
    }
}
