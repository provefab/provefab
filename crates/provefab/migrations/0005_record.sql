-- Evidence and decision record (docs/specs/2026-09-30-evidence-record-design.md, section 3).
-- change_events is append-only: rows are only deleted by `provefab prune`.
CREATE TABLE change_events (
    id             INTEGER PRIMARY KEY,
    task_id        INTEGER NOT NULL REFERENCES tasks (id),
    seq            INTEGER NOT NULL,
    kind           TEXT    NOT NULL,
    source         TEXT    NOT NULL,
    schema_version INTEGER NOT NULL,
    payload        TEXT    NOT NULL,
    at             INTEGER NOT NULL,
    UNIQUE (task_id, seq)
);
CREATE INDEX change_events_kind ON change_events (task_id, kind);

CREATE TABLE findings (
    id             INTEGER PRIMARY KEY,
    task_id        INTEGER NOT NULL REFERENCES tasks (id),
    key            TEXT    NOT NULL,
    pass           INTEGER NOT NULL,
    round          INTEGER NOT NULL,
    reviewer_model TEXT    NOT NULL,
    severity       TEXT    NOT NULL,
    file           TEXT    NOT NULL,
    line           INTEGER,
    text           TEXT    NOT NULL,
    event_id       INTEGER NOT NULL REFERENCES change_events (id),
    UNIQUE (task_id, key)
);
