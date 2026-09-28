-- Plan 3b: what the pipeline needs to resume a task after a restart.
-- Set once the retry ladder has moved the implement stage up a tier.
ALTER TABLE tasks ADD COLUMN escalated INTEGER NOT NULL DEFAULT 0;

-- Structured stage answers (plan, review) and Provefab's own texts
-- (the NeedsInfo question), newest last (spec §3.2).
CREATE TABLE stage_outputs (
    id      INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks (id),
    kind    TEXT    NOT NULL,
    json    TEXT    NOT NULL,
    at      INTEGER NOT NULL
);

CREATE INDEX stage_outputs_task ON stage_outputs (task_id, kind);
