-- Spec §3.5. Times are unix seconds.
CREATE TABLE tasks (
    id            INTEGER PRIMARY KEY,
    repo          TEXT    NOT NULL,
    issue_number  INTEGER NOT NULL,
    issue_url     TEXT    NOT NULL,
    title         TEXT    NOT NULL,
    author        TEXT    NOT NULL,
    state         TEXT    NOT NULL,
    kind          TEXT,
    attempts      INTEGER NOT NULL DEFAULT 0,
    review_rounds INTEGER NOT NULL DEFAULT 0,
    branch        TEXT,
    worktree      TEXT,
    pr_url        TEXT,
    pr_state      TEXT,
    reopen_count  INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    UNIQUE (repo, issue_number)
);

CREATE TABLE transitions (
    id         INTEGER PRIMARY KEY,
    task_id    INTEGER NOT NULL REFERENCES tasks (id),
    from_state TEXT,
    to_state   TEXT    NOT NULL,
    reason     TEXT    NOT NULL,
    at         INTEGER NOT NULL
);

CREATE TABLE routing_decisions (
    id           INTEGER PRIMARY KEY,
    task_id      INTEGER NOT NULL REFERENCES tasks (id),
    jev_model    TEXT,
    verdict_json TEXT,
    tiers_json   TEXT    NOT NULL,
    reasons      TEXT    NOT NULL,
    at           INTEGER NOT NULL
);

CREATE TABLE stage_runs (
    id            INTEGER PRIMARY KEY,
    task_id       INTEGER NOT NULL REFERENCES tasks (id),
    stage         TEXT    NOT NULL,
    model_id      TEXT    NOT NULL,
    exit          TEXT    NOT NULL,
    turns         INTEGER NOT NULL,
    input_tokens  INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    session_dir   TEXT    NOT NULL,
    gate_score    TEXT,
    started_at    INTEGER NOT NULL,
    finished_at   INTEGER NOT NULL
);

CREATE TABLE replies_seen (
    task_id         INTEGER PRIMARY KEY REFERENCES tasks (id),
    last_comment_at TEXT    NOT NULL
);

CREATE INDEX tasks_state ON tasks (state);
CREATE INDEX transitions_task ON transitions (task_id);
CREATE INDEX stage_runs_task ON stage_runs (task_id);
