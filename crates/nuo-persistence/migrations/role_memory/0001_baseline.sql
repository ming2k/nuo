-- Role dialogue memory baseline, schema version 1 (ADR-0192).
--
-- This is a second durable SQLite file, not part of nuo.db. It uses the same
-- baseline + ledger + structural-verification machinery as the main database,
-- so a schema change here is a numbered migration rather than ad-hoc
-- `CREATE TABLE IF NOT EXISTS` drift.

CREATE TABLE role_memories (
    id                 TEXT PRIMARY KEY,
    role               TEXT NOT NULL,
    session_id         TEXT,
    user_prompt        TEXT NOT NULL,
    role_response      TEXT NOT NULL,
    created_at_s       INTEGER NOT NULL CHECK (created_at_s >= 0),
    last_accessed_at_s INTEGER NOT NULL CHECK (last_accessed_at_s >= 0),
    access_count       INTEGER NOT NULL DEFAULT 1 CHECK (access_count >= 0),
    strength           REAL NOT NULL DEFAULT 1.0 CHECK (strength >= 0.0),
    importance         REAL NOT NULL DEFAULT 1.0 CHECK (importance >= 0.0)
) STRICT;

CREATE INDEX idx_role_memories_role_created
    ON role_memories(role, created_at_s DESC);
CREATE INDEX idx_role_memories_role_access
    ON role_memories(role, last_accessed_at_s DESC);

CREATE VIRTUAL TABLE fts_role_memories USING fts5(
    memory_id UNINDEXED,
    role UNINDEXED,
    content,
    tokenize = 'porter unicode61'
);

CREATE TRIGGER trg_role_memories_ai AFTER INSERT ON role_memories BEGIN
    INSERT INTO fts_role_memories(memory_id, role, content)
    VALUES (new.id, new.role, new.user_prompt || ' ' || new.role_response);
END;

CREATE TRIGGER trg_role_memories_au AFTER UPDATE OF user_prompt, role_response ON role_memories BEGIN
    DELETE FROM fts_role_memories WHERE memory_id = old.id;
    INSERT INTO fts_role_memories(memory_id, role, content)
    VALUES (new.id, new.role, new.user_prompt || ' ' || new.role_response);
END;

CREATE TRIGGER trg_role_memories_ad AFTER DELETE ON role_memories BEGIN
    DELETE FROM fts_role_memories WHERE memory_id = old.id;
END;
