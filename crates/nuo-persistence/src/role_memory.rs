//! Role-scoped dialogue memory store with human-like memory characteristics (ADR cognitive RAG).
//!
//! Features:
//! - Strictly role-isolated dialogue recording (user prompt <-> role response turns only).
//! - Dual-store architecture: working memory (recent high-activation context) and long-term memory.
//! - Ebbinghaus forgetting curve decay: retention R = exp( - delta_t / (strength * tau) ).
//! - Spaced repetition / retrieval practice: accessing memories strengthens retention and slows decay.
//! - Self-pruning and bounded capacity to prevent unbounded growth for end-user maintainability.
//! - Embedded SQLite + FTS5 full-text indexing with CJK / substring matching fallback.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default half-life stability factor in days (7 days).
pub const DEFAULT_BASE_TAU_DAYS: f64 = 7.0;

/// Immediate working memory window in seconds (12 hours = 43,200s).
/// Interactions within this window maintain full retention without decay.
pub const WORKING_MEMORY_WINDOW_SECS: i64 = 43_200;

/// Forgetting threshold: below this retention score, un-reinforced memories
/// are eligible for pruning when database capacity is constrained.
pub const FORGETTING_THRESHOLD: f64 = 0.05;

/// Default capacity cap per role to prevent unbounded memory growth.
pub const DEFAULT_MAX_MEMORIES_PER_ROLE: usize = 1000;

/// One persisted dialogue memory record between a user and a specific role.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoleMemoryEntry {
    pub id: String,
    pub role: String,
    pub session_id: Option<String>,
    pub user_prompt: String,
    pub role_response: String,
    pub created_at_s: i64,
    pub last_accessed_at_s: i64,
    pub access_count: u32,
    pub strength: f64,
    pub importance: f64,
}

/// A recalled memory returned to the model/agent with cognitive metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecalledMemory {
    pub id: String,
    pub role: String,
    pub session_id: Option<String>,
    pub user_prompt: String,
    pub role_response: String,
    pub created_at_s: i64,
    pub last_accessed_at_s: i64,
    pub access_count: u32,
    pub strength: f64,
    pub retention: f64,
    pub score: f64,
    pub memory_type: String,
    pub recency_label: String,
}

fn current_time_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Calculate Ebbinghaus memory retention score in [0.0, 1.0].
///
/// Formula:
/// If within working memory window (<= 12h), retention is 1.0.
/// Beyond 12h, memory decays continuously without a cliff drop:
/// R = exp( - (elapsed_s - working_window_s) / (strength * tau_days * 86400) )
pub fn calculate_retention(elapsed_s: i64, strength: f64, tau_days: f64) -> f64 {
    let decay_elapsed_s = elapsed_s.saturating_sub(WORKING_MEMORY_WINDOW_SECS);
    if decay_elapsed_s == 0 {
        return 1.0;
    }
    let decay_days = (decay_elapsed_s as f64) / 86400.0;
    let s = strength.max(0.1);
    let tau = tau_days.max(0.1);
    (-decay_days / (s * tau)).exp().clamp(0.0, 1.0)
}

/// Format friendly elapsed time description.
pub fn format_recency(elapsed_s: i64) -> String {
    if elapsed_s < 60 {
        "just now".to_string()
    } else if elapsed_s < 3600 {
        format!("{} minutes ago", (elapsed_s / 60).max(1))
    } else if elapsed_s < 86400 {
        let hours = elapsed_s / 3600;
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{hours} hours ago")
        }
    } else if elapsed_s < 86400 * 30 {
        let days = elapsed_s / 86400;
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{days} days ago")
        }
    } else if elapsed_s < 86400 * 365 {
        let months = elapsed_s / (86400 * 30);
        if months == 1 {
            "1 month ago".to_string()
        } else {
            format!("{months} months ago")
        }
    } else {
        let years = elapsed_s / (86400 * 365);
        if years == 1 {
            "1 year ago".to_string()
        } else {
            format!("{years} years ago")
        }
    }
}

/// Thread-safe SQLite store for role-isolated dialogue memories.
#[derive(Clone)]
pub struct RoleMemoryStore {
    conn: Arc<Mutex<Connection>>,
}

