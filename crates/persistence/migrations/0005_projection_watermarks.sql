-- The derived view's per-session watermark: the highest journal seq it has
-- absorbed. A watermark behind the journal is repaired by replay (ADR-0242).
CREATE TABLE projection_watermarks (
    thread_id     TEXT PRIMARY KEY,
    seq           INTEGER NOT NULL CHECK (seq >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0)
) STRICT;
