-- The catalog plane needs a small, fact-free thread summary (ADR-0243).
-- ADR-0189 dropped these columns while facts and the list view shared one
-- database; with the catalog/content split, the list view can no longer join
-- `causal_nodes`, so the summary is materialised once per projection.
ALTER TABLE threads ADD COLUMN msg_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE threads ADD COLUMN last_user_prompt TEXT;

-- Backfill from the still-local fact log. Future rows are written by the
-- projection; this one-shot pass keeps an existing database's picker intact.
UPDATE threads
SET msg_count = (
        SELECT COUNT(*) FROM causal_nodes cn
        WHERE cn.thread_id = threads.id
          AND cn.kind IN ('dialogue', 'observation')
    ),
    last_user_prompt = (
        SELECT json_extract(cn.payload_json, '$.message.content')
        FROM causal_nodes cn
        WHERE cn.thread_id = threads.id
          AND cn.kind = 'dialogue'
          AND lower(json_extract(cn.payload_json, '$.message.role')) = 'user'
        ORDER BY cn.seq DESC
        LIMIT 1
    );

DROP VIEW thread_list_view;

CREATE VIEW thread_list_view AS
SELECT
    t.id,
    t.parent_thread_id AS parent_id,
    t.origin,
    t.title,
    t.created_at_ms / 1000 AS created_at_s,
    t.updated_at_ms / 1000 AS updated_at_s,
    p.workspace_root,
    p.system_role AS persona,
    t.msg_count,
    t.last_user_prompt,
    t.digest,
    t.status,
    t.round_counter
FROM threads t
LEFT JOIN thread_policies p ON p.thread_id = t.id;