impl RoleMemoryStore {
    /// Open or create the role dialogue memory database on disk.
    pub fn open(db_path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(db_path)?;
        Self::init_connection(conn)
    }

    /// Open an in-memory instance for testing and sandboxed environments.
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init_connection(conn)
    }

    /// Open the standard user-global role memory database.
    pub fn open_default() -> rusqlite::Result<Self> {
        let path = crate::paths::get().role_memory_db();
        Self::open(&path)
    }

    fn init_connection(conn: Connection) -> rusqlite::Result<Self> {
        conn.pragma_update(None, "busy_timeout", 5000)?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS role_memories (
                id TEXT PRIMARY KEY,
                role TEXT NOT NULL,
                session_id TEXT,
                user_prompt TEXT NOT NULL,
                role_response TEXT NOT NULL,
                created_at_s INTEGER NOT NULL,
                last_accessed_at_s INTEGER NOT NULL,
                access_count INTEGER NOT NULL DEFAULT 1,
                strength REAL NOT NULL DEFAULT 1.0,
                importance REAL NOT NULL DEFAULT 1.0
            );

            CREATE INDEX IF NOT EXISTS idx_role_memories_role_created
                ON role_memories(role, created_at_s DESC);

            CREATE INDEX IF NOT EXISTS idx_role_memories_role_access
                ON role_memories(role, last_accessed_at_s DESC);

            CREATE VIRTUAL TABLE IF NOT EXISTS fts_role_memories USING fts5(
                memory_id UNINDEXED,
                role UNINDEXED,
                content,
                tokenize = 'porter unicode61'
            );

            CREATE TRIGGER IF NOT EXISTS trg_role_memories_ai AFTER INSERT ON role_memories BEGIN
                INSERT INTO fts_role_memories(memory_id, role, content)
                VALUES (new.id, new.role, new.user_prompt || ' ' || new.role_response);
            END;

            CREATE TRIGGER IF NOT EXISTS trg_role_memories_au AFTER UPDATE OF user_prompt, role_response ON role_memories BEGIN
                DELETE FROM fts_role_memories WHERE memory_id = old.id;
                INSERT INTO fts_role_memories(memory_id, role, content)
                VALUES (new.id, new.role, new.user_prompt || ' ' || new.role_response);
            END;

            CREATE TRIGGER IF NOT EXISTS trg_role_memories_ad AFTER DELETE ON role_memories BEGIN
                DELETE FROM fts_role_memories WHERE memory_id = old.id;
            END;
            "#,
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Record a dialogue turn between the user and a specific role.
    ///
    /// Only non-empty dialogues are saved. Deduplication prevents rapid duplicate saves.
    pub fn record_dialogue(
        &self,
        role: &str,
        session_id: Option<&str>,
        user_prompt: &str,
        role_response: &str,
    ) -> rusqlite::Result<Option<RoleMemoryEntry>> {
        let user_prompt = user_prompt.trim();
        let role_response = role_response.trim();
        if user_prompt.is_empty() || role_response.is_empty() {
            return Ok(None);
        }

        let role = role.trim().to_lowercase();
        let now_s = current_time_s();

        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        // Deduplication: if the exact same interaction happened in the last 60 seconds, update access time instead
        let duplicate_id: Option<String> = conn
            .query_row(
                "SELECT id FROM role_memories WHERE role = ?1 AND user_prompt = ?2 AND role_response = ?3 AND (?4 - created_at_s) < 60",
                params![role, user_prompt, role_response, now_s],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(id) = duplicate_id {
            conn.execute(
                "UPDATE role_memories SET last_accessed_at_s = ?1 WHERE id = ?2",
                params![now_s, id],
            )?;
            return Ok(None);
        }

        let lower = user_prompt.to_lowercase();
        let importance = if lower.contains("remember")
            || lower.contains("core")
            || lower.contains("principle")
            || lower.contains("fundamental")
            || user_prompt.contains("记住")
            || user_prompt.contains("核心")
            || user_prompt.contains("原则")
            || user_prompt.contains("重要")
        {
            1.5
        } else {
            1.0
        };

        let entry_id = uuid::Uuid::new_v4().to_string();
        let entry = RoleMemoryEntry {
            id: entry_id.clone(),
            role: role.clone(),
            session_id: session_id.map(str::to_string),
            user_prompt: user_prompt.to_string(),
            role_response: role_response.to_string(),
            created_at_s: now_s,
            last_accessed_at_s: now_s,
            access_count: 1,
            strength: 1.0,
            importance,
        };

        conn.execute(
            "INSERT INTO role_memories (id, role, session_id, user_prompt, role_response, created_at_s, last_accessed_at_s, access_count, strength, importance)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                entry.id,
                entry.role,
                entry.session_id,
                entry.user_prompt,
                entry.role_response,
                entry.created_at_s,
                entry.last_accessed_at_s,
                entry.access_count,
                entry.strength,
                entry.importance,
            ],
        )?;

        drop(conn);

        // Bounded capacity check
        let _ = self.prune_if_needed(&role, DEFAULT_MAX_MEMORIES_PER_ROLE);

        Ok(Some(entry))
    }

    /// Recall relevant past dialogues for a specific role matching `query`.
    ///
    /// Evaluates candidate entries against the Ebbinghaus forgetting curve,
    /// combines relevance and retention, and reinforces retrieved memories (spaced repetition).
    pub fn recall(
        &self,
        role: &str,
        query: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<RecalledMemory>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let role = role.trim().to_lowercase();
        let now_s = current_time_s();
        let limit = limit.clamp(1, 20);

        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        // Step 1: Candidate retrieval via FTS5
        let mut candidate_ids = std::collections::HashSet::new();
        let mut fts_scores = std::collections::HashMap::new();

        let fts_query = sanitize_fts_query(query);
        if !fts_query.is_empty() {
            let mut stmt = conn.prepare(
                "SELECT memory_id, bm25(fts_role_memories) AS score
                 FROM fts_role_memories
                 WHERE fts_role_memories MATCH ?1 AND role = ?2
                 ORDER BY score ASC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![fts_query, role, (limit * 3) as i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
            })?;
            for row in rows.flatten() {
                candidate_ids.insert(row.0.clone());
                // In SQLite bm25, lower is more relevant (negative or small positive depending on version).
                // Convert to a positive relevance weight.
                let raw_bm25 = row.1;
                let rel = if raw_bm25 <= 0.0 {
                    1.0 + raw_bm25.abs()
                } else {
                    1.0 / (1.0 + raw_bm25)
                };
                fts_scores.insert(row.0, rel);
            }
        }

        // Step 2: Fallback retrieval for CJK text (where unicode61 doesn't segment without spaces)
        let has_cjk = query
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
        let cjk_chunks: Vec<String> = if has_cjk {
            query
                .split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
                .filter(|t| {
                    !t.is_empty() && t.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
                })
                .map(|t| t.to_string())
                .collect()
        } else {
            Vec::new()
        };

        for chunk in &cjk_chunks {
            let pattern = format!("%{chunk}%");
            let mut stmt = conn.prepare(
                "SELECT id FROM role_memories
                 WHERE role = ?1 AND (user_prompt LIKE ?2 OR role_response LIKE ?2)
                 ORDER BY last_accessed_at_s DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![role, pattern, limit as i64], |row| {
                row.get::<_, String>(0)
            })?;
            for id in rows.flatten() {
                candidate_ids.insert(id);
            }
        }

        if candidate_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Tokenize query words for term overlap scoring
        let terms: Vec<String> = query
            .split_whitespace()
            .filter_map(|t| {
                let s: String = t
                    .chars()
                    .filter(|c| c.is_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(c))
                    .collect();
                if s.is_empty() {
                    None
                } else {
                    Some(s.to_lowercase())
                }
            })
            .collect();

        // Step 3: Fetch candidate details and compute Ebbinghaus cognitive scores
        let mut candidates = Vec::new();
        for id in &candidate_ids {
            let entry: Option<RoleMemoryEntry> = conn
                .query_row(
                    "SELECT id, role, session_id, user_prompt, role_response, created_at_s, last_accessed_at_s, access_count, strength, importance
                     FROM role_memories WHERE id = ?1",
                    params![id],
                    |row| {
                        Ok(RoleMemoryEntry {
                            id: row.get(0)?,
                            role: row.get(1)?,
                            session_id: row.get(2)?,
                            user_prompt: row.get(3)?,
                            role_response: row.get(4)?,
                            created_at_s: row.get(5)?,
                            last_accessed_at_s: row.get(6)?,
                            access_count: row.get(7)?,
                            strength: row.get(8)?,
                            importance: row.get(9)?,
                        })
                    },
                )
                .optional()?;

            if let Some(entry) = entry {
                let elapsed_s = (now_s - entry.last_accessed_at_s).max(0);
                let retention =
                    calculate_retention(elapsed_s, entry.strength, DEFAULT_BASE_TAU_DAYS);

                // Base relevance: from FTS if present, or term overlap count
                let base_rel = *fts_scores.get(&entry.id).unwrap_or(&1.0);
                let text = format!("{} {}", entry.user_prompt, entry.role_response).to_lowercase();
                let term_matches = terms
                    .iter()
                    .filter(|t| text.contains(&t.to_lowercase()))
                    .count() as f64;
                let term_boost = 1.0 + (term_matches * 0.5);

                let relevance = base_rel * term_boost;

                // Cognitive score: relevance * (0.2 baseline + 0.8 * retention) * importance
                let score = relevance * (0.2 + 0.8 * retention) * entry.importance;

                let memory_type = if elapsed_s <= WORKING_MEMORY_WINDOW_SECS {
                    "Working Memory".to_string()
                } else {
                    format!("Long-term Memory (Retained {:.0}%)", retention * 100.0)
                };

                let recency_label = format_recency(now_s - entry.created_at_s);

                candidates.push(RecalledMemory {
                    id: entry.id,
                    role: entry.role,
                    session_id: entry.session_id,
                    user_prompt: entry.user_prompt,
                    role_response: entry.role_response,
                    created_at_s: entry.created_at_s,
                    last_accessed_at_s: entry.last_accessed_at_s,
                    access_count: entry.access_count,
                    strength: entry.strength,
                    retention,
                    score,
                    memory_type,
                    recency_label,
                });
            }
        }

        // Sort descending by cognitive score
        candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
        candidates.truncate(limit);

        // Step 4: Spaced repetition reinforcement
        // Recalling a memory updates its access time and boosts its strength S,
        // making it more resistant to future forgetting.
        for item in &candidates {
            let _ = conn.execute(
                "UPDATE role_memories
                 SET access_count = access_count + 1,
                     last_accessed_at_s = ?1,
                     strength = strength + 1.0
                 WHERE id = ?2",
                params![now_s, item.id],
            );
        }

        Ok(candidates)
    }

    /// Prune low-retention or excess memories to keep database bounded and maintainable.
    pub fn prune_role_memories(&self, role: &str, max_capacity: usize) -> rusqlite::Result<usize> {
        let role = role.trim().to_lowercase();
        let now_s = current_time_s();

        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        let mut stmt = conn.prepare(
            "SELECT id, last_accessed_at_s, strength, access_count, importance
             FROM role_memories WHERE role = ?1",
        )?;

        let rows = stmt.query_map(params![role], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, u32>(3)?,
                row.get::<_, f64>(4)?,
            ))
        })?;

        let mut entries = Vec::new();
        for row in rows.flatten() {
            let (id, last_accessed, strength, access_count, importance) = row;
            let elapsed_s = (now_s - last_accessed).max(0);
            let retention = calculate_retention(elapsed_s, strength, DEFAULT_BASE_TAU_DAYS);
            entries.push((id, retention, access_count, importance));
        }

        let mut to_delete = Vec::new();

        // 1. Mark forgotten items: low retention (< FORGETTING_THRESHOLD) with no reinforcement
        for (id, retention, access_count, _) in &entries {
            if *retention < FORGETTING_THRESHOLD && *access_count <= 1 {
                to_delete.push(id.clone());
            }
        }

        // 2. Capacity eviction if total active exceeds max_capacity
        let remaining_count = entries.len().saturating_sub(to_delete.len());
        if remaining_count > max_capacity {
            let excess = remaining_count - max_capacity;
            let mut remaining: Vec<_> = entries
                .into_iter()
                .filter(|(id, _, _, _)| !to_delete.contains(id))
                .collect();
            // Sort by retention * importance ascending (lowest retention evicted first)
            remaining.sort_by(|a, b| (a.1 * a.3).total_cmp(&(b.1 * b.3)));
            for (id, _, _, _) in remaining.into_iter().take(excess) {
                to_delete.push(id);
            }
        }

        let pruned_count = to_delete.len();
        if !to_delete.is_empty() {
            for id in &to_delete {
                let _ = conn.execute("DELETE FROM role_memories WHERE id = ?1", params![id]);
            }
        }

        Ok(pruned_count)
    }

    fn prune_if_needed(&self, role: &str, max_capacity: usize) -> rusqlite::Result<()> {
        let count = self.count_memories(role)?;
        if count > max_capacity {
            let _ = self.prune_role_memories(role, max_capacity);
        }
        Ok(())
    }

    /// Count total dialogue memories for a role.
    pub fn count_memories(&self, role: &str) -> rusqlite::Result<usize> {
        let role = role.trim().to_lowercase();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM role_memories WHERE role = ?1",
            params![role],
            |r| r.get(0),
        )?;
        Ok(count as usize)
    }

    /// Clear all dialogue memories for a role.
    pub fn clear_role_memories(&self, role: &str) -> rusqlite::Result<usize> {
        let role = role.trim().to_lowercase();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let count = conn.execute("DELETE FROM role_memories WHERE role = ?1", params![role])?;
        Ok(count)
    }
}

