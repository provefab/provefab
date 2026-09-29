-- Post-merge verification (docs/specs/2026-09-29-post-merge-verification-design.md, section 10).
-- One row per merged commit; `state` follows the section 5 state machine.
-- `merge_sha` is the literal 'unknown' when GitHub never reported it.
-- `pr_url` is the merged pull request: the task may open another one later.
CREATE TABLE post_merge_checks (
    id                INTEGER PRIMARY KEY,
    task_id           INTEGER NOT NULL REFERENCES tasks (id),
    merge_sha         TEXT NOT NULL,
    base              TEXT NOT NULL,
    pr_url            TEXT,
    commit_count      INTEGER,
    auto_merged       INTEGER NOT NULL DEFAULT 0,
    state             TEXT NOT NULL,
    failure_kind      TEXT,
    failure_summary   TEXT,
    failed_commands   TEXT,
    flaky             TEXT,
    base_sha          TEXT,
    revert_sha        TEXT,
    revert_branch     TEXT,
    revert_pr_url     TEXT,
    base_moves        INTEGER NOT NULL DEFAULT 0,
    infra_errors      INTEGER NOT NULL DEFAULT 0,
    started_at        INTEGER,
    finished_at       INTEGER,
    issue_notified_at INTEGER,
    pr_notified_at    INTEGER,
    UNIQUE (task_id, merge_sha)
);

CREATE INDEX post_merge_checks_task ON post_merge_checks (task_id);
CREATE INDEX post_merge_checks_state ON post_merge_checks (state);
