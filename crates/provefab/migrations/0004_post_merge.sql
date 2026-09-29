-- Post-merge verification is attached to a known Provefab task, not a new
-- primary task state. One check per merged commit makes scheduler retries safe.
CREATE TABLE post_merge_checks (
    id              INTEGER PRIMARY KEY,
    task_id         INTEGER NOT NULL REFERENCES tasks (id),
    merge_sha       TEXT NOT NULL,
    base            TEXT NOT NULL,
    state           TEXT NOT NULL,
    started_at      INTEGER,
    finished_at     INTEGER,
    worktree        TEXT,
    failure_summary TEXT,
    revert_pr_url   TEXT,
    notified_at     INTEGER,
    UNIQUE (task_id, merge_sha)
);

CREATE INDEX post_merge_checks_task ON post_merge_checks (task_id);
