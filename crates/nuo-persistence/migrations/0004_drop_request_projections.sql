-- Request projections are forensic evidence, not session state (ADR-0240).
-- They moved to a best-effort per-session ring beside the journal; the SQLite
-- table, its foreign key, and its per-turn insert are retired.
DROP TABLE IF EXISTS request_projections;
