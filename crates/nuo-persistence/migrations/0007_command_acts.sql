-- ADR-0249: command acts are append-only journal frames projected into a
-- normalized table, retiring the monolithic `commands_json` column.

ALTER TABLE thread_policies DROP COLUMN commands_json;

CREATE TABLE command_acts (
    thread_id    TEXT NOT NULL,
    seq          INTEGER NOT NULL CHECK (seq >= 0),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    PRIMARY KEY (thread_id, seq)
) STRICT;

CREATE INDEX idx_command_acts_thread ON command_acts(thread_id, seq ASC);