/// Sanitize search query for SQLite FTS5 MATCH expression.
fn sanitize_fts_query(query: &str) -> String {
    let tokens: Vec<String> = query
        .split_whitespace()
        .filter_map(|word| {
            let cleaned: String = word
                .chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
                .collect();
            if cleaned.is_empty() {
                None
            } else {
                Some(format!("\"{cleaned}\""))
            }
        })
        .collect();

    if tokens.is_empty() {
        String::new()
    } else {
        tokens.join(" OR ")
    }
}

static GLOBAL_ROLE_MEMORY: OnceLock<RoleMemoryStore> = OnceLock::new();

/// Get or initialize the global shared [`RoleMemoryStore`].
pub fn get_role_memory_store() -> Result<RoleMemoryStore, String> {
    if let Some(store) = GLOBAL_ROLE_MEMORY.get() {
        return Ok(store.clone());
    }
    let store = RoleMemoryStore::open_default()
        .map_err(|e| format!("failed to open role memory store: {e}"))?;
    let _ = GLOBAL_ROLE_MEMORY.set(store.clone());
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ebbinghaus_working_memory_and_decay() {
        // Within working memory window (e.g. 1 hour = 3600s or 12 hours = 43200s), retention is 1.0
        let r_working = calculate_retention(3600, 1.0, DEFAULT_BASE_TAU_DAYS);
        assert!((r_working - 1.0).abs() < f64::EPSILON);
        let r_12h = calculate_retention(WORKING_MEMORY_WINDOW_SECS, 1.0, DEFAULT_BASE_TAU_DAYS);
        assert!((r_12h - 1.0).abs() < f64::EPSILON);

        // Continuous decay: 7 days after the 12h working window with S=1.0, retention is exp(-7 / 7) = exp(-1) ~ 0.368
        let r_7d = calculate_retention(WORKING_MEMORY_WINDOW_SECS + 7 * 86400, 1.0, 7.0);
        assert!((r_7d - (-1.0f64).exp()).abs() < 0.01);

        // After 7 decay days with reinforced S=2.0, retention is exp(-7 / (2*7)) = exp(-0.5) ~ 0.606
        let r_7d_reinforced = calculate_retention(WORKING_MEMORY_WINDOW_SECS + 7 * 86400, 2.0, 7.0);
        assert!((r_7d_reinforced - (-0.5f64).exp()).abs() < 0.01);
        assert!(r_7d_reinforced > r_7d);
    }

    #[test]
    fn salience_importance_boost() {
        let store = RoleMemoryStore::open_in_memory().unwrap();
        let normal = store
            .record_dialogue("philosophist", None, "What is time?", "Time is flux.")
            .unwrap()
            .unwrap();
        assert_eq!(normal.importance, 1.0);

        let salient = store
            .record_dialogue(
                "philosophist",
                None,
                "Please remember this core principle: virtue is knowledge.",
                "Socrates identified virtue with wisdom.",
            )
            .unwrap()
            .unwrap();
        assert_eq!(salient.importance, 1.5);
    }

    #[test]
    fn records_and_recalls_role_memory() {
        let store = RoleMemoryStore::open_in_memory().unwrap();

        let entry = store
            .record_dialogue(
                "philosophist",
                Some("sess-1"),
                "What is Camus's view of the Absurd?",
                "Camus defines the Absurd as the collision between humanity's search for meaning and the cold indifference of the universe.",
            )
            .unwrap();
        assert!(entry.is_some());

        // Unrelated role should have 0 memories
        assert_eq!(store.count_memories("developer").unwrap(), 0);
        assert_eq!(store.count_memories("philosophist").unwrap(), 1);

        // Recall with related query
        let hits = store
            .recall("philosophist", "Camus Absurd meaning", 5)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].user_prompt, "What is Camus's view of the Absurd?");
        assert_eq!(hits[0].memory_type, "Working Memory");
        assert_eq!(hits[0].recency_label, "just now");

        // After recall, access_count and strength increased
        assert_eq!(hits[0].access_count, 1); // was 1 at recall, updated to 2 in db
        let hits_again = store.recall("philosophist", "Camus", 5).unwrap();
        assert_eq!(hits_again[0].access_count, 2);
        assert!(hits_again[0].strength >= 2.0);
    }

    #[test]
    fn isolates_dialogue_by_role() {
        let store = RoleMemoryStore::open_in_memory().unwrap();

        store
            .record_dialogue(
                "philosophist",
                None,
                "Is determinism true?",
                "We examined soft vs hard determinism.",
            )
            .unwrap();

        store
            .record_dialogue(
                "developer",
                None,
                "How to compile rust code?",
                "Run cargo build --release.",
            )
            .unwrap();

        let phil_hits = store.recall("philosophist", "determinism", 5).unwrap();
        assert_eq!(phil_hits.len(), 1);
        assert_eq!(phil_hits[0].role, "philosophist");

        let dev_hits = store.recall("developer", "determinism", 5).unwrap();
        assert!(dev_hits.is_empty());

        let dev_code_hits = store.recall("developer", "cargo build", 5).unwrap();
        assert_eq!(dev_code_hits.len(), 1);
        assert_eq!(dev_code_hits[0].role, "developer");
    }

    #[test]
    fn cjk_fallback_and_recalls() {
        let store = RoleMemoryStore::open_in_memory().unwrap();

        store
            .record_dialogue(
                "philosophist",
                None,
                "你如何看待存在主义中的自由与美？",
                "存在先于本质，人是被判定为自由的，必须为自己的选择承担全部责任。",
            )
            .unwrap();

        let hits = store.recall("philosophist", "存在主义 自由", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].role_response.contains("存在先于本质"));

        // Single character CJK search should also match cleanly
        let hits_single = store.recall("philosophist", "美", 5).unwrap();
        assert_eq!(hits_single.len(), 1);
    }

    #[test]
    fn capacity_bounding_and_pruning() {
        let store = RoleMemoryStore::open_in_memory().unwrap();

        for i in 0..10 {
            store
                .record_dialogue(
                    "philosophist",
                    None,
                    &format!("Question {i}"),
                    &format!("Answer {i}"),
                )
                .unwrap();
        }
        assert_eq!(store.count_memories("philosophist").unwrap(), 10);

        // Prune down to capacity of 5
        let pruned = store.prune_role_memories("philosophist", 5).unwrap();
        assert_eq!(pruned, 5);
        assert_eq!(store.count_memories("philosophist").unwrap(), 5);
    }
}
