-- Repository rules (docs/specs/2026-10-01-repo-rules-design.md sections 5 and 7).
-- The rule a review finding cites (`R3`); NULL when it cites none.
ALTER TABLE findings ADD COLUMN rule TEXT;

-- Periodic maintenance (section 7): the scheduler's daily call per repository
-- (kind `periodic`) and the runs a policy records (Provefab Pro: `rules`).
-- `detail` is the policy's own JSON about the run; Provefab never shows it.
CREATE TABLE maintenance_runs (
    id          INTEGER PRIMARY KEY,
    repo        TEXT    NOT NULL,
    kind        TEXT    NOT NULL,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    model_id    TEXT,
    cost_usd    REAL,
    quota_units REAL,
    outcome     TEXT    NOT NULL,
    pr_url      TEXT,
    detail      TEXT
);
CREATE INDEX maintenance_runs_repo_kind ON maintenance_runs (repo, kind, started_at);
