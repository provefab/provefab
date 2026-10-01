-- Issue trackers (docs/specs/2026-10-01-issue-trackers-design.md section 4): the
-- ticket key (ENG-123) of a Jira or Linear issue; NULL for a GitHub issue.
-- issue_number and UNIQUE (repo, issue_number) are unchanged: one project per repo.
ALTER TABLE tasks ADD COLUMN issue_key TEXT;
