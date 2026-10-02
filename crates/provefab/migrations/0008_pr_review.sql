-- no-transaction
-- Reviewing human pull requests (docs/specs/2026-10-02-pr-review-design.md section 3).
-- `mode` is 'issue' or 'pr_review'; `pr_head` is the commit a pull request
-- review last reviewed. Uniqueness becomes (repo, mode, issue_number): pull
-- request #12 and ticket ENG-12 share the number 12.
-- SQLite cannot change a table's constraints in place, so `tasks` is rebuilt
-- with the procedure of https://www.sqlite.org/lang_altertable.html section 7,
-- keeping every id, so the eight tables that reference `tasks (id)` stay valid.
-- `PRAGMA foreign_keys` is a no-op inside a transaction: the first line makes
-- sqlx run this file outside its own, and the file opens and commits its own.
PRAGMA foreign_keys = OFF;
BEGIN IMMEDIATE;
CREATE TABLE tasks_new (
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
    escalated     INTEGER NOT NULL DEFAULT 0,
    issue_key     TEXT,
    mode          TEXT    NOT NULL DEFAULT 'issue',
    pr_head       TEXT,
    UNIQUE (repo, mode, issue_number)
);
INSERT INTO tasks_new (id, repo, issue_number, issue_url, title, author, state, kind,
                       attempts, review_rounds, branch, worktree, pr_url, pr_state,
                       reopen_count, created_at, updated_at, escalated, issue_key)
SELECT id, repo, issue_number, issue_url, title, author, state, kind,
       attempts, review_rounds, branch, worktree, pr_url, pr_state,
       reopen_count, created_at, updated_at, escalated, issue_key
FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_new RENAME TO tasks;
CREATE INDEX tasks_state ON tasks (state);
COMMIT;
PRAGMA foreign_keys = ON;
