-- Migration 2: the thread list projection derives count/prompt from facts.
--
-- v1 stored `threads.msg_count` and `threads.last_user_prompt` as a fallback
-- for the list view. They are duplicate state: the fact log already owns every
-- message, and the view already prefers it. Keeping the columns let two
-- writers disagree about the same conversation. Drop them and let the view be
-- the only projection (ADR-0189).

DROP VIEW thread_list_view;

ALTER TABLE threads DROP COLUMN msg_count;
ALTER TABLE threads DROP COLUMN last_user_prompt;

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
    (SELECT COUNT(*) FROM causal_nodes cn
      WHERE cn.thread_id = t.id AND cn.kind IN ('dialogue','observation')) AS msg_count,
    (SELECT json_extract(cn.payload_json, '$.message.content')
       FROM causal_nodes cn
      WHERE cn.thread_id = t.id
        AND cn.kind = 'dialogue'
        AND lower(json_extract(cn.payload_json, '$.message.role')) = 'user'
      ORDER BY cn.seq DESC LIMIT 1) AS last_user_prompt,
    t.digest,
    t.status,
    t.round_counter
FROM threads t
LEFT JOIN thread_policies p ON p.thread_id = t.id;
